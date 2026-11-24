//! Read-only OpenTherm telemetry from nlclient's event log and settings.
//! The diagnostic timestamp is the time of measurement, not of log flush.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::json::{num, Fields};

const POLL: Duration = Duration::from_secs(5);
// Diagnostics arrive about once a minute but can be buffered for several
// minutes. This is a freshness policy, not a guarantee of update frequency.
const MAX_AGE: i64 = 10 * 60;
const FUTURE_TOLERANCE: i64 = 30;

pub const NUMBERS: [(&str, &str, &str, &str); 7] = [
    ("opentherm_max_supply_temperature", "Nest CH temperature limit", "°C", "temperature"),
    ("opentherm_requested_flow_temperature", "Requested CH flow temperature", "°C", "temperature"),
    ("opentherm_flow_temperature", "Boiler flow temperature", "°C", "temperature"),
    ("opentherm_return_temperature", "CH return temperature", "°C", "temperature"),
    ("opentherm_dhw_temperature", "DHW temperature", "°C", "temperature"),
    ("opentherm_modulation", "Boiler modulation", "%", ""),
    ("opentherm_water_pressure", "CH water pressure", "bar", "pressure"),
];
pub const FLAGS: [(&str, &str, &str); 4] = [
    ("opentherm_ch_active", "Central heating active", "running"),
    ("opentherm_dhw_active", "DHW active", "running"),
    ("opentherm_source_on", "Burner active", "running"),
    ("opentherm_fault", "Boiler fault", "problem"),
];
pub const CODES: [(&str, &str); 2] = [
    ("opentherm_oem_fault_code", "Boiler OEM fault code"),
    ("opentherm_diagnostic_code", "Boiler diagnostic code"),
];
pub const TIMES: [(&str, &str); 2] = [
    ("opentherm_diagnostics_timestamp", "Boiler diagnostics last measured"),
    ("opentherm_control_timestamp", "CH target last calculated"),
];

// Only the Gen 3 platform and its event schema have been validated.
pub fn supported_platform(name: &str) -> bool { name == "diamond3" }

pub const GRAPH_NUMBERS: [(&str, &str); 4] = [
    ("opentherm_ch_flow_temperature", "CH flow temperature"),
    ("opentherm_dhw_primary_flow_temperature", "DHW primary flow temperature"),
    ("opentherm_idle_flow_temperature", "Idle flow temperature"),
    ("opentherm_combined_flow_temperature", "Combined CH and DHW flow temperature"),
];

fn graph_index(values: &Fields) -> Option<usize> {
    match (values.get("opentherm_ch_active").and_then(Value::as_bool),
           values.get("opentherm_dhw_active").and_then(Value::as_bool)) {
        (Some(true), Some(false)) => Some(0),
        (Some(false), Some(true)) => Some(1),
        (Some(false), Some(false)) => Some(2),
        (Some(true), Some(true)) => Some(3),
        _ => None,
    }
}

fn graph_values(values: &mut Fields) {
    let selected = graph_index(values);
    let flow = values.get("opentherm_flow_temperature").cloned().unwrap_or(Value::Null);
    for (i, (key, _)) in GRAPH_NUMBERS.iter().enumerate() {
        values.insert((*key).into(), if selected == Some(i) { flow.clone() } else { Value::Null });
    }
}

pub fn enabled(dev: &Fields) -> bool {
    dev.get("heat_link_heat_type").and_then(Value::as_str) == Some("opentherm")
}

pub fn dhw_temperature_supported(dev: &Fields) -> bool {
    enabled(dev) && dev.get("has_hot_water_temperature").and_then(Value::as_bool) == Some(true)
        && dev.get("boiler_type").and_then(Value::as_str) == Some("combi")
}

fn empty_state() -> Fields {
    NUMBERS.iter().map(|(k, ..)| *k)
        .chain(GRAPH_NUMBERS.iter().map(|(k, ..)| *k))
        .chain(FLAGS.iter().map(|(k, ..)| *k))
        .chain(CODES.iter().map(|(k, ..)| *k))
        .chain(TIMES.iter().map(|(k, ..)| *k))
        .map(|k| (k.into(), Value::Null)).collect()
}

#[derive(Default)]
pub struct Watch {
    shown: Mutex<Fields>,
    registers: Mutex<Fields>,
    combined_seen: AtomicBool,
    dhw_seen: AtomicBool,
}

impl Watch {
    pub fn shown(&self) -> Fields {
        let mut fields = empty_state();
        fields.extend(self.shown.lock().unwrap().clone());
        fields.extend(self.register_fields());
        fields
    }

    pub(crate) fn combined_seen(&self) -> bool { self.combined_seen.load(Ordering::Relaxed) }
    pub(crate) fn dhw_seen(&self) -> bool { self.dhw_seen.load(Ordering::Relaxed) }

