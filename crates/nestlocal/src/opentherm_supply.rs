//! Managed supply-temperature command lifecycle, independent of MQTT and IO.
//! Not connected to a live write path yet. V5 is the supervised one-way
//! trial; a future managed native mode must explicitly accept its own ABI.
//! Never turn a native setter return into an optimistic bucket update.
use std::time::Duration;

const FIXED: u32 = 65536;
const ARM_MAGIC: u32 = 0x314d544f; // OTM1: reserved managed arm
const REQ_MAGIC: u32 = 0x3143544f; // OTC1: reserved managed request
const ACK_MAGIC: u32 = 0x314b544f; // OTK1: existing acknowledgement
const TTL: u32 = 30;
const ACK_WAIT: Duration = Duration::from_secs(35);
const PERSIST_WAIT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits { min: u32, max: u32 }
impl Limits {
    // Representation bounds only: the operator must explicitly establish
    // installation-specific limits. There is deliberately no Default.
    pub(crate) fn new(min: u32, max: u32) -> Option<Self> {
        (min > 0 && min < max && max <= 100).then_some(Self { min, max })
    }
    // Match native mode 4's pilot ceiling; no default or automatic opt-in.
    pub(crate) fn pilot(min: u32, max: u32) -> Option<Self> {
        if min < 50 || max > 55 { return None; }
        Self::new(min, max)
    }
    pub(crate) fn parse_target(self, arg: &str) -> Option<u32> {
        let t = arg.trim().parse::<f64>().ok()?;
        if !t.is_finite() || t.fract() != 0.0 || t < self.min as f64 || t > self.max as f64 {
            return None;
        }
        Some(t as u32)
    }
    fn contains(self, t: u32) -> bool { (self.min..=self.max).contains(&t) }
}

/// Include the proc start ticks, not just a reusable PID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Generation { pub pid: u32, pub start_ticks: u64 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    AwaitingNative,
    AwaitingPersistence,
    Confirmed,
    Rejected,
    Uncertain,
}

pub(crate) struct Transaction {
    generation: Generation,
    token: u32,
    expires: u32,
    limits: Limits,
    before: u32,
    target: u32,
    outcome: Outcome,
    ack_at: Option<Duration>,
}

fn encode(words: [u32; 6]) -> [u8; 24] {
    let mut bytes = [0; 24];
    for (i, word) in words.into_iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    bytes
}
fn decode(bytes: &[u8]) -> Option<[u32; 6]> {
    if bytes.len() != 24 { return None; }
    let mut words = [0; 6];
    for (word, part) in words.iter_mut().zip(bytes.chunks_exact(4)) {
        *word = u32::from_le_bytes(part.try_into().ok()?);
    }
    Some(words)
}

impl Transaction {
    pub(crate) fn new(generation: Generation, token: u32, now: u32,
                      limits: Limits, before: u32, target: u32) -> Option<Self> {
        if generation.pid <= 1 || generation.start_ticks == 0 || token == 0 || now == 0
            || !limits.contains(before) || !limits.contains(target) || before == target {
            return None;
        }
        Some(Self { generation, token, expires: now.checked_add(TTL)?, limits, before, target,
                    outcome: Outcome::AwaitingNative, ack_at: None })
    }

    /// A future IO adapter must publish arm first, request last, once only,
    /// with exclusive producer ownership and no replacement of in-flight
    /// files. These magics are deliberately NOT accepted by the V5 binary.
    pub(crate) fn records(&self) -> ([u8; 24], [u8; 24]) {
        (encode([ARM_MAGIC, self.generation.pid, self.token, self.expires,
                 self.limits.min * FIXED, self.limits.max * FIXED]),
         encode([REQ_MAGIC, self.generation.pid, self.token, self.expires,
                 self.before * FIXED, self.target * FIXED]))
    }

