use super::RecordConsumer;
use crate::config::Mode;
use crate::error::Error;
use crate::key::KeyOutcome;
use crate::range::{Routed, Router};
use crate::sink::{Destination, Sink};
use crate::source::RawRecord;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub per_range: Vec<u64>,
    pub unmatched: u64,
    pub invalid: u64,
}

/// Routes records to a sink; shared by `split` and `plan`.
pub(crate) struct RouterConsumer<S: Sink> {
    pub router: Router,
    pub mode: Mode,
    pub sink: S,
    pub counts: Counts,
    pub checkpoint: Option<Box<dyn FnMut(&mut S, &Counts, u64, u64) -> Result<(), Error>>>,
}

impl<S: Sink> RouterConsumer<S> {
    pub(crate) fn new(router: Router, mode: Mode, sink: S, counts: Counts) -> Self {
        Self { router, mode, sink, counts, checkpoint: None }
    }
}

impl<S: Sink> RecordConsumer for RouterConsumer<S> {
    fn consume(&mut self, record: &RawRecord<'_>, key: KeyOutcome) -> Result<(), Error> {
        let dest = match key {
            KeyOutcome::Invalid(_) => {
                self.counts.invalid += 1;
                Destination::Invalid
            }
            KeyOutcome::Valid(v) => match self.router.route(&v) {
                Routed::Range(i) => {
                    self.counts.per_range[i] += 1;
                    Destination::Range(i)
                }
                Routed::Unmatched if self.mode == Mode::Split => {
                    self.counts.unmatched += 1;
                    Destination::Unmatched
                }
                Routed::Unmatched => return Ok(()),
            },
        };
        self.sink.accept(record, dest)
    }

    fn checkpoint(&mut self, input_offset: u64, records: u64) -> Result<(), Error> {
        if let Some(cp) = &mut self.checkpoint {
            cp(&mut self.sink, &self.counts, input_offset, records)?;
        }
        Ok(())
    }

    fn on_cancel(&mut self, input_offset: u64, records: u64) -> Result<bool, Error> {
        self.checkpoint(input_offset, records)?;
        Ok(self.checkpoint.is_some())
    }
}
