//! Optional, read-only capture of the cached Heat Link register table on
//! the verified 6.4-8 Diamond3 nlclient build. No ptrace or boiler writes.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};
use crate::json::{num, Fields};
use crate::opentherm::Watch;

const DIR: &str = "/media/scratch/opentherm-registers";
const EXE: &str = "/nestlabs/sbin/nlclient";
const HASH: &str = "f3c3ec6b440661d64077d76010c7cc0f477c14ab8485957e9444aa3ade32bd21";
const GLOBAL: u64 = 0x82cfe8;
const VTABLE: u64 = 0x6c4364;
const SIZE: usize = 1032;
const MAX_AGE: i64 = 3 * 3600;
const TIME: &str = "opentherm_register_timestamp";

// Stable entity keys; announce only acknowledged fields, not 128 registers.
// No duplicates of the faster event-based temperature/status measurements.
pub const ENTITIES: &[(&str, &str, &str, &str)] = &[
    ("opentherm_boiler_dhw_setpoint", "Boiler DHW setpoint", "°C", "temperature"),
    ("opentherm_boiler_ch_limit", "Boiler CH temperature limit", "°C", "temperature"),
    ("opentherm_dhw_min", "Boiler DHW minimum setpoint", "°C", "temperature"),
    ("opentherm_dhw_max", "Boiler DHW maximum setpoint", "°C", "temperature"),
    ("opentherm_ch_min", "Boiler CH minimum limit", "°C", "temperature"),
    ("opentherm_ch_max", "Boiler CH maximum limit", "°C", "temperature"),
    ("opentherm_boiler_product", "Boiler product type / version", "", ""),
    ("opentherm_boiler_member_id", "Boiler member ID", "", ""),
    ("opentherm_dhw_access", "Boiler DHW remote access", "", ""),
    ("opentherm_ch_limit_access", "Boiler CH limit remote access", "", ""),
    ("opentherm_boiler_capacity", "Boiler maximum capacity", "kW", "power"),
    ("opentherm_min_modulation", "Boiler minimum modulation", "%", ""),
    ("opentherm_dhw_flow_rate", "DHW flow rate (cached)", "L/min", "volume_flow_rate"),
    ("opentherm_flue_temperature", "Flue gas temperature (cached)", "°C", "temperature"),
    ("opentherm_burner_starts", "Successful burner starts", "", ""),
    ("opentherm_ch_pump_starts", "CH pump starts", "", ""),
    ("opentherm_dhw_pump_starts", "DHW pump / valve starts", "", ""),
    ("opentherm_dhw_burner_starts", "DHW burner starts", "", ""),
    ("opentherm_burner_hours", "Boiler burner hours", "h", "duration"),
    ("opentherm_ch_pump_hours", "CH pump hours", "h", "duration"),
    ("opentherm_dhw_pump_hours", "DHW pump / valve hours", "h", "duration"),
    ("opentherm_dhw_burner_hours", "DHW burner hours", "h", "duration"),
    ("opentherm_boiler_protocol", "Boiler OpenTherm version", "", ""),
    (TIME, "OpenTherm register table last captured", "", "timestamp"),
];

