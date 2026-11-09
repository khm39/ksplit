//! Consumers receive each record with its key outcome.

mod router;
mod stats;

pub(crate) use router::{Counts, RouterConsumer};
pub(crate) use stats::StatsCollector;

use crate::error::Error;
use crate::key::KeyOutcome;
use crate::source::RawRecord;

pub(crate) trait RecordConsumer {
    fn consume(&mut self, record: &RawRecord<'_>, key: KeyOutcome) -> Result<(), Error>;
    /// Called at checkpoint intervals (record boundary). Default: nothing to persist.
    fn checkpoint(&mut self, _input_offset: u64, _records: u64) -> Result<(), Error> {
        Ok(())
    }
    /// Called on cancellation; returns whether a checkpoint was written.
    fn on_cancel(&mut self, _input_offset: u64, _records: u64) -> Result<bool, Error> {
        Ok(false)
    }
}

/// Feeds every record to two consumers; used when plan and profile share one pass.
pub(crate) struct Tee<A, B>(pub A, pub B);

impl<A: RecordConsumer, B: RecordConsumer> RecordConsumer for Tee<A, B> {
    fn consume(&mut self, record: &RawRecord<'_>, key: KeyOutcome) -> Result<(), Error> {
        self.0.consume(record, key.clone())?;
        self.1.consume(record, key)
    }
}
