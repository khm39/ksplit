//! The single read loop shared by profile, plan and split.

use std::time::{Duration, Instant};

use crate::cancel::{CancelToken, Progress};
use crate::config::{InvalidGuard, OnInvalid};
use crate::consumer::RecordConsumer;
use crate::error::{DataErrorKind, Error};
use crate::key::{KeyExtractor, KeyOutcome};
use crate::source::CsvSource;

pub(crate) struct RunOptions {
    pub progress_interval_bytes: u64,
    pub checkpoint_interval_bytes: Option<u64>,
    pub invalid_guard: Option<InvalidGuard>,
    pub on_invalid: OnInvalid,
    /// Records already accounted for before this run (resume).
    pub records_done: u64,
}

pub(crate) struct RunOutcome {
    pub records: u64,
    pub bytes_read: u64,
    pub elapsed: Duration,
}

pub(crate) fn run<C: RecordConsumer>(
    source: &mut CsvSource,
    extractor: &mut KeyExtractor,
    consumer: &mut C,
    options: &RunOptions,
    cancel: &CancelToken,
    mut progress: impl FnMut(&Progress),
) -> Result<RunOutcome, Error> {
    let started = Instant::now();
    let start_pos = source.position();
    let mut records = options.records_done;
    let mut next_progress = start_pos.saturating_add(options.progress_interval_bytes.max(1));
    let mut next_checkpoint = options
        .checkpoint_interval_bytes
        .map(|i| start_pos.saturating_add(i.max(1)));
    let mut guard = Guard::new(options.invalid_guard.as_ref(), records);

    while let Some(record) = source.next_record()? {
        records += 1;
        let outcome = extractor.extract(&record);
        if let KeyOutcome::Invalid(reason) = &outcome {
            if options.on_invalid == OnInvalid::Fail {
                return Err(Error::Data {
                    record: records,
                    byte_offset: record.offset,
                    column: Some(extractor.column()),
                    kind: DataErrorKind::InvalidKey(reason.to_string()),
                });
            }
        }
        let tripped = guard.observe(&outcome);
        let offset = record.offset;
        consumer.consume(&record, outcome)?;
        if let Some(window) = tripped {
            return Err(Error::Data {
                record: records,
                byte_offset: offset,
                column: Some(extractor.column()),
                kind: DataErrorKind::InvalidGuardTripped {
                    window,
                    last_reason: std::mem::take(&mut guard.last_reason),
                },
            });
        }

        let pos = source.position();
        if pos >= next_progress {
            progress(&Progress {
                bytes_read: pos,
                total_bytes: source.total_bytes(),
                records,
            });
            next_progress = pos.saturating_add(options.progress_interval_bytes.max(1));
        }
        if next_checkpoint.is_some_and(|cp| pos >= cp) {
            consumer.checkpoint(pos, records)?;
            next_checkpoint = Some(
                pos.saturating_add(options.checkpoint_interval_bytes.unwrap_or(u64::MAX).max(1)),
            );
        }
        if cancel.is_cancelled() {
            let checkpoint_written = consumer.on_cancel(pos, records)?;
            return Err(Error::Cancelled { checkpoint_written });
        }
    }
    let bytes_read = source.position();
    progress(&Progress {
        bytes_read,
        total_bytes: source.total_bytes(),
        records,
    });
    Ok(RunOutcome {
        records,
        bytes_read,
        elapsed: started.elapsed(),
    })
}

struct Guard {
    window: Option<u64>,
    seen: u64,
    last_reason: String,
}

impl Guard {
    fn new(guard: Option<&InvalidGuard>, records_done: u64) -> Self {
        // A resumed run has already seen valid records; never trip after resume.
        // A zero window means the guard is disabled.
        let window = guard
            .map(|g| g.window_records)
            .filter(|w| *w > 0 && records_done == 0);
        Self {
            window,
            seen: 0,
            last_reason: String::new(),
        }
    }

    /// Returns the window size when the guard trips on this record.
    fn observe(&mut self, outcome: &KeyOutcome) -> Option<u64> {
        let window = self.window?;
        let reason = match outcome {
            KeyOutcome::Valid(_) => {
                self.window = None;
                return None;
            }
            KeyOutcome::Invalid(reason) => reason,
        };
        self.seen += 1;
        if self.seen >= window {
            self.window = None;
            self.last_reason = reason.to_string();
            return Some(window);
        }
        None
    }
}