fn word(data: &[u8], id: usize) -> Option<u16> {
    let r = &data[8+id*8..16+id*8];
    // Read acknowledgements only. A write echo is not proof of a measured value.
    (r[4] == 4).then(|| u16::from_le_bytes([r[0], r[1]]))
}
fn fixed(v: u16) -> Value { num((v as i16 as f64 / 256.0 * 100.0).round() / 100.0) }
fn decode(data: &[u8]) -> Option<Fields> {
    if data.len() != SIZE || data[..8] != [3,0,0,0,0,0,0,0] ||
        !data[8..].chunks_exact(8).enumerate().all(|(i,r)| r[2] as usize == i && r[4] <= 7) {
        return None;
    }
    let mut out = Fields::new();
    for (id,key) in [(56,"opentherm_boiler_dhw_setpoint"),(57,"opentherm_boiler_ch_limit"),
        (19,"opentherm_dhw_flow_rate"),(125,"opentherm_boiler_protocol")] {
        if let Some(v) = word(data,id) { out.insert(key.into(),fixed(v)); }
    }
    for (id,lo,hi) in [(48,"opentherm_dhw_min","opentherm_dhw_max"),
        (49,"opentherm_ch_min","opentherm_ch_max")] {
        if let Some(v) = word(data,id) {
            let [min,max] = v.to_le_bytes();
            if (min as i8) <= (max as i8) {
                out.insert(lo.into(), Value::from(min as i8));
                out.insert(hi.into(), Value::from(max as i8));
            }
        }
    }
    if let Some(v) = word(data,3) { out.insert("opentherm_boiler_member_id".into(),Value::from(v as u8)); }
    if let Some(v) = word(data,127) {
        let [version,kind] = v.to_le_bytes();
        out.insert("opentherm_boiler_product".into(),format!("{kind} / {version}").into());
    }
    if let Some(v) = word(data,6) {
        let [write,read] = v.to_le_bytes();
        for (mask,key) in [(1,"opentherm_dhw_access"),(2,"opentherm_ch_limit_access")] {
            let value = match (read & mask != 0,write & mask != 0) {
                (true,true) => "Read/write", (true,false) => "Read only",
                (false,_) => "Transfer disabled",
            };
            out.insert(key.into(),value.into());
        }
    }
    if let Some(v) = word(data,15) {
        let [min,capacity] = v.to_le_bytes();
        if min <= 100 { out.insert("opentherm_min_modulation".into(),Value::from(min)); }
        out.insert("opentherm_boiler_capacity".into(),Value::from(capacity));
    }
    if let Some(v) = word(data,33) { out.insert("opentherm_flue_temperature".into(),Value::from(v as i16)); }
    for (i,key) in ["opentherm_burner_starts","opentherm_ch_pump_starts",
        "opentherm_dhw_pump_starts","opentherm_dhw_burner_starts",
        "opentherm_burner_hours","opentherm_ch_pump_hours",
        "opentherm_dhw_pump_hours","opentherm_dhw_burner_hours"].iter().enumerate() {
        if let Some(v) = word(data,116+i) { out.insert((*key).into(),Value::from(v)); }
    }
    Some(out)
}