    /// Read-only observations. elapsed is monotonic since publication.
    /// Supply values are native 16.16 raw values, not object-store echoes.
    /// A fresh independent telemetry sample must match persisted settings.
    /// No terminal outcome can be changed by later or duplicated evidence.
    pub(crate) fn observe(&mut self, generation: Option<Generation>, elapsed: Duration,
                          ack: Option<&[u8]>, saved: Option<u32>, telemetry: Option<u32>) -> Outcome {
        if !matches!(self.outcome, Outcome::AwaitingNative | Outcome::AwaitingPersistence) {
            return self.outcome;
        }
        if generation != Some(self.generation) {
            self.outcome = Outcome::Uncertain;
            return self.outcome;
        }
        if self.outcome == Outcome::AwaitingNative {
            if elapsed > ACK_WAIT {
                self.outcome = Outcome::Uncertain;
                return self.outcome;
            }
            if let Some([magic, pid, token, status, before, after]) = ack.and_then(decode) {
                if (magic, pid, token) == (ACK_MAGIC, self.generation.pid, self.token) {
                    match status {
                        0 if before == after => self.outcome = Outcome::Rejected,
                        2 if before == self.before * FIXED && after == self.target * FIXED => {
                            self.outcome = Outcome::AwaitingPersistence;
                            self.ack_at = Some(elapsed);
                        }
                        _ => self.outcome = Outcome::Uncertain,
                    }
                }
            }
        }
        if self.outcome == Outcome::AwaitingPersistence {
            if elapsed.saturating_sub(self.ack_at.unwrap_or(elapsed)) > PERSIST_WAIT {
                self.outcome = Outcome::Uncertain;
            } else if saved == Some(self.target * FIXED) && telemetry == saved {
                self.outcome = Outcome::Confirmed;
            } else if saved.is_some_and(|v| v != self.before * FIXED && v != self.target * FIXED)
                || telemetry.is_some_and(|v| v != self.before * FIXED && v != self.target * FIXED) {
                self.outcome = Outcome::Uncertain;
            }
        }
        self.outcome
    }
}

// Read-only mode 3 worker heartbeat (separate from an ACK). A live PID
// and fresh timestamp are necessary but not proof of main-thread execution.
// Managed writes must use a different capability/mode value.
pub(crate) fn auto_worker_ready(bytes: &[u8], generation: Generation, now: u32) -> bool {
    worker_ready(bytes, generation, now, 3)
}
pub(crate) fn managed_worker_ready(bytes: &[u8], generation: Generation, now: u32) -> bool {
    worker_ready(bytes, generation, now, 4)
}
fn worker_ready(bytes: &[u8], generation: Generation, now: u32, mode: u32) -> bool {
    if bytes.len() != 16 || generation.pid <= 1 || generation.start_ticks == 0 { return false; }
    let word = |i: usize| u32::from_le_bytes(bytes[i*4..i*4+4].try_into().unwrap());
    word(0) == 0x31525457 && word(1) == generation.pid && word(3) == mode
        && word(2) != 0 && now.checked_sub(word(2)).is_some_and(|age| age <= 6)
}


pub(crate) const CONTROL: &str = "opentherm_max_supply_temperature_control";
const OPT_IN: &str = "/etc/nestlocal/opentherm-supply-pilot";
const READY: &str = "ot-supply.ready";
const LOCK: &str = "ot-managed.inflight";
const ARM: &str = "ot-managed.arm";
const REQUEST: &str = "ot-managed.request";
const ACK: &str = "ot-managed.ack";

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

fn now() -> Option<u32> { chrono::Utc::now().timestamp().try_into().ok() }
fn parse_limits(text: &str) -> Option<Limits> {
    let mut parts = text.split_whitespace();
    let min = parts.next()?.parse().ok()?;
    let max = parts.next()?.parse().ok()?;
    if parts.next().is_some() { return None; }
    Limits::pilot(min,max)
}
fn read_limits(path: &Path) -> Option<Limits> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o077 != 0 || meta.len() > 64 { return None; }
    parse_limits(&fs::read_to_string(path).ok()?)
}

fn generation(pid: u32) -> Option<(Generation, u32)> {
    let proc = PathBuf::from(format!("/proc/{pid}"));
    if fs::read_link(proc.join("exe")).ok()?.as_path() != Path::new("/nestlabs/sbin/nlclient") { return None; }
    let stat = fs::read_to_string(proc.join("stat")).ok()?;
    let start_ticks: u64 = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()?;
    let boot: u64 = fs::read_to_string("/proc/stat").ok()?.lines()
        .find_map(|l| l.strip_prefix("btime ")?.trim().parse().ok())?;
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks <= 0 || start_ticks == 0 || pid <= 1 { return None; }
    let started = boot.checked_add(start_ticks / ticks as u64)?.try_into().ok()?;
    Some((Generation { pid, start_ticks }, started))
}
fn current_generation() -> Option<Generation> {
    let pids = crate::pids("nlclient");
    if pids.len() != 1 { return None; }
    generation(pids[0].try_into().ok()?).map(|(g,_)| g)
}
fn exists(path: &Path) -> bool { path.try_exists().unwrap_or(true) }