    pub(crate) fn register_fields(&self) -> Fields {
        self.registers.lock().unwrap().clone()
    }

    pub(crate) fn update_registers(&self, next: Fields) -> bool {
        let mut old = self.registers.lock().unwrap();
        if *old == next { return false; }
        *old = next;
        true
    }

    fn update(&self, next: Fields) -> bool {
        let mut shown = self.shown.lock().unwrap();
        if *shown == next {
            return false;
        }
        *shown = next;
        true
    }
}

#[derive(Debug, PartialEq)]
struct Sample {
    timestamp: i64,
    values: Fields,
}

#[derive(Default)]
struct Samples {
    diagnostics: Option<Sample>,
    control: Option<Sample>,
    combined_seen: bool,
    dhw_seen: bool,
}

fn decimal(s: &str) -> Value {
    s.parse::<f64>().ok().filter(|n| n.is_finite()).map_or(Value::Null, |n| num((n * 100.0).round() / 100.0))
}

fn flag(s: &str) -> Value {
    match s {
        "0" => false.into(),
        "1" => true.into(),
        _ => Value::Null,
    }
}

fn code(s: &str) -> Value {
    s.parse::<u32>().ok().map_or(Value::Null, Value::from)
}

fn timestamp(t: i64) -> Value {
    chrono::DateTime::from_timestamp(t, 0)
        .map_or(Value::Null, |d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true).into())
}

fn fresh(t: i64, now: i64) -> bool {
    (-FUTURE_TOLERANCE..=MAX_AGE).contains(&now.saturating_sub(t))
}

fn parse(line: &str) -> Option<(bool, Sample)> {
    let mut words = line.split_whitespace();
    let logged = words.next()?;
    let event = words.next()?;
    match event {
        "RemoteModulatingBoilerDiag" => {
            let f: Vec<_> = words.collect();
            // The 6.4-8 schema has 23 fields, including Timestamp. Do not
            // silently shift positions if a different firmware adds fields.
            if f.len() != 23 { return None; }
            let t = f[0].parse::<i64>().ok()?;
            let mut values = Fields::new();
            for (key, i) in [("opentherm_fault", 1), ("opentherm_ch_active", 2),
                             ("opentherm_dhw_active", 3), ("opentherm_source_on", 4)] {
                values.insert(key.into(), flag(f[i]));
            }
            for (key, i) in [("opentherm_modulation", 15), ("opentherm_water_pressure", 16),
                             ("opentherm_flow_temperature", 17), ("opentherm_dhw_temperature", 18),
                             ("opentherm_return_temperature", 20)] {
                values.insert(key.into(), decimal(f[i]));
            }
            values.insert("opentherm_oem_fault_code".into(), code(f[14]));
            values.insert("opentherm_diagnostic_code".into(), code(f[21]));
            graph_values(&mut values);
            Some((true, Sample { timestamp: t, values }))
        }
        "RethinkStateResults" => {
            let f: Vec<_> = words.collect();
            if f.len() != 19 { return None; }
            let t = chrono::NaiveDateTime::parse_from_str(logged, "%Y-%m-%dT%H:%M:%S").ok()?.and_utc().timestamp();
            let mut values = Fields::new();
            values.insert("opentherm_requested_flow_temperature".into(), decimal(f[6]));
            Some((false, Sample { timestamp: t, values }))
        }
        _ => None,
    }
}

impl Samples {
    fn line(&mut self, line: &str, now: i64) {
        let Some((diag, sample)) = parse(line) else { return; };
        // Ignore records implausibly far in the future, including after a
        // clock adjustment. Accept equal timestamps (control recalculations
        // can occur in the same second), in file order.
        if sample.timestamp > now.saturating_add(FUTURE_TOLERANCE) { return; }
        if diag {
            self.combined_seen |= graph_index(&sample.values) == Some(3);
            self.dhw_seen |= sample.values.get("opentherm_dhw_active").and_then(Value::as_bool) == Some(true);
        }
        let slot = if diag { &mut self.diagnostics } else { &mut self.control };
        if slot.as_ref().is_none_or(|old| sample.timestamp >= old.timestamp || old.timestamp > now.saturating_add(FUTURE_TOLERANCE)) {
            *slot = Some(sample);
        }
    }

    fn shown(&self, now: i64, maximum: Value) -> Fields {
        let mut state = empty_state();
        state.insert("opentherm_max_supply_temperature".into(), maximum);
        for (sample, key) in [(&self.diagnostics, TIMES[0].0), (&self.control, TIMES[1].0)] {
            if let Some(sample) = sample {
                state.insert(key.into(), timestamp(sample.timestamp));
                if fresh(sample.timestamp, now) {
                    state.extend(sample.values.clone());
                }
            }
        }
        state
    }
}

