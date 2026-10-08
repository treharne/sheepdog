//! Per-attachment replay and echo barriers around one serialized PTY writer.
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

pub const MAX_INPUT: usize = 64 * 1024;
pub const INPUT_BUDGET: usize = 1024 * 1024;
const RECORD_COST: usize = 32;
const MAX_ATTACHMENTS: usize = 128;
pub const ECHO_SETTLE: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AttachmentId(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Connection {
    pub(crate) id: AttachmentId,
    pub(crate) epoch: u64,
}

impl Connection {
    pub fn attachment_id(self) -> AttachmentId {
        self.id
    }
    pub fn epoch(self) -> u64 {
        self.epoch
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub connection_epoch: u64,
    pub connected: bool,
    pub accepted: u64,
    pub echo_ack: u64,
    pub prediction_epoch: u64,
    pub prediction_allowed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputResult {
    Accepted,
    Replay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    StaleConnection,
    SequenceGap,
    InvalidInput,
    Busy,
    Exhausted,
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for InputError {}

#[derive(Debug)]
struct Attachment {
    epoch: u64,
    connected: bool,
    accepted: u64,
    echo_ack: u64,
    prediction_epoch: u64,
}

struct Chunk {
    source: Option<(AttachmentId, u64)>,
    bytes: Vec<u8>,
    offset: usize,
}
struct Completed {
    source: (AttachmentId, u64),
    at: Instant,
}

#[derive(Default)]
pub(crate) struct Ledger {
    attachments: BTreeMap<AttachmentId, Attachment>,
    next_id: u64,
    chunks: VecDeque<Chunk>,
    completed: VecDeque<Completed>,
    budget: usize,
}

impl Ledger {
    fn invalidate_all(&mut self) {
        // A saturated counter cannot silently continue prediction.
        for a in self.attachments.values_mut() {
            a.prediction_epoch = a.prediction_epoch.saturating_add(1);
        }
    }

    pub fn attach(&mut self) -> Result<Connection, InputError> {
        if self.attachments.len() >= MAX_ATTACHMENTS {
            return Err(InputError::Busy);
        }
        self.next_id = self.next_id.checked_add(1).ok_or(InputError::Exhausted)?;
        let id = AttachmentId(self.next_id);
        self.attachments.insert(
            id,
            Attachment {
                epoch: 1,
                connected: true,
                accepted: 0,
                echo_ack: 0,
                prediction_epoch: 0,
            },
        );
        self.invalidate_all();
        Ok(Connection { id, epoch: 1 })
    }

    // Disconnected attachments can reconnect using their last valid epoch.
    pub fn resume(&mut self, old: Connection) -> Result<Connection, InputError> {
        let a = self
            .attachments
            .get(&old.id)
            .ok_or(InputError::StaleConnection)?;
        if a.epoch != old.epoch {
            return Err(InputError::StaleConnection);
        }
        self.claim(old.id)
    }

    fn claim(&mut self, id: AttachmentId) -> Result<Connection, InputError> {
        let a = self
            .attachments
            .get_mut(&id)
            .ok_or(InputError::StaleConnection)?;
        a.epoch = a.epoch.checked_add(1).ok_or(InputError::Exhausted)?;
        a.connected = true;
        let epoch = a.epoch;
        self.invalidate_all();
        Ok(Connection { id, epoch })
    }

    pub fn detach(&mut self, connection: Connection) -> Result<(), InputError> {
        self.validate(connection)?;
        self.attachments.get_mut(&connection.id).unwrap().connected = false;
        self.invalidate_all();
        Ok(())
    }

    pub fn retire(&mut self, c: Connection) -> Result<(), InputError> {
        let a = self
            .attachments
            .get(&c.id)
            .ok_or(InputError::StaleConnection)?;
        if a.epoch != c.epoch {
            return Err(InputError::StaleConnection);
        }
        if a.connected || a.accepted != a.echo_ack {
            return Err(InputError::Busy);
        }
        self.attachments.remove(&c.id);
        self.invalidate_all();
        Ok(())
    }

    pub fn validate(&self, c: Connection) -> Result<(), InputError> {
        match self.attachments.get(&c.id) {
            Some(a) if a.connected && a.epoch == c.epoch => Ok(()),
            _ => Err(InputError::StaleConnection),
        }
    }

    pub fn input(
        &mut self,
        c: Connection,
        seq: u64,
        bytes: Vec<u8>,
    ) -> Result<InputResult, InputError> {
        self.validate(c)?;
        if seq == 0 || bytes.is_empty() || bytes.len() > MAX_INPUT {
            return Err(InputError::InvalidInput);
        }
        let a = self.attachments.get(&c.id).unwrap();
        if seq <= a.accepted {
            return Ok(InputResult::Replay);
        }
        if a.accepted.checked_add(1) != Some(seq) {
            return Err(InputError::SequenceGap);
        }
        let cost = bytes.len() + RECORD_COST;
        if self.budget + cost > INPUT_BUDGET {
            return Err(InputError::Busy);
        }
        self.budget += cost;
        self.chunks.push_back(Chunk {
            source: Some((c.id, seq)),
            bytes,
            offset: 0,
        });
        self.attachments.get_mut(&c.id).unwrap().accepted = seq;
        for (id, other) in &mut self.attachments {
            if *id != c.id {
                other.prediction_epoch = other.prediction_epoch.saturating_add(1);
            }
        }
        Ok(InputResult::Accepted)
    }

    /// Terminal-generated DSR/DA responses share the same writer and budget.
    pub fn generated(&mut self, bytes: Vec<u8>) -> Result<(), InputError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let cost = bytes
            .len()
            .checked_add(RECORD_COST)
            .ok_or(InputError::Busy)?;
        if cost > INPUT_BUDGET.saturating_sub(self.budget) {
            return Err(InputError::Busy);
        }
        self.budget += cost;
        self.chunks.push_back(Chunk {
            source: None,
            bytes,
            offset: 0,
        });
        Ok(())
    }

    pub fn write_buf(&self) -> Option<&[u8]> {
        self.chunks.front().map(|c| &c.bytes[c.offset..])
    }

    pub fn written(&mut self, n: usize, at: Instant) {
        let c = self.chunks.front_mut().expect("writer had a chunk");
        assert!(n > 0 && n <= c.bytes.len() - c.offset);
        c.offset += n;
        self.budget -= n;
        if c.offset == c.bytes.len() {
            let c = self.chunks.pop_front().unwrap();
            if let Some(source) = c.source {
                self.completed.push_back(Completed { source, at });
            } else {
                self.budget -= RECORD_COST;
            }
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.completed.front().map(|c| c.at + ECHO_SETTLE)
    }

    /// Caller publishes the barrier with an authoritative screen snapshot.
    pub fn settle(&mut self, now: Instant) -> bool {
        let mut changed = false;
        while self
            .completed
            .front()
            .is_some_and(|c| now.saturating_duration_since(c.at) >= ECHO_SETTLE)
        {
            let c = self.completed.pop_front().unwrap();
            self.attachments.get_mut(&c.source.0).unwrap().echo_ack = c.source.1;
            self.budget -= RECORD_COST;
            changed = true;
        }
        changed
    }

    pub fn geometry_changed(&mut self) {
        self.invalidate_all();
    }

    pub fn checkpoints(&self) -> BTreeMap<AttachmentId, Checkpoint> {
        let active = self.attachments.values().filter(|a| a.connected).count();
        self.attachments
            .iter()
            .map(|(id, a)| {
                let foreign_pending = self
                    .attachments
                    .iter()
                    .any(|(other_id, other)| other_id != id && other.accepted > other.echo_ack);
                (
                    *id,
                    Checkpoint {
                        connection_epoch: a.epoch,
                        connected: a.connected,
                        accepted: a.accepted,
                        echo_ack: a.echo_ack,
                        prediction_epoch: a.prediction_epoch,
                        prediction_allowed: a.connected
                            && active == 1
                            && !foreign_pending
                            && a.prediction_epoch != u64::MAX,
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_sequences_share_one_ordered_writer() {
        let mut l = Ledger::default();
        let a = l.attach().unwrap();
        let b = l.attach().unwrap();
        l.input(a, 1, b"aaa".to_vec()).unwrap();
        l.input(b, 1, b"bbb".to_vec()).unwrap();
        let now = Instant::now();
        assert_eq!(l.write_buf(), Some(b"aaa".as_slice()));
        l.written(2, now);
        assert!(!l.settle(now + ECHO_SETTLE));
        assert_eq!(l.checkpoints()[&a.id].echo_ack, 0);
        l.written(1, now);
        assert_eq!(l.write_buf(), Some(b"bbb".as_slice()));
        l.written(3, now + Duration::from_millis(10));
        assert!(l.settle(now + ECHO_SETTLE));
        assert_eq!(l.checkpoints()[&a.id].echo_ack, 1);
        assert_eq!(l.checkpoints()[&b.id].echo_ack, 0);
        l.settle(now + ECHO_SETTLE + Duration::from_millis(10));
        assert_eq!(l.checkpoints()[&b.id].echo_ack, 1);
        assert_eq!(l.budget, 0);
    }

    #[test]
    fn resume_replays_are_deduplicated_and_stale_epoch_cannot_write_or_detach() {
        let mut l = Ledger::default();
        let a = l.attach().unwrap();
        l.input(a, 1, b"x".to_vec()).unwrap();
        l.detach(a).unwrap();
        let next = l.resume(a).unwrap();
        assert_eq!(l.input(next, 1, b"x".to_vec()), Ok(InputResult::Replay));
        assert_eq!(
            l.input(a, 2, b"y".to_vec()),
            Err(InputError::StaleConnection)
        );
        assert_eq!(l.detach(a), Err(InputError::StaleConnection));
        assert_eq!(l.resume(a), Err(InputError::StaleConnection));
        assert_eq!(l.write_buf(), Some(b"x".as_slice()));
        assert_eq!(
            l.input(next, 3, b"z".to_vec()),
            Err(InputError::SequenceGap)
        );
    }

    #[test]
    fn budget_rejection_does_not_accept_or_drop_input() {
        let mut l = Ledger::default();
        let a = l.attach().unwrap();
        let mut seq = 1;
        loop {
            match l.input(a, seq, vec![0; MAX_INPUT]) {
                Ok(_) => seq += 1,
                Err(InputError::Busy) => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(l.checkpoints()[&a.id].accepted, seq - 1);
        while let Some(bytes) = l.write_buf() {
            let n = bytes.len();
            l.written(n, Instant::now());
        }
        l.settle(Instant::now() + ECHO_SETTLE);
        assert_eq!(
            l.input(a, seq, b"retry".to_vec()),
            Ok(InputResult::Accepted)
        );
    }

    #[test]
    fn tiny_completed_records_remain_bounded_before_their_echo_barrier() {
        let mut l = Ledger::default();
        let a = l.attach().unwrap();
        let now = Instant::now();
        let mut seq = 1;
        while l.input(a, seq, vec![b'x']).is_ok() {
            l.written(1, now);
            seq += 1;
        }
        assert!(seq <= (INPUT_BUDGET / RECORD_COST + 1) as u64);
        assert!(l.budget <= INPUT_BUDGET);
        l.settle(now + ECHO_SETTLE);
        assert_eq!(l.budget, 0);
    }

    #[test]
    fn prediction_stays_disabled_until_foreign_input_settles() {
        let mut l = Ledger::default();
        let a = l.attach().unwrap();
        let before = l.checkpoints()[&a.id];
        assert!(before.prediction_allowed);
        let b = l.attach().unwrap();
        assert!(!l.checkpoints()[&a.id].prediction_allowed);
        l.input(b, 1, b"foreign".to_vec()).unwrap();
        l.detach(b).unwrap();
        assert!(!l.checkpoints()[&a.id].prediction_allowed);
        let now = Instant::now();
        l.written(7, now);
        assert!(!l.checkpoints()[&a.id].prediction_allowed);
        l.settle(now + ECHO_SETTLE);
        assert!(l.checkpoints()[&a.id].prediction_allowed);
        assert!(l.checkpoints()[&a.id].prediction_epoch > before.prediction_epoch);
        assert_eq!(l.checkpoints()[&a.id].echo_ack, 0);
    }

    #[test]
    fn retirement_reclaims_slots_but_never_discards_pending_input() {
        let mut l = Ledger::default();
        let a = l.attach().unwrap();
        l.input(a, 1, b"pending".to_vec()).unwrap();
        l.detach(a).unwrap();
        assert_eq!(l.retire(a), Err(InputError::Busy));
        let now = Instant::now();
        l.written(7, now);
        l.settle(now + ECHO_SETTLE);
        l.retire(a).unwrap();
        assert_eq!(l.resume(a), Err(InputError::StaleConnection));
        let mut last = None;
        for _ in 0..MAX_ATTACHMENTS {
            last = Some(l.attach().unwrap());
        }
        assert_eq!(l.attach(), Err(InputError::Busy));
        let last = last.unwrap();
        l.detach(last).unwrap();
        l.retire(last).unwrap();
        assert!(l.attach().is_ok());
    }

    #[test]
    fn generated_replies_do_not_ack_user_input() {
        let mut l = Ledger::default();
        let a = l.attach().unwrap();
        l.generated(b"response".to_vec()).unwrap();
        l.written(8, Instant::now());
        assert!(!l.settle(Instant::now() + ECHO_SETTLE));
        assert_eq!(l.checkpoints()[&a.id].echo_ack, 0);
        assert_eq!(l.budget, 0);
    }
}
