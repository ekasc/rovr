use std::{collections::VecDeque, time::SystemTime};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    pub sequence: u64,
    pub unix_ms: u128,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct FlightRecorder {
    capacity: usize,
    next_sequence: u64,
    records: VecDeque<EventRecord>,
}

impl FlightRecorder {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            next_sequence: 1,
            records: VecDeque::with_capacity(capacity.max(1)),
        }
    }

    pub fn record(&mut self, kind: impl Into<String>, detail: impl Into<String>) {
        if self.records.len() == self.capacity {
            self.records.pop_front();
        }

        let unix_ms = SystemTime::UNIX_EPOCH
            .elapsed()
            .map(|duration| duration.as_millis())
            .unwrap_or_default();

        self.records.push_back(EventRecord {
            sequence: self.next_sequence,
            unix_ms,
            kind: kind.into(),
            detail: detail.into(),
        });
        self.next_sequence = self.next_sequence.saturating_add(1);
    }

    pub fn snapshot(&self) -> Vec<EventRecord> {
        self.records.iter().cloned().collect()
    }
}

impl Default for FlightRecorder {
    fn default() -> Self {
        Self::new(2048)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_is_bounded() {
        let mut recorder = FlightRecorder::new(2);
        recorder.record("one", "1");
        recorder.record("two", "2");
        recorder.record("three", "3");

        let records = recorder.snapshot();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].kind, "two");
        assert_eq!(records[1].kind, "three");
    }
}
