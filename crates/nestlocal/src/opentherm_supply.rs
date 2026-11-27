//! Managed supply-temperature control, with explicit installation limits and one-shot native IO.
//! Mode 4 is required; older experimental binaries cannot accept this ABI.
//! Never turn a native setter return into an optimistic bucket update.
use std::time::Duration;

const FIXED: u32 = 65536;
const ARM_MAGIC: u32 = 0x314d544f; // OTM1: reserved managed arm
const REQ_MAGIC: u32 = 0x3143544f; // OTC1: reserved managed request
const ACK_MAGIC: u32 = 0x314b544f; // OTK1: existing acknowledgement
const TTL: u32 = 30;
const ACK_WAIT: Duration = Duration::from_secs(35);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits { min: u32, max: u32 }
impl Limits {
    // Setup defaults to the full Nest Pro Setup range (1–100 C). An operator
    // may deliberately narrow it; these bounds are not a safety recommendation.
    pub(crate) fn new(min: u32, max: u32) -> Option<Self> {
        (min > 0 && min < max && max <= 100).then_some(Self { min, max })
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
                    outcome: Outcome::AwaitingNative })
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

    /// A matching status-2 ACK means the native setter returned and its
    /// immediate native getter matched the requested value. That is the
    /// transaction's confirmation point; the local settings file and a
    /// separate Heat Link ACK are not prerequisites.
    /// No terminal outcome can be changed by later or duplicated evidence.
    pub(crate) fn observe(&mut self, generation: Option<Generation>, elapsed: Duration,
                          ack: Option<&[u8]>) -> Outcome {
        if self.outcome != Outcome::AwaitingNative {
            return self.outcome;
        }
        if generation != Some(self.generation) || elapsed > ACK_WAIT {
            self.outcome = Outcome::Uncertain;
            return self.outcome;
        }
        if let Some([magic, pid, token, status, before, after]) = ack.and_then(decode) {
            if (magic, pid, token) == (ACK_MAGIC, self.generation.pid, self.token) {
                self.outcome = match status {
                    0 if before == after => Outcome::Rejected,
                    2 if before == self.before * FIXED && after == self.target * FIXED => Outcome::Confirmed,
                    _ => Outcome::Uncertain,
                };
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
const OPT_IN: &str = "/etc/nestlocal/opentherm-supply-control";
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
fn observed_baseline(limits: Limits, observed: f64) -> Option<u32> {
    if !observed.is_finite() || observed.fract() != 0.0 || observed < 1.0 || observed > 100.0 {
        return None;
    }
    let degrees = observed as u32;
    limits.contains(degrees).then_some(degrees)
}
fn parse_limits(text: &str) -> Option<Limits> {
    let mut parts = text.split_whitespace();
    let min = parts.next()?.parse().ok()?;
    let max = parts.next()?.parse().ok()?;
    if parts.next().is_some() { return None; }
    Limits::new(min,max)
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

fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

// The request filename is the native worker's only dispatch trigger. If it
// is absent, publication never reached the setter; it is then safe to remove
// a partial ARM and the lock. If it exists (or cannot be inspected), retain
// everything for manual review.
fn cleanup_unpublished(dir: &Path) -> Result<(), String> {
    let request = dir.join(REQUEST);
    if exists(&request) {
        return Err("native request is visible; refusing to clear transaction markers".into());
    }
    for path in [
        dir.join(ARM),
        dir.join(format!("{ARM}.partial")),
        dir.join(format!("{REQUEST}.partial")),
    ] {
        remove_if_present(&path)?;
    }
    if exists(&request) {
        return Err("native request appeared during cleanup; retaining transaction lock".into());
    }
    remove_if_present(&dir.join(LOCK))
}

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
    started: Instant,
}
struct Ready {
    generation: Generation,
    now: u32,
    before: u32,
}
struct Run {
    active: Option<Active>,
    status: &'static str,
}
/// Explicit local opt-in only. The in-flight marker survives a nestlocal
/// restart during the same boot. Uncertain outcomes require local review;
/// no automated retry, reverse write or marker clearing.
pub(crate) struct SupplyControl {
    limits: Limits,
    settings: PathBuf,
    dir: PathBuf,
    seen: AtomicBool,
    run: Mutex<Run>,
    last_ready_reason: Mutex<Option<&'static str>>,
}
fn stage_reason_for_code(outcome: Outcome, stage: u32) -> Option<&'static str> {
    match (outcome, stage) {
        (Outcome::Rejected, 11) => Some("rejected_no_native_client"),
        (Outcome::Rejected, 12) => Some("rejected_handler_allocation_failed"),
        (Outcome::Rejected, 13) => Some("rejected_event_allocation_failed"),
        (Outcome::Rejected, 20) => Some("rejected_screen_initialisation_guard"),
        (Outcome::Uncertain, 1) => Some("native_callback_not_acknowledged"),
        (Outcome::Uncertain, 2) => Some("native_callback_started_no_ack"),
        (Outcome::Uncertain, 3) => Some("native_validation_or_setter_no_ack"),
        (Outcome::Uncertain, 4) => Some("native_callback_reached_ack_stage_but_confirmation_uncertain"),
        (Outcome::Uncertain, 111 | 112 | 113 | 120) =>
            Some("pre_dispatch_rejection_ack_write_failed"),
        _ => None,
    }
}

fn stage_reason_for_record(text: &str, pid: u32, token: u32, outcome: Outcome) -> Option<&'static str> {
    let mut fields = text.split_whitespace();
    if fields.next()?.parse::<u32>().ok()? != pid { return None; }
    if fields.next()?.parse::<u32>().ok()? != token { return None; }
    let stage = fields.next()?.parse::<u32>().ok()?;
    if fields.next().is_some() { return None; }
    stage_reason_for_code(outcome, stage)
}

impl SupplyControl {
    pub(crate) fn load(settings: PathBuf) -> Option<Self> {
        let limits = read_limits(Path::new(OPT_IN))?;
        let dir = PathBuf::from(crate::settings_ui::DIR);
        let m = fs::symlink_metadata(&dir).ok()?;
        if !m.is_dir() || m.uid() != 0 || m.mode() & 0o077 != 0 { return None; }
        Some(Self { limits, settings, dir, seen: AtomicBool::new(false),
            run: Mutex::new(Run {active:None, status:"idle"}),
            last_ready_reason: Mutex::new(None) })
    }
    pub(crate) fn limits(&self) -> (u32,u32) { (self.limits.min,self.limits.max) }
    pub(crate) fn discovered(&self) -> bool { self.seen.load(Ordering::Relaxed) }
    pub(crate) fn active(&self) -> bool { self.run.lock().unwrap().active.is_some() }
    fn worker(&self, t: u32) -> Result<Generation, &'static str> {
        if exists(Path::new("/var/run/unfr0zen-screen.attempt")) {
            return Err("screen_initialisation_in_progress");
        }
        let bytes = fs::read(self.dir.join(READY)).map_err(|_| "native_worker_heartbeat_missing")?;
        if bytes.len() != 16 { return Err("native_worker_heartbeat_malformed"); }
        let word = |i: usize| u32::from_le_bytes(bytes[i*4..i*4+4].try_into().unwrap());
        if word(0) != 0x31525457 { return Err("native_worker_heartbeat_magic_mismatch"); }
        let pid = word(1);
        if pid <= 1 { return Err("native_worker_heartbeat_pid_invalid"); }
        let (g, started) = generation(pid).ok_or("nlclient_process_not_valid")?;
        let current = current_generation().ok_or("nlclient_process_missing_or_ambiguous")?;
        if current != g { return Err("nlclient_process_generation_mismatch"); }
        if word(3) != 4 { return Err("native_worker_mode_not_managed"); }
        if word(2) == 0 { return Err("native_worker_heartbeat_timestamp_invalid"); }
        let age = t.checked_sub(word(2)).ok_or("native_worker_heartbeat_in_future")?;
        if age > 6 { return Err("native_worker_heartbeat_stale"); }
        if word(2) < started { return Err("native_worker_heartbeat_predates_process"); }
        if !managed_worker_ready(&bytes,g,t) { return Err("native_worker_not_ready"); }
        self.seen.store(true,Ordering::Relaxed);
        Ok(g)
    }
    // Native event readback is authoritative for the current setting. The
    // persisted settings.config value may lag a successful native setter.
    fn before(&self, watch: &crate::opentherm::Watch) -> Option<u32> {
        observed_baseline(self.limits, watch.shown().get("opentherm_max_supply_temperature")?.as_f64()?)
    }
    fn readiness(&self, watch: &crate::opentherm::Watch) -> Result<Ready, &'static str> {
        if read_limits(Path::new(OPT_IN)) != Some(self.limits) {
            return Err("installation_limits_missing_or_changed");
        }
        if exists(&self.dir.join(LOCK)) {
            return Err("previous_command_requires_review");
        }
        if exists(&self.dir.join(ARM)) || exists(&self.dir.join(REQUEST)) {
            return Err("previous_command_record_present");
        }
        let t = now().ok_or("thermostat_clock_unavailable")?;
        let generation = self.worker(t)?;
        let before = self.before(watch).ok_or("native_supply_readback_missing_or_outside_limits")?;
        Ok(Ready { generation, now: t, before })
    }
    pub(crate) fn snapshot(&self, watch: &crate::opentherm::Watch) -> (bool,&'static str,bool,&'static str) {
        let readiness = self.readiness(watch);
        let (ready, reason) = match readiness {
            Ok(_) => (true, "ready"),
            Err(reason) => (false, reason),
        };
        {
            let mut previous = self.last_ready_reason.lock().unwrap();
            if *previous != Some(reason) {
                crate::logf!("OpenTherm write readiness transition: {} -> {reason}", (*previous).unwrap_or("uninitialised"));
                *previous = Some(reason);
            }
        }
        let run = self.run.lock().unwrap();
        let status = if run.active.is_none() && exists(&self.dir.join(LOCK)) { "review_required" } else { run.status };
        let reason = if run.active.is_some() { "command_in_progress" } else { reason };
        (ready,status,self.discovered(),reason)
    }
    fn publication_failed(&self, run: &mut Run, error: String) -> Result<(),String> {
        match cleanup_unpublished(&self.dir) {
            Ok(()) => {
                run.status = "publication_failed_no_native_dispatch";
                Err(error)
            }
            Err(cleanup) => {
                run.status = "review_required";
                Err(format!("{error}; safe cleanup failed: {cleanup}"))
            }
        }
    }

    pub(crate) fn submit(&self, arg: &str, watch: &crate::opentherm::Watch) -> Result<(),String> {
        let mut run = self.run.lock().unwrap();
        let target = self.limits.parse_target(arg).ok_or("invalid supply temperature")?;
        if run.active.is_some() { return Err("command_in_progress".into()); }
        let ready = match self.readiness(watch) {
            Ok(ready) => ready,
            Err(reason) => {
                crate::logf!("OpenTherm supply control: pre-dispatch rejection target={target} C reason={reason}; native request NOT published");
                return Err(reason.to_owned());
            }
        };
        let before = ready.before;
        if before == target { return Ok(()); }
        let t = ready.now;
        let g = ready.generation;
        let mut random = [0u8;4];
        fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut random)).map_err(|e| e.to_string())?;
        let token = u32::from_le_bytes(random);
        if fs::read(self.dir.join(ACK)).ok().and_then(|b| decode(&b))
            .is_some_and(|a| a[2] == token) { return Err("token collision; no command published".into()); }
        let tx = Transaction::new(g,token,t,self.limits,before,target).ok_or("invalid transaction")?;
        let (arm,request) = tx.records();
        // The exclusive marker is established before either record. Even
        // a partial publication leaves a lock requiring manual review.
        let lock = self.dir.join(LOCK);
        let mut file = fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&lock).map_err(|e| e.to_string())?;
        if let Err(error) = file.write_all(&request) {
            drop(file);
            return self.publication_failed(&mut run, format!("cannot write in-flight marker: {error}"));
        }
        drop(file);
        run.status = "publishing";
        if let Err(error) = publish_once(&self.dir,ARM,&arm).and_then(|_| publish_once(&self.dir,REQUEST,&request)) {
            return self.publication_failed(&mut run, format!("native publication failed: {error}"));
        }
        run.active = Some(Active { tx, started:Instant::now() });
        run.status = "awaiting_native";
        crate::logf!("OpenTherm supply control: published {before} -> {target} C, token {token}");
        Ok(())
    }
    fn native_stage_reason(&self, pid: u32, token: u32, outcome: Outcome) -> Option<&'static str> {
        let text = fs::read_to_string(self.dir.join("ot-managed.stage")).ok()?;
        stage_reason_for_record(&text, pid, token, outcome)
    }

    pub(crate) fn poll(&self) {
        let mut run = self.run.lock().unwrap();
        let Some(active) = run.active.as_mut() else { return; };
        let ack = fs::read(self.dir.join(ACK)).ok();
        let outcome = active.tx.observe(current_generation(),active.started.elapsed(),ack.as_deref());
        let failure = self.native_stage_reason(active.tx.generation.pid, active.tx.token, outcome);
        run.status = match outcome {
            Outcome::AwaitingNative => "awaiting_native",
            Outcome::Confirmed => "confirmed",
            Outcome::Rejected => failure.unwrap_or("rejected"),
            Outcome::Uncertain => failure.unwrap_or("review_required"),
        };
        if matches!(outcome,Outcome::Confirmed|Outcome::Rejected|Outcome::Uncertain) {
            if outcome != Outcome::Uncertain && ![ARM,REQUEST].iter().any(|n| exists(&self.dir.join(n))) {
                if let Err(e) = fs::remove_file(self.dir.join(LOCK)) {
                    crate::logf!("OpenTherm supply control: cannot release command marker: {e}");
                    run.status = "review_required";
                }
            }
            crate::logf!("OpenTherm supply control: final outcome {outcome:?} pid={} token={} target={} C stage_reason={}", active.tx.generation.pid, active.tx.token, active.tx.target, failure.unwrap_or("none"));
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
    fn baseline_uses_confirmed_native_readback_even_when_persistence_lags() {
        let limits = Limits::new(50, 65).unwrap();
        assert_eq!(observed_baseline(limits, 60.0), Some(60));
        assert_eq!(observed_baseline(limits, 65.0), Some(65));
        assert_eq!(observed_baseline(limits, 49.0), None);
        assert_eq!(observed_baseline(limits, 66.0), None);
        assert_eq!(observed_baseline(limits, 60.5), None);
        assert_eq!(observed_baseline(limits, f64::NAN), None);
    }
    #[test]
    fn control_publication_never_overwrites_a_pending_record() {
        let dir = std::env::temp_dir().join(format!("ot-control-{}-{:?}",std::process::id(),std::thread::current().id()));
        fs::create_dir(&dir).unwrap();
        publish_once(&dir, ARM, b"first").unwrap();
        assert!(publish_once(&dir, ARM, b"second").is_err());
        assert_eq!(fs::read(dir.join(ARM)).unwrap(),b"first");
        assert!(!dir.join(format!("{ARM}.partial")).exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn unpublished_cleanup_removes_only_records_without_a_request_trigger() {
        let dir = std::env::temp_dir().join(format!("ot-unpublished-{}-{:?}",std::process::id(),std::thread::current().id()));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join(LOCK), b"lock").unwrap();
        fs::write(dir.join(ARM), b"arm").unwrap();
        fs::write(dir.join(format!("{ARM}.partial")), b"partial-arm").unwrap();
        fs::write(dir.join(format!("{REQUEST}.partial")), b"partial-request").unwrap();
        cleanup_unpublished(&dir).unwrap();
        for name in [LOCK,ARM,REQUEST] {
            assert!(!dir.join(name).exists(), "{name} should be absent after safe cleanup");
        }
        assert!(!dir.join(format!("{ARM}.partial")).exists());
        assert!(!dir.join(format!("{REQUEST}.partial")).exists());

        fs::write(dir.join(LOCK), b"lock").unwrap();
        fs::write(dir.join(ARM), b"arm").unwrap();
        fs::write(dir.join(REQUEST), b"request").unwrap();
        assert!(cleanup_unpublished(&dir).is_err());
        assert!(dir.join(LOCK).exists());
        assert!(dir.join(ARM).exists());
        assert!(dir.join(REQUEST).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn explicit_control_limits_parser_has_no_default() {
        assert_eq!(parse_limits("50 55"),Limits::new(50,55));
        assert_eq!(parse_limits("50 60"),Limits::new(50,60));
        assert_eq!(parse_limits("1 100"),Limits::new(1,100));
        for invalid in ["","50","0 60","60 50","50 101","50 55 60","55 55","50.5 55"] {
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
        assert!(Limits::new(0,55).is_none());
        assert!(Limits::new(55,50).is_none());
        assert!(Limits::new(50,101).is_none());
        let l = Limits::new(50,60).unwrap();
        let full = Limits::new(1,100).unwrap();
        assert_eq!(full.parse_target("1"),Some(1));
        assert_eq!(full.parse_target("100"),Some(100));
        assert_eq!(full.parse_target("0"),None);
        assert!(Transaction::new(GEN,79,1000,full,50,100).is_some());
        for arg in ["NaN","inf","49","61","52.5",""] { assert_eq!(l.parse_target(arg), None); }
        assert_eq!(l.parse_target("50.0"), Some(50));
        assert_eq!(l.parse_target("60"), Some(60));
        assert!(Transaction::new(GEN,77,1000,l,55,55).is_none());
        assert!(Transaction::new(GEN,0,1000,l,55,50).is_none());
        assert!(Transaction::new(GEN,77,u32::MAX,l,55,50).is_none());
        let wide = Transaction::new(GEN,78,1000,l,55,60).unwrap();
        let (wide_arm,wide_request) = wide.records();
        assert_eq!(decode(&wide_arm),Some([ARM_MAGIC,42,78,1030,50*FIXED,60*FIXED]));
        assert_eq!(decode(&wide_request),Some([REQ_MAGIC,42,78,1030,55*FIXED,60*FIXED]));
        let (a,r) = trial().records();
        assert_eq!(decode(&a),Some([ARM_MAGIC,42,77,1030,50*FIXED,55*FIXED]));
        assert_eq!(decode(&r),Some([REQ_MAGIC,42,77,1030,55*FIXED,50*FIXED]));
        assert_eq!(decode(&r[..23]),None);
    }
    #[test]
    fn native_status_two_ack_confirms_without_a_settings_file_or_heat_link_ack() {
        let mut t = trial();
        let a = ack(2,55,50);
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&a)),Outcome::Confirmed);
        // Confirmed is terminal; delayed or contradictory evidence cannot
        // turn this already verified native transaction into a false negative.
        assert_eq!(t.observe(Some(GEN),secs(150),None),Outcome::Confirmed);
        assert_eq!(t.observe(Some(GEN),secs(151),Some(&ack(3,55,55))),Outcome::Confirmed);
    }
    #[test]
    fn stale_ack_and_restart_never_confirm() {
        let mut t = trial();
        let stale = encode([ACK_MAGIC,42,78,2,55*FIXED,50*FIXED]);
        assert_eq!(t.observe(Some(GEN),secs(5),Some(&stale)),Outcome::AwaitingNative);
        let next = Generation { start_ticks: 124, ..GEN };
        assert_eq!(t.observe(Some(next),secs(6),Some(&ack(2,55,50))),Outcome::Uncertain);
        assert_eq!(t.observe(Some(GEN),secs(7),Some(&ack(2,55,50))),Outcome::Uncertain);
    }
    #[test]
    fn native_rejection_and_readback_mismatch() {
        let mut t = trial();
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&ack(0,55,55))),Outcome::Rejected);
        let mut t = trial();
        let pre_dispatch = encode([ACK_MAGIC,42,77,0,0,0]);
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&pre_dispatch)),Outcome::Rejected);
        let mut t = trial();
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&ack(3,55,55))),Outcome::Uncertain);
        let mut t = trial();
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&ack(2,55,55))),Outcome::Uncertain);
        let mut t = trial();
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&ack(2,54,50))),Outcome::Uncertain);
    }
    #[test]
    fn native_stage_reasons_are_specific_and_unknown_values_stay_unclassified() {
        assert_eq!(stage_reason_for_code(Outcome::Rejected,11), Some("rejected_no_native_client"));
        assert_eq!(stage_reason_for_code(Outcome::Rejected,12), Some("rejected_handler_allocation_failed"));
        assert_eq!(stage_reason_for_code(Outcome::Rejected,13), Some("rejected_event_allocation_failed"));
        assert_eq!(stage_reason_for_code(Outcome::Rejected,20), Some("rejected_screen_initialisation_guard"));
        assert_eq!(stage_reason_for_code(Outcome::Uncertain,120), Some("pre_dispatch_rejection_ack_write_failed"));
        assert_eq!(stage_reason_for_code(Outcome::Uncertain,1), Some("native_callback_not_acknowledged"));
        assert_eq!(stage_reason_for_code(Outcome::Uncertain,2), Some("native_callback_started_no_ack"));
        assert_eq!(stage_reason_for_code(Outcome::Uncertain,3), Some("native_validation_or_setter_no_ack"));
        assert_eq!(stage_reason_for_code(Outcome::Uncertain,4),
            Some("native_callback_reached_ack_stage_but_confirmation_uncertain"));
        assert_eq!(stage_reason_for_code(Outcome::Confirmed,4), None);
        assert_eq!(stage_reason_for_code(Outcome::Rejected,4), None);
        assert_eq!(stage_reason_for_record("42 77 11", 42, 77, Outcome::Rejected),
            Some("rejected_no_native_client"));
        assert_eq!(stage_reason_for_record("42 78 11", 42, 77, Outcome::Rejected), None);
        assert_eq!(stage_reason_for_record("42 77 11 trailing", 42, 77, Outcome::Rejected), None);
        assert_eq!(stage_reason_for_record("42 11", 42, 77, Outcome::Rejected), None);
    }

    #[test]
    fn timeouts_and_wrong_ack_are_terminal_or_wait_only_for_native_ack() {
        let mut t = trial();
        assert_eq!(t.observe(Some(GEN),secs(36),None),Outcome::Uncertain);
        let mut t = trial();
        assert_eq!(t.observe(Some(GEN),secs(2),None),Outcome::AwaitingNative);
        assert_eq!(t.observe(Some(GEN),secs(3),Some(&ack(2,55,50))),Outcome::Confirmed);
        let mut t = trial();
        let wrong = encode([ACK_MAGIC,42,77,2,50*FIXED,55*FIXED]);
        assert_eq!(t.observe(Some(GEN),secs(2),Some(&wrong)),Outcome::Uncertain);
    }
}
