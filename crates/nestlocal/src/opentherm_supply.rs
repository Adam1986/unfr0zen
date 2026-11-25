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
