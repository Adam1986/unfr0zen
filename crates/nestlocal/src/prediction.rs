//! Read-only candidate True Radiant start from the native schedule timer.
//! This timer also carries ordinary schedule deadlines and hourly fallback
//! checks: never treat an arbitrary timeout as a preheating prediction.
use crate::{json::{Fields, number, num}, schedule, store::Object};
use chrono::{Datelike, TimeZone};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const KEYS: [&str; 4] = ["next_scheduled_setpoint", "next_scheduled_temperature",
    "predicted_heating_start", "predicted_preheating_duration"];
const MAX_AGE_MS: i64 = 90_000;
const LOG_TAIL: u64 = 64 * 1024;
const MARKER: &str = "Smart Thermostat Manager: sent next temperature schedule timeout request for ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    logged_ms: i64,
    deadline: i64,
}

fn parse(line: &str) -> Option<Sample> {
    let (header, deadline) = line.split_once(MARKER)?;
    if !header.contains(" nlclient[") || !header.contains(":contro]") { return None; }
    let logged_ms = chrono::NaiveDateTime::parse_from_str(header.get(..26)?, "%Y-%m-%d %H:%M:%S%.f")
        .ok()?.and_utc().timestamp_millis();
    let deadline = deadline.trim().parse::<i64>().ok()?;
    Some(Sample { logged_ms, deadline })
}

/// Bounded read of the current native log, following its path across rotation.
/// Ignore an incomplete final record. No logging changes, writes or wake locks.
pub fn latest(path: &Path) -> Option<Sample> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(LOG_TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.take(LOG_TAIL).read_to_end(&mut buf).ok()?;
    let end = buf.iter().rposition(|b| *b == b'\n')?;
    let start = if start == 0 { 0 } else { buf.iter().position(|b| *b == b'\n')? + 1 };
    if start > end { return None; }
    buf[start..end].split(|b| *b == b'\n')
        .filter_map(|line| parse(std::str::from_utf8(line).ok()?))
        .max_by_key(|s| s.logged_ms)
}

fn timestamp(t: i64) -> Value {
    chrono::DateTime::from_timestamp(t, 0)
        .map_or(Value::Null, |t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string().into())
}

/// Weekly schedule uses the thermostat's local wall time. Skip continuations
/// and refuse ambiguous/nonexistent DST times instead of guessing.
fn next_heat<T: TimeZone>(bucket: &Fields, now: i64, zone: &T) -> Option<(i64, f64)> {
    if schedule::kind(bucket) != Some(schedule::Kind::Heat) { return None; }
    let local = chrono::DateTime::from_timestamp(now, 0)?.with_timezone(zone);
    let today = local.weekday().num_days_from_monday() as usize;
    let mut candidates = Vec::new();
    for ahead in 0..=7 {
        let day = (today + ahead) % 7;
        let Some(entries) = bucket.get("days").and_then(|d| d.get(day.to_string())).and_then(Value::as_object) else { continue; };
        for entry in entries.values().filter_map(Value::as_object) {
            if entry.get("entry_type").is_some_and(|v| v != "setpoint") { continue; }
            let Some(sec) = number(entry.get("time")).filter(|v| (0.0..86400.0).contains(v) && v.fract() == 0.0) else { continue; };
            let Some(temp) = number(entry.get("temp")).filter(|v| v.is_finite()) else { continue; };
            let date = local.date_naive().checked_add_days(chrono::Days::new(ahead as u64))?;
            let sec = sec as u32;
            let wall = date.and_hms_opt(sec / 3600, sec / 60 % 60, sec % 60)?;
            // An ambiguous upcoming change makes the prediction uncertain.
            if wall <= local.naive_local() { continue; }
            let at = zone.from_local_datetime(&wall).single()?.timestamp();
            if at > now { candidates.push((at, temp)); }
        }
        if !candidates.is_empty() { break; }
    }
    candidates.into_iter().min_by_key(|(at, _)| *at)
}