fn maximum(settings: &[u8]) -> Value {
    crate::nlsettings::value(settings, "supplytemp")
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0 && *v <= 100 * 65536)
        .map_or(Value::Null, |v| num(v as f64 / 65536.0))
}

struct Reader {
    inode: u64,
    dev: u64,
    reader: BufReader<File>,
}

impl Reader {
    fn open(path: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        let metadata = file.metadata().ok()?;
        Some(Self { inode: metadata.ino(), dev: metadata.dev(), reader: BufReader::new(file) })
    }

    fn current(&mut self, path: &Path) -> bool {
        let Ok(position) = self.reader.stream_position() else { return false; };
        std::fs::metadata(path).is_ok_and(|m| m.ino() == self.inode && m.dev() == self.dev && m.len() >= position)
    }

    fn read(&mut self, samples: &mut Samples, now: i64) -> std::io::Result<()> {
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = self.reader.read_until(b'\n', &mut line)?;
            if n == 0 { return Ok(()); }
            if !line.ends_with(b"\n") {
                self.reader.seek_relative(-(n as i64))?;
                return Ok(());
            }
            if let Ok(line) = std::str::from_utf8(&line) {
                samples.line(line, now);
            }
        }
    }
}

/// Runs only for MQTT. No wake locks, boiler commands or settings writes.
pub fn watch(state: Arc<Watch>, path: PathBuf, settings: PathBuf, changed: impl Fn() + Send + Sync + 'static) {
    let changed = Arc::new(changed);
    let notify = changed.clone();
    crate::ot_registers::watch(state.clone(), settings.clone(), move || notify());
    std::thread::spawn(move || {
        let mut reader: Option<Reader> = None;
        let mut samples = Samples::default();
        loop {
            let now = chrono::Utc::now().timestamp();
            let config = std::fs::read(&settings).unwrap_or_default();
            if crate::nlsettings::value(&config, "heatlinkheat") == Some(b"opentherm".as_slice()) {
                // Drain the old file before opening its replacement, unless
                // it was truncated. A buffered partial line is retried.
                if let Some(old) = reader.as_mut() {
                    if old.read(&mut samples, now).is_err() || !old.current(&path) {
                        reader = None;
                    }
                }
                if reader.is_none() {
                    reader = Reader::open(&path);
                    if let Some(new) = reader.as_mut() {
                        if new.read(&mut samples, now).is_err() { reader = None; }
                    }
                }
                let combined_new = samples.combined_seen && !state.combined_seen.swap(true, Ordering::Relaxed);
                let dhw_new = samples.dhw_seen && !state.dhw_seen.swap(true, Ordering::Relaxed);
                let updated = state.update(samples.shown(now, maximum(&config)));
                if updated || combined_new || dhw_new { changed(); }
            } else {
                reader = None;
                samples = Samples::default();
                let combined = state.combined_seen.swap(false, Ordering::Relaxed);
                let dhw = state.dhw_seen.swap(false, Ordering::Relaxed);
                if state.update(empty_state()) || combined || dhw { changed(); }
            }
            std::thread::sleep(POLL);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Captured field values, with synthetic timestamps (no device identifiers).
    const DIAG: &str = "2026-01-01T00:00:00 RemoteModulatingBoilerDiag 1767225600 0 1 0 1 0 0 0 0 0 0 0 0 0 0 43.000000 - 50.500000 50.500000 - - - 0";
    const IDLE: &str = "2026-01-01T00:00:00 RemoteModulatingBoilerDiag 1767225600 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.000000 - 65.399994 45.600006 - - - 0";
    const CONTROL: &str = "2026-01-01T00:00:00 RethinkStateResults 21.000 20.720 21.280 21.000 1000.000 -1.000 55.000 0.000 false false false 0 0 true true 0 21 \"CARBON\" 21.110";
    const T: i64 = 1767225600;

    #[test]
    fn opentherm_graph_categories_preserve_original_and_unknowns() {
        for (ch, dhw, expected) in [(Some(true), Some(false), Some(0)),
            (Some(false), Some(true), Some(1)), (Some(false), Some(false), Some(2)),
            (Some(true), Some(true), Some(3)), (None, Some(false), None),
            (Some(true), None, None), (None, Some(true), None), (None, None, None)] {
            let mut v = Fields::new();
            v.insert("opentherm_ch_active".into(), json!(ch));
            v.insert("opentherm_dhw_active".into(), json!(dhw));
            v.insert("opentherm_flow_temperature".into(), json!(54.5));
            graph_values(&mut v);
            assert_eq!(v["opentherm_flow_temperature"], json!(54.5));
            for (i, (key, _)) in GRAPH_NUMBERS.iter().enumerate() {
                assert_eq!(v[*key], if expected == Some(i) { json!(54.5) } else { Value::Null });
            }
            v.insert("opentherm_flow_temperature".into(), Value::Null);
            graph_values(&mut v);
            for (key, _) in GRAPH_NUMBERS { assert_eq!(v[key], Value::Null); }
        }
        let (_, s) = parse(DIAG).unwrap();
        assert_eq!(s.values[GRAPH_NUMBERS[0].0], json!(50.5));
        let (_, s) = parse(IDLE).unwrap();
        assert_eq!(s.values[GRAPH_NUMBERS[2].0], json!(65.4));
    }

    #[test]
    fn opentherm_supported_platforms() {
        assert!(supported_platform("diamond3"));
        for name in ["diamond", "j49", "unknown", "thermostat-e"] {
            assert!(!supported_platform(name));
        }
    }

    #[test]
    fn active_diagnostics_decode_and_nulls_stay_null() {
        let (_, s) = parse(DIAG).unwrap();
        assert_eq!(s.timestamp, T);
        assert_eq!(s.values["opentherm_ch_active"], json!(true));
        assert_eq!(s.values["opentherm_source_on"], json!(true));
        assert_eq!(s.values["opentherm_dhw_active"], json!(false));
        assert_eq!(s.values["opentherm_modulation"], json!(43));
        assert_eq!(s.values["opentherm_flow_temperature"], json!(50.5));
        assert_eq!(s.values["opentherm_dhw_temperature"], json!(50.5));
        assert_eq!(s.values["opentherm_return_temperature"], Value::Null);
        assert_eq!(s.values["opentherm_water_pressure"], Value::Null);
        assert_eq!(s.values["opentherm_oem_fault_code"], json!(0));
    }

    #[test]
    fn idle_and_requested_values_are_distinct() {
        let (_, s) = parse(IDLE).unwrap();
        assert_eq!(s.values["opentherm_modulation"], json!(0));
        assert_eq!(s.values["opentherm_source_on"], json!(false));
        assert_eq!(s.values["opentherm_flow_temperature"], json!(65.4));
        let (diag, s) = parse(CONTROL).unwrap();
        assert!(!diag);
        assert_eq!(s.timestamp, T);
        assert_eq!(s.values["opentherm_requested_flow_temperature"], json!(55));
        assert_eq!(maximum(br#"<s key="supplytemp" value="3604480"/>"#), json!(55));
        assert_eq!(maximum(b"missing"), Value::Null);
    }

    #[test]
    fn old_records_do_not_overwrite_new_ones_and_expire() {
        let mut samples = Samples::default();
        samples.line(DIAG, T);
        samples.line(CONTROL, T);
        samples.line(&IDLE.replace("1767225600", "1767225599"), T);
        assert_eq!(samples.shown(T, json!(55))["opentherm_modulation"], json!(43));
        let stale = samples.shown(T + MAX_AGE + 1, json!(55));
        assert_eq!(stale["opentherm_modulation"], Value::Null);
        assert_eq!(stale["opentherm_source_on"], Value::Null);
        assert_eq!(stale[GRAPH_NUMBERS[0].0], Value::Null);
        assert_eq!(stale["opentherm_requested_flow_temperature"], Value::Null);
        assert_eq!(stale["opentherm_max_supply_temperature"], json!(55));
        assert_eq!(stale["opentherm_diagnostics_timestamp"], json!("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn reader_handles_partial_lines_rotation_and_truncation() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("nestlocal-opentherm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nlevent");
        let (part, last) = DIAG.rsplit_once(' ').unwrap();
        std::fs::write(&path, part).unwrap();
        let mut reader = Reader::open(&path).unwrap();
        let mut samples = Samples::default();
        reader.read(&mut samples, T).unwrap();
        assert!(samples.diagnostics.is_none());
        writeln!(std::fs::OpenOptions::new().append(true).open(&path).unwrap(), " {last}").unwrap();
        reader.read(&mut samples, T).unwrap();
        assert_eq!(samples.shown(T, Value::Null)["opentherm_modulation"], json!(43));
        std::fs::rename(&path, dir.join("nlevent#1")).unwrap();
        std::fs::write(&path, format!("{CONTROL}\n")).unwrap();
        assert!(!reader.current(&path));
        let mut reader = Reader::open(&path).unwrap();
        reader.read(&mut samples, T).unwrap();
        assert_eq!(samples.shown(T, Value::Null)["opentherm_requested_flow_temperature"], json!(55));
        std::fs::write(&path, b"").unwrap();
        assert!(!reader.current(&path));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema_headers_and_truncated_records_are_ignored() {
        assert!(parse("2026-01-01T00:00:00 %RemoteModulatingBoilerDiag {}").is_none());
        assert!(parse(DIAG.rsplit_once(' ').unwrap().0).is_none());
        assert!(parse("2026-01-01T00:00:00 RethinkStateResults 21").is_none());
        assert_eq!(flag("-"), Value::Null);
        assert_eq!(decimal("NaN"), Value::Null);
    }
}
