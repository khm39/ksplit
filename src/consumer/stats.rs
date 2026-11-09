use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use super::RecordConsumer;
use crate::error::Error;
use crate::key::{KeyOutcome, KeyValue};
use crate::source::RawRecord;

/// Exact count/min/max plus a reservoir sample of key values; used by `profile`.
pub(crate) struct StatsCollector {
    pub valid: u64,
    pub invalid: u64,
    pub min: Option<KeyValue>,
    pub max: Option<KeyValue>,
    reservoir: Option<Reservoir>,
}

struct Reservoir {
    capacity: usize,
    items: Vec<KeyValue>,
    seen: u64,
    rng: StdRng,
}

impl StatsCollector {
    pub(crate) fn new(sample_size: Option<usize>, seed: Option<u64>) -> Self {
        let reservoir = sample_size.filter(|n| *n > 0).map(|capacity| Reservoir {
            capacity,
            items: Vec::with_capacity(capacity.min(1 << 16)),
            seen: 0,
            rng: seed.map_or_else(StdRng::from_entropy, StdRng::seed_from_u64),
        });
        Self { valid: 0, invalid: 0, min: None, max: None, reservoir }
    }

    /// The sample so far and how many valid values it was drawn from.
    pub(crate) fn sample(&self) -> Option<(&[KeyValue], u64)> {
        self.reservoir.as_ref().map(|r| (r.items.as_slice(), r.seen))
    }
}

impl RecordConsumer for StatsCollector {
    fn consume(&mut self, _record: &RawRecord<'_>, key: KeyOutcome) -> Result<(), Error> {
        let value = match key {
            KeyOutcome::Invalid(_) => {
                self.invalid += 1;
                return Ok(());
            }
            KeyOutcome::Valid(v) => v,
        };
        self.valid += 1;
        if self.min.as_ref().is_none_or(|m| &value < m) {
            self.min = Some(value.clone());
        }
        if self.max.as_ref().is_none_or(|m| &value > m) {
            self.max = Some(value.clone());
        }
        if let Some(r) = &mut self.reservoir {
            if r.items.len() < r.capacity {
                r.items.push(value);
            } else {
                let slot = r.rng.gen_range(0..=r.seen);
                if (slot as usize) < r.capacity {
                    r.items[slot as usize] = value;
                }
            }
            r.seen += 1;
        }
        Ok(())
    }
}
