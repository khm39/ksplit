//! Destinations for routed records.

mod count;
pub(crate) mod file;

pub(crate) use count::CountSink;
pub(crate) use file::{FileSink, OutputLength};

use std::path::PathBuf;

use crate::error::Error;
use crate::source::RawRecord;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Destination {
    Range(usize),
    Unmatched,
    Invalid,
}

pub(crate) trait Sink {
    fn accept(&mut self, record: &RawRecord<'_>, dest: Destination) -> Result<(), Error>;
    /// Flushes and fsyncs every output; returns their current lengths.
    fn persist(&mut self) -> Result<Vec<OutputLength>, Error>;
    /// Finishes writing; returns each range's output path (None when nothing was written).
    fn finish(&mut self) -> Result<Vec<Option<PathBuf>>, Error>;
}
