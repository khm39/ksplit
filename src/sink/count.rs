use std::path::PathBuf;

use super::{Destination, OutputLength, Sink};
use crate::error::Error;
use crate::source::RawRecord;

/// Counts only; used by `plan`.
pub(crate) struct CountSink {
    ranges: usize,
}

impl CountSink {
    pub(crate) fn new(ranges: usize) -> Self {
        Self { ranges }
    }
}

impl Sink for CountSink {
    fn accept(&mut self, _record: &RawRecord<'_>, _dest: Destination) -> Result<(), Error> {
        Ok(())
    }

    fn persist(&mut self) -> Result<Vec<OutputLength>, Error> {
        Ok(Vec::new())
    }

    fn finish(&mut self) -> Result<Vec<Option<PathBuf>>, Error> {
        Ok(vec![None; self.ranges])
    }
}