fn allowed(dir: &Path) -> bool {
    fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir() && m.uid() == 0 && m.mode() & 0o777 == 0o700)
}
fn readable(maps: &str, addr: u64, size: usize) -> bool {
    let Some(end) = addr.checked_add(size as u64) else { return false; };
    maps.lines().any(|line| {
        let mut w = line.split_whitespace();
        let Some((a,b)) = w.next().and_then(|s| s.split_once('-')) else { return false; };
        let (Ok(a),Ok(b)) = (u64::from_str_radix(a,16),u64::from_str_radix(b,16)) else { return false; };
        w.next().is_some_and(|p| p.starts_with('r')) && a <= addr && end <= b
    })
}
fn read_u32(mem: &File, addr: u64) -> io::Result<u64> {
    let mut b = [0u8;4]; mem.read_exact_at(&mut b,addr)?; Ok(u32::from_le_bytes(b) as u64)
}
struct Target {
    proc: PathBuf,
    mem: File,
    base: u64,
    object: u64,
    captured: bool,
}
impl Target {
    fn open() -> Option<Self> {
        for p in fs::read_dir("/proc").ok()?.filter_map(Result::ok) {
            if p.file_name().to_string_lossy().parse::<u32>().is_err() { continue; }
            let proc = p.path();
            if fs::read_link(proc.join("exe")).ok().as_deref() != Some(Path::new(EXE)) { continue; }
            // Hash the running executable, not just the file on disk.
            let mut executable = Vec::new();
            File::open(proc.join("exe")).ok()?.take(16*1024*1024).read_to_end(&mut executable).ok()?;
            if format!("{:x}",Sha256::digest(&executable)) != HASH { return None; }
            let maps = fs::read_to_string(proc.join("maps")).ok()?;
            let base = maps.lines().find_map(|line| {
                let w: Vec<_> = line.split_whitespace().collect();
                if w.len() != 6 || w[2] != "00000000" || w[5] != EXE { return None; }
                u64::from_str_radix(w[0].split_once('-')?.0,16).ok()
            })?;
            let mem = File::open(proc.join("mem")).ok()?;
            if !readable(&maps,base+GLOBAL,4) { return None; }
            let object = read_u32(&mem,base+GLOBAL).ok()?;
            if object == 0 || !readable(&maps,object,0x30) ||
                read_u32(&mem,object).ok()? != base+VTABLE { return None; }
            logf!("opentherm registers: verified downloader attached (read only)");
            return Some(Self {proc,mem,base,object,captured:false});
        }
        None
    }
    fn poll(&mut self) -> io::Result<Option<[u8;SIZE]>> {
        let bad = || io::Error::new(io::ErrorKind::InvalidData,"downloader changed");
        if read_u32(&self.mem,self.base+GLOBAL)? != self.object ||
            read_u32(&self.mem,self.object)? != self.base+VTABLE { return Err(bad()); }
        let ptr = read_u32(&self.mem,self.object+0x10)?;
        if ptr == 0 { self.captured = false; return Ok(None); }
        if self.captured { return Ok(None); }
        let len = read_u32(&self.mem,self.object+0x18)?;
        if !(SIZE as u64..=1024*1024).contains(&len) { return Ok(None); }
        let maps = fs::read_to_string(self.proc.join("maps"))?;
        if !readable(&maps,ptr,SIZE) { return Ok(None); }
        let mut a = [0u8;SIZE]; let mut b = [0u8;SIZE];
        // Double read and recheck pointer and received length to reject a
        // torn or released transfer. Never read the engineering log or heap.
        if self.mem.read_exact_at(&mut a,ptr).is_err() ||
            self.mem.read_exact_at(&mut b,ptr).is_err() ||
            read_u32(&self.mem,self.object+0x10)? != ptr ||
            read_u32(&self.mem,self.object+0x18)? < SIZE as u64 ||
            a != b || decode(&a).is_none() { return Ok(None); }
        self.captured = true;
        Ok(Some(a))
    }
}
fn save(dir: &Path,t: i64,data: &[u8;SIZE]) -> io::Result<()> {
    let tmp = dir.join("last.tmp");
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    let result = f.write_all(&t.to_le_bytes()).and_then(|_| f.write_all(data))
        .and_then(|_| f.sync_all()).and_then(|_| fs::rename(&tmp,dir.join("last.bin")));
    if result.is_err() { let _ = fs::remove_file(tmp); }
    result
}
fn load(dir: &Path) -> Option<(i64,Fields)> {
    let mut b = Vec::new();
    File::open(dir.join("last.bin")).ok()?.take((SIZE+9) as u64).read_to_end(&mut b).ok()?;
    if b.len() != SIZE+8 { return None; }
    Some((i64::from_le_bytes(b[..8].try_into().ok()?),decode(&b[8..])?))
}
fn shown(sample: &Option<(i64,Fields)>,now: i64) -> Fields {
    let mut out = Fields::new();
    if let Some((t,fields)) = sample {
        let fresh = (-30..=MAX_AGE).contains(&now.saturating_sub(*t));
        out.extend(fields.iter().map(|(k,v)| (k.clone(),if fresh {v.clone()} else {Value::Null})));
        out.insert(TIME.into(),chrono::DateTime::from_timestamp(*t,0)
            .map_or(Value::Null,|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs,true).into()));
    }
    out
}
pub fn watch(state: Arc<Watch>,settings: PathBuf,changed: impl Fn()+Send+'static) {
    std::thread::spawn(move || {
        let dir = Path::new(DIR);
        let mut target = None;
        let mut sample = None;
        let mut enabled = false;
        let mut checked = Instant::now() - Duration::from_secs(10);
        let mut retry = Instant::now();
        loop {
            if checked.elapsed() >= Duration::from_secs(5) {
                checked = Instant::now();
                let config = fs::read(&settings).unwrap_or_default();
                let next = allowed(dir) && crate::nlsettings::value(&config,"heatlinkheat") == Some(b"opentherm".as_slice());
                if next && !enabled { sample = load(dir); retry = Instant::now(); }
                if !next { target = None; sample = None; }
                enabled = next;
                if state.update_registers(shown(&sample,chrono::Utc::now().timestamp())) { changed(); }
            }
            if !enabled { std::thread::sleep(Duration::from_secs(1)); continue; }
            if target.is_none() && Instant::now() >= retry {
                target = Target::open(); retry = Instant::now()+Duration::from_secs(30);
            }
            if let Some(t) = target.as_mut() {
                match t.poll() {
                    Ok(Some(data)) => {
                        let now = chrono::Utc::now().timestamp();
                        if let Some(fields) = decode(&data) {
                            // A stopped previous process may have left its private temp file.
                            let _ = fs::remove_file(dir.join("last.tmp"));
                            if let Err(e) = save(dir,now,&data) { logf!("opentherm registers: save failed: {e}"); }
                            logf!("opentherm registers: captured validated 128-entry table");
                            sample = Some((now,fields));
                            if state.update_registers(shown(&sample,now)) { changed(); }
                        }
                    }
                    Ok(None) => {},
                    Err(_) => { target = None; retry = Instant::now()+Duration::from_secs(5); }
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> [u8;SIZE] {
        let mut b = [0;SIZE]; b[0]=3;
        for i in 0..128 { b[8+i*8+2]=i as u8; b[8+i*8+4]=7; }
        for (id,word) in [(3,0x2111u16),(6,0x0301),(48,0x3c1e),(49,0x501e),(56,0x3400),(57,0x5000),(127,0x0233)] {
            let off=8+id*8; b[off..off+2].copy_from_slice(&word.to_le_bytes()); b[off+4]=4;
        }
        b
    }
    #[test]
    fn opentherm_register_table_decodes_only_acknowledgements() {
        let mut b=fixture(); let f=decode(&b).unwrap();
        assert_eq!(f["opentherm_boiler_dhw_setpoint"],num(52.0));
        assert_eq!(f["opentherm_boiler_ch_limit"],num(80.0));
        assert_eq!(f["opentherm_dhw_max"],Value::from(60));
        assert_eq!(f["opentherm_dhw_min"],Value::from(30));
        assert_eq!(f["opentherm_dhw_access"],Value::from("Read/write"));
        assert_eq!(f["opentherm_ch_limit_access"],Value::from("Read only"));
        assert!(!f.contains_key("opentherm_dhw_flow_rate"));
        b[8+56*8+4]=5; assert!(!decode(&b).unwrap().contains_key("opentherm_boiler_dhw_setpoint"));
        b[8+56*8+2]=0; assert!(decode(&b).is_none());
        assert!(decode(&b[..1024]).is_none());
    }
    #[test]
    fn opentherm_register_table_expiry_and_mapping() {
        let f=decode(&fixture()).unwrap(); let s=Some((1000,f));
        assert_eq!(shown(&s,1001)["opentherm_dhw_max"],Value::from(60));
        assert!(shown(&s,1000+MAX_AGE+1)["opentherm_dhw_max"].is_null());
        assert!(!shown(&s,1000+MAX_AGE+1)[TIME].is_null());
        assert!(readable("1000-2000 rw-p 0 00:00 0",0x1100,1032));
        assert!(!readable("1000-2000 rw-p 0 00:00 0",0x1fff,4));
    }
}
