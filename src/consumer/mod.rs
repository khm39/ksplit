//! Consumers receive each record with its key outcome.

mod router;

pub(crate) use router::{Counts, RouterConsumer};

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