pub fn values<T: TimeZone>(dev: &Fields, state: &Fields, week: Option<&Object>,
    sample: Option<Sample>, now_ms: i64, zone: &T) -> Fields {
    let mut out: Fields = KEYS.iter().map(|k| (k.to_string(), Value::Null)).collect();
    let now = now_ms.div_euclid(1000);
    if state.get("mode").and_then(Value::as_str) != Some("heat")
        || state.get("schedule_enabled").and_then(Value::as_bool) != Some(true)
        || state.get("preset").and_then(Value::as_str) != Some("none") { return out; }
    let Some(week) = week else { return out; };
    let Some((next, temp)) = next_heat(&week.value, now, zone) else { return out; };
    out.insert(KEYS[0].into(), timestamp(next));
    out.insert(KEYS[1].into(), num((temp * 100.0).round() / 100.0));
    if dev.get("radiant_control_enabled").and_then(Value::as_bool) != Some(true)
        || dev.get("preconditioning_active").and_then(Value::as_bool) != Some(false)
        || dev.get("preconditioning_ready").and_then(Value::as_bool) != Some(true)
        || dev.get("eta_preconditioning_active").and_then(Value::as_bool) == Some(true)
        || !number(state.get("target")).is_some_and(|v| temp > v + 0.01)
        || !number(state.get("current_temperature")).is_some_and(|v| temp > v + 0.01) { return out; }
    let Some(s) = sample else { return out; };
    if !(0..=MAX_AGE_MS).contains(&(now_ms - s.logged_ms))
        || s.logged_ms < week.timestamp
        || s.deadline <= now || s.deadline >= next
        // When disabled the native controller schedules exactly now + 3600.
        // Exclude that fallback shape even immediately after re-enabling.
        || (s.deadline - s.logged_ms.div_euclid(1000) - 3600).abs() <= 1 {
        return out;
    }
    out.insert(KEYS[2].into(), timestamp(s.deadline));
    out.insert(KEYS[3].into(), num(((next - s.deadline) as f64 / 60.0 * 10.0).round() / 10.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn obj(v: Value) -> Fields { v.as_object().unwrap().clone() }

    #[test]
    fn true_radiant_native_timer_prediction_and_guards() {
        let logged = "2026-11-25 10:55:50.519352 09AA nlclient[559:contro] (2) Smart Thermostat Manager: sent next temperature schedule timeout request for 1795604400";
        let sample = parse(logged).unwrap();
        let now = sample.logged_ms + 5_000;
        let mut week = Object::default();
        week.value = obj(json!({"schedule_mode":"HEAT","days":{"2":{"0":{"time":0,"temp":18,"entry_type":"continuation"},"1":{"time":43200,"temp":21},"2":{"time":61200,"temp":21}}}}));
        let mut dev = obj(json!({"radiant_control_enabled":true,"preconditioning_active":false,"preconditioning_ready":true,"eta_preconditioning_active":false}));
        let mut state = obj(json!({"mode":"heat","schedule_enabled":true,"preset":"none","target":18.317108,"current_temperature":19.5}));
        let read = |d: &Fields, st: &Fields, w: &Object, s: Option<Sample>, at| values(d, st, Some(w), s, at, &chrono::Utc);
        let v = read(&dev, &state, &week, Some(sample), now);
        assert_eq!(v[KEYS[0]], json!("2026-11-25T12:00:00Z"));
        assert_eq!(v[KEYS[1]], json!(21.0));
        assert_eq!(v[KEYS[2]], json!("2026-11-25T11:00:00Z"));
        assert_eq!(v[KEYS[3]], json!(60.0));
        for s in [None, Some(Sample{deadline: sample.logged_ms/1000+3600,..sample}), Some(Sample{deadline:1795608000,..sample})] {
            assert!(read(&dev, &state, &week, s, now)[KEYS[2]].is_null());
        }
        assert!(read(&dev, &state, &week, Some(sample), now+90_001)[KEYS[2]].is_null());
        assert!(read(&dev, &state, &week, Some(sample), 1795604400000)[KEYS[2]].is_null());
        week.timestamp = sample.logged_ms + 1;
        assert!(read(&dev, &state, &week, Some(sample), now)[KEYS[2]].is_null());
        week.timestamp = 0;
        dev.insert("preconditioning_active".into(), true.into());
        assert!(read(&dev, &state, &week, Some(sample), now)[KEYS[2]].is_null());
        dev.insert("preconditioning_active".into(), false.into());
        dev.insert("radiant_control_enabled".into(), false.into());
        assert!(read(&dev, &state, &week, Some(sample), now)[KEYS[2]].is_null());
        dev.insert("radiant_control_enabled".into(), true.into());
        state.insert("schedule_enabled".into(), false.into());
        assert!(read(&dev, &state, &week, Some(sample), now).values().all(Value::is_null));
        state.insert("schedule_enabled".into(), true.into());
        state.insert("preset".into(), "eco".into());
        assert!(read(&dev, &state, &week, Some(sample), now).values().all(Value::is_null));
        assert!(parse("2026-11-25 10:55:50.519352 other: sent next temperature schedule timeout request for 1795604400").is_none());
    }
}