// Never replace a pending command. A private, exclusively created file is
// linked into its final name atomically. Both paths are on the same tmpfs.
fn publish_once(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<()> {
    let tmp = dir.join(format!("{name}.partial"));
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    let result = f.write_all(data).and_then(|_| fs::hard_link(&tmp, dir.join(name)));
    drop(f);
    let _ = fs::remove_file(tmp);
    result
}
fn saved_raw(path: &Path) -> Option<u32> {
    let config = fs::read(path).ok()?;
    if crate::nlsettings::value(&config, "heatlinkheat")? != b"opentherm" { return None; }
    let entries = crate::nlsettings::entries(&config, "supplytemp");
    if entries.len() != 1 { return None; }
    let raw: u32 = std::str::from_utf8(entries[0].2).ok()?.parse().ok()?;
    (raw > 0 && raw <= 100*FIXED && raw % FIXED == 0).then_some(raw)
}
struct Active {
    tx: Transaction,
    issued: u32,
    started: Instant,
}
struct Run {
    active: Option<Active>,
    status: &'static str,
}
/// Explicit local opt-in only. The in-flight marker survives a nestlocal
/// restart during the same boot. Uncertain outcomes require local review;
/// no automated retry, reverse write or marker clearing.
pub(crate) struct Pilot {
    limits: Limits,
    settings: PathBuf,
    dir: PathBuf,
    seen: AtomicBool,
    run: Mutex<Run>,
}
impl Pilot {
    pub(crate) fn load(settings: PathBuf) -> Option<Self> {
        let limits = read_limits(Path::new(OPT_IN))?;
        let dir = PathBuf::from(crate::settings_ui::DIR);
        let m = fs::symlink_metadata(&dir).ok()?;
        if !m.is_dir() || m.uid() != 0 || m.mode() & 0o077 != 0 { return None; }
        Some(Self { limits, settings, dir, seen: AtomicBool::new(false),
            run: Mutex::new(Run {active:None, status:"idle"}) })
    }
    pub(crate) fn limits(&self) -> (u32,u32) { (self.limits.min,self.limits.max) }
    pub(crate) fn discovered(&self) -> bool { self.seen.load(Ordering::Relaxed) }
    fn worker(&self, t: u32) -> Option<Generation> {
        if read_limits(Path::new(OPT_IN)) != Some(self.limits) { return None; }
        if exists(Path::new("/var/run/unfr0zen-screen.attempt")) { return None; }
        let bytes = fs::read(self.dir.join(READY)).ok()?;
        if bytes.len() != 16 { return None; }
        let pid = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
        let (g, started) = generation(pid)?;
        if current_generation() != Some(g) || !managed_worker_ready(&bytes,g,t)
            || u32::from_le_bytes(bytes[8..12].try_into().ok()?) < started { return None; }
        self.seen.store(true,Ordering::Relaxed);
        Some(g)
    }
    fn before(&self, watch: &crate::opentherm::Watch) -> Option<u32> {
        let raw = saved_raw(&self.settings)?;
        let observed = watch.shown().get("opentherm_max_supply_temperature")?.as_f64()?;
        let degrees = raw / FIXED;
        (self.limits.contains(degrees) && observed == degrees as f64).then_some(degrees)
    }
    fn available(&self, watch: &crate::opentherm::Watch) -> bool {
        let Some(t) = now() else { return false; };
        self.worker(t).is_some() && ![LOCK,ARM,REQUEST].iter().any(|n| exists(&self.dir.join(n)))
            && watch.pilot_idle(t as i64) && self.before(watch).is_some()
    }
    pub(crate) fn snapshot(&self, watch: &crate::opentherm::Watch) -> (bool,&'static str,bool) {
        let available = self.available(watch);
        let run = self.run.lock().unwrap();
        let status = if run.active.is_none() && exists(&self.dir.join(LOCK)) { "review_required" } else { run.status };
        (available,status,self.discovered())
    }
    pub(crate) fn submit(&self, arg: &str, watch: &crate::opentherm::Watch) -> Result<(),String> {
        let mut run = self.run.lock().unwrap();
        let target = self.limits.parse_target(arg).ok_or("invalid pilot temperature")?;
        if run.active.is_some() || !self.available(watch) { return Err("native pilot not ready or previous command unresolved".into()); }
        let before = self.before(watch).ok_or("saved and observed supply differ")?;
        if before == target { return Ok(()); }
        let t = now().ok_or("invalid clock")?;
        let g = self.worker(t).ok_or("native worker not ready")?;
        let mut random = [0u8;4];
        fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut random)).map_err(|e| e.to_string())?;
        let token = u32::from_le_bytes(random);
        let tx = Transaction::new(g,token,t,self.limits,before,target).ok_or("invalid transaction")?;
        let (arm,request) = tx.records();
        // The exclusive marker is established before either record. Even
        // a partial publication leaves a lock requiring manual review.
        let lock = self.dir.join(LOCK);
        let mut file = fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(lock).map_err(|e| e.to_string())?;
        file.write_all(&request).map_err(|e| e.to_string())?;
        drop(file);
        run.status = "publishing";
        if let Err(e) = publish_once(&self.dir,ARM,&arm).and_then(|_| publish_once(&self.dir,REQUEST,&request)) {
            run.status = "review_required";
            return Err(format!("native publication uncertain: {e}"));
        }
        run.active = Some(Active { tx, issued:t, started:Instant::now() });
        run.status = "awaiting_native";
        crate::logf!("OpenTherm pilot: published {before} -> {target} C, token {token}");
        Ok(())
    }
    pub(crate) fn poll(&self, watch: &crate::opentherm::Watch) {
        let mut run = self.run.lock().unwrap();
        let Some(active) = run.active.as_mut() else { return; };
        let ack = fs::read(self.dir.join(ACK)).ok();
        let outcome = active.tx.observe(current_generation(),active.started.elapsed(),ack.as_deref(),
            saved_raw(&self.settings),watch.pilot_event_since(active.issued as i64));
        run.status = match outcome {
            Outcome::AwaitingNative => "awaiting_native",
            Outcome::AwaitingPersistence => "awaiting_persistence",
            Outcome::Confirmed => "confirmed",
            Outcome::Rejected => "rejected",
            Outcome::Uncertain => "review_required",
        };
        if matches!(outcome,Outcome::Confirmed|Outcome::Rejected|Outcome::Uncertain) {
            if outcome != Outcome::Uncertain && ![ARM,REQUEST].iter().any(|n| exists(&self.dir.join(n))) {
                if let Err(e) = fs::remove_file(self.dir.join(LOCK)) {
                    crate::logf!("OpenTherm pilot: cannot release command marker: {e}");
                    run.status = "review_required";
                }
            }
            crate::logf!("OpenTherm pilot: final outcome {outcome:?}");
            run.active = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const GEN: Generation = Generation { pid: 42, start_ticks: 123 };
    fn trial() -> Transaction { Transaction::new(GEN, 77, 1000, Limits::new(50,55).unwrap(), 55,50).unwrap() }
    fn ack(status: u32, before: u32, after: u32) -> [u8; 24] {
        encode([ACK_MAGIC, 42, 77, status, before * FIXED, after * FIXED])
    }
    fn secs(n: u64) -> Duration { Duration::from_secs(n) }
    #[test]
    fn pilot_opt_in_parser_has_no_default() {
        assert_eq!(parse_limits("50 55"),Limits::pilot(50,55));
        for invalid in ["","50","49 55","50 56","50 55 60","55 55","50.5 55"] {
            assert_eq!(parse_limits(invalid),None);
        }
    }
    #[test]
    fn auto_worker_heartbeat_is_fresh_and_mode_specific() {
        let record: Vec<u8> = [0x31525457u32, 42, 1000, 3].into_iter().flat_map(u32::to_le_bytes).collect();
        assert!(auto_worker_ready(&record, GEN, 1002));
        assert!(!auto_worker_ready(&record, GEN, 1007));
        assert!(!auto_worker_ready(&record, GEN, 999));
        assert!(!auto_worker_ready(&record[..15], GEN, 1002));
        assert!(!auto_worker_ready(&record, Generation {pid:43,..GEN}, 1002));
        let mut wrong_mode = record.clone();
        wrong_mode[12] = 2;
        assert!(!auto_worker_ready(&wrong_mode, GEN, 1002));
        assert!(!managed_worker_ready(&record, GEN, 1002));
        wrong_mode[12] = 4;
        assert!(managed_worker_ready(&wrong_mode, GEN, 1002));
        assert!(!auto_worker_ready(&wrong_mode, GEN, 1002));
        assert!(!managed_worker_ready(&wrong_mode, GEN, 1007));
    }
    #[test]
    fn managed_bounds_and_records() {
        assert!(Limits::pilot(49,55).is_none());
        assert!(Limits::pilot(50,56).is_none());
        assert!(Limits::pilot(55,55).is_none());
        assert!(Limits::pilot(50,55).is_some());
        assert!(Limits::new(0,55).is_none());
        assert!(Limits::new(55,50).is_none());
        assert!(Limits::new(50,101).is_none());
        let l = Limits::new(50,55).unwrap();
        for arg in ["NaN","inf","49","56","52.5",""] { assert_eq!(l.parse_target(arg), None); }
        assert_eq!(l.parse_target("50.0"), Some(50));
        assert!(Transaction::new(GEN,77,1000,l,55,55).is_none());
        assert!(Transaction::new(GEN,0,1000,l,55,50).is_none());
        assert!(Transaction::new(GEN,77,u32::MAX,l,55,50).is_none());
        let (a,r) = trial().records();
        assert_eq!(decode(&a),Some([ARM_MAGIC,42,77,1030,50*FIXED,55*FIXED]));
        assert_eq!(decode(&r),Some([REQ_MAGIC,42,77,1030,55*FIXED,50*FIXED]));
        assert_eq!(decode(&r[..23]),None);
    }
    #[test]
    fn delayed_persistence_is_not_a_failed_native_write() {
        let mut t = trial();
        let a = ack(2,55,50);
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&a),Some(55*FIXED),Some(50*FIXED)),Outcome::AwaitingPersistence);
        assert_eq!(t.observe(Some(GEN),secs(40),None,Some(55*FIXED),Some(50*FIXED)),Outcome::AwaitingPersistence);
        assert_eq!(t.observe(Some(GEN),secs(45),None,Some(50*FIXED),Some(50*FIXED)),Outcome::Confirmed);
    }
    #[test]
    fn stale_ack_and_restart_never_confirm() {
        let mut t = trial();
        let stale = encode([ACK_MAGIC,42,78,2,55*FIXED,50*FIXED]);
        assert_eq!(t.observe(Some(GEN),secs(5),Some(&stale),Some(50*FIXED),Some(50*FIXED)),Outcome::AwaitingNative);
        let next = Generation { start_ticks: 124, ..GEN };
        assert_eq!(t.observe(Some(next),secs(6),Some(&ack(2,55,50)),Some(50*FIXED),Some(50*FIXED)),Outcome::Uncertain);
        assert_eq!(t.observe(Some(GEN),secs(7),Some(&ack(2,55,50)),Some(50*FIXED),Some(50*FIXED)),Outcome::Uncertain);
    }
    #[test]
    fn native_rejection_and_readback_mismatch() {
        let mut t = trial();
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&ack(0,55,55)),None,None),Outcome::Rejected);
        let mut t = trial();
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&ack(3,55,55)),None,None),Outcome::Uncertain);
        let mut t = trial();
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&ack(2,55,55)),None,None),Outcome::Uncertain);
    }
    #[test]
    fn timeouts_and_conflicting_readback_are_terminal() {
        let mut t = trial();
        assert_eq!(t.observe(Some(GEN),secs(36),None,None,None),Outcome::Uncertain);
        let mut t = trial();
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&ack(2,55,50)),None,None),Outcome::AwaitingPersistence);
        assert_eq!(t.observe(Some(GEN),secs(123),None,Some(50*FIXED),Some(50*FIXED)),Outcome::Uncertain);
        let mut t = trial();
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&ack(2,55,50)),Some(52*FIXED),None),Outcome::Uncertain);
    }
}
