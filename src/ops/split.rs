use std::fs;

use crate::cancel::{CancelToken, Progress};
use crate::checkpoint::{self, Checkpoint, InputIdentity};
use crate::config::{ExistingOutput, Input, OnInvalid, SplitConfig};
use crate::consumer::{Counts, RecordConsumer, RouterConsumer};
use crate::error::{ConfigError, Error, ResumeRejection};
use crate::pipeline::{self, RunOptions};
use crate::range::Router;
use crate::report::{RangeResult, Report, SCHEMA_VERSION};
use crate::sink::file::Layout;
use crate::sink::{CountSink, FileSink, Sink};

/// Routes every record to one output file per range.
pub fn split(
    config: SplitConfig,
    cancel: &CancelToken,
    progress: impl FnMut(&Progress),
) -> Result<Report, Error> {
    let router = config.router()?;
    let config_hash = checkpoint::config_hash(&config, &router);
    let input_identity = match (&config.input, &config.checkpoint) {
        (Input::Path(p), Some(_)) => Some(InputIdentity::of(p)?),
        _ => None,
    };
    let SplitConfig {
        input,
        output_dir,
        dialect,
        encoding,
        key,
        mode,
        on_invalid,
        existing,
        checkpoint: policy,
        write_buffer_bytes,
        progress_interval_bytes,
        ..
    } = config;
    let (mut source, mut extractor) = super::open_with_key(input, &dialect, encoding, &key)?;

    let layout = Layout {
        dir: &output_dir,
        range_names: router.output_names(),
        mode,
        write_invalid: on_invalid == OnInvalid::WriteToFile,
    };
    let header = source.header().map(|h| h.to_vec()).unwrap_or_else(|| {
        if source.has_bom() {
            b"\xEF\xBB\xBF".to_vec()
        } else {
            Vec::new()
        }
    });
    let saved = checkpoint::read(&output_dir)?;
    let mut warnings = Vec::new();

    let (sink, counts, resumed) = match (existing, saved) {
        (ExistingOutput::Fail, Some(_)) => {
            return Err(ConfigError::InterruptedRunExists(output_dir.clone()).into())
        }
        (ExistingOutput::Fail, None) => (
            FileSink::create(&layout, &header, write_buffer_bytes, false)?,
            fresh_counts(&router),
            None,
        ),
        (ExistingOutput::Overwrite, _) => {
            for path in layout.all_paths() {
                remove_if_exists(&path)?;
            }
            checkpoint::remove(&output_dir)?;
            (
                FileSink::create(&layout, &header, write_buffer_bytes, true)?,
                fresh_counts(&router),
                None,
            )
        }
        (ExistingOutput::Resume, None) => {
            if layout.all_paths().iter().any(|p| p.exists()) {
                return Err(Error::ResumeRejected(
                    ResumeRejection::OutputsWithoutCheckpoint,
                ));
            }
            warnings.push("no checkpoint found; starting from the beginning".to_string());
            (
                FileSink::create(&layout, &header, write_buffer_bytes, false)?,
                fresh_counts(&router),
                None,
            )
        }
        (ExistingOutput::Resume, Some(saved)) => {
            saved
                .check_compatible(&config_hash)
                .map_err(Error::ResumeRejected)?;
            let identity = input_identity
                .as_ref()
                .ok_or(ConfigError::ResumeRequiresCheckpoint)?;
            identity
                .check_against(&saved.input)
                .map_err(Error::ResumeRejected)?;
            let sink = FileSink::reopen(
                &layout,
                |file| {
                    saved
                        .outputs
                        .iter()
                        .find(|e| e.file == file)
                        .map(|e| e.bytes)
                },
                write_buffer_bytes,
            )?;
            let counts = saved.counts();
            if counts.per_range.len() != router.ranges().len() {
                return Err(Error::ResumeRejected(ResumeRejection::CorruptCheckpoint(
                    "range count differs".into(),
                )));
            }
            source.seek(saved.input_offset, saved.records_done)?;
            (sink, counts, Some((saved.input_offset, saved.records_done)))
        }
    };

    let mut consumer = RouterConsumer::new(router, mode, sink, counts);
    let mut checkpoint_interval = None;
    if let (Some(policy), Some(identity)) = (policy, input_identity) {
        let dir = output_dir.clone();
        consumer.checkpoint = Some(Box::new(
            move |sink: &mut FileSink,
                  router: &Router,
                  counts: &Counts,
                  offset: u64,
                  records: u64| {
                let lengths = sink.persist()?;
                let entries = Checkpoint::entries(&lengths, router, counts);
                checkpoint::write(
                    &dir,
                    &Checkpoint::new(
                        config_hash.clone(),
                        identity.clone(),
                        offset,
                        records,
                        entries,
                    ),
                )
            },
        ));
        if resumed.is_none() {
            consumer.checkpoint(source.position(), 0)?;
        }
        checkpoint_interval = Some(policy.interval_bytes);
    }

    let options = RunOptions {
        progress_interval_bytes,
        checkpoint_interval_bytes: checkpoint_interval,
        invalid_guard: key.invalid_guard.clone(),
        on_invalid,
        records_done: resumed.map_or(0, |(_, r)| r),
    };
    let outcome = pipeline::run(
        &mut source,
        &mut extractor,
        &mut consumer,
        &options,
        cancel,
        progress,
    )?;
    let paths = consumer.sink.finish()?;
    checkpoint::remove(&output_dir)?;
    let mut report = report_for(
        &consumer,
        paths,
        outcome.records,
        outcome.bytes_read,
        outcome.elapsed,
    );
    report.resumed_from = resumed.map(|(o, _)| o);
    report.warnings = warnings;
    Ok(report)
}

/// Counts what `split` would write, without creating any output.
pub fn plan(
    config: SplitConfig,
    cancel: &CancelToken,
    progress: impl FnMut(&Progress),
) -> Result<Report, Error> {
    let router = config.router()?;
    let SplitConfig {
        input,
        dialect,
        encoding,
        key,
        mode,
        on_invalid,
        progress_interval_bytes,
        ..
    } = config;
    let (mut source, mut extractor) = super::open_with_key(input, &dialect, encoding, &key)?;
    let counts = fresh_counts(&router);
    let mut consumer =
        RouterConsumer::new(router, mode, CountSink::new(counts.per_range.len()), counts);
    let options = RunOptions {
        progress_interval_bytes,
        checkpoint_interval_bytes: None,
        invalid_guard: key.invalid_guard.clone(),
        on_invalid,
        records_done: 0,
    };
    let outcome = pipeline::run(
        &mut source,
        &mut extractor,
        &mut consumer,
        &options,
        cancel,
        progress,
    )?;
    let paths = consumer.sink.finish()?;
    Ok(report_for(
        &consumer,
        paths,
        outcome.records,
        outcome.bytes_read,
        outcome.elapsed,
    ))
}

fn fresh_counts(router: &Router) -> Counts {
    Counts {
        per_range: vec![0; router.ranges().len()],
        ..Counts::default()
    }
}

fn remove_if_exists(path: &std::path::Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(Some(path), e)),
    }
}

pub(crate) fn report_for<S: Sink>(
    consumer: &RouterConsumer<S>,
    paths: Vec<Option<std::path::PathBuf>>,
    records: u64,
    bytes_read: u64,
    elapsed: std::time::Duration,
) -> Report {
    let per_range = consumer
        .router
        .ranges()
        .iter()
        .zip(paths)
        .zip(&consumer.counts.per_range)
        .map(|((r, path), &records)| RangeResult {
            name: r.name.clone(),
            path,
            records,
        })
        .collect();
    Report {
        schema_version: SCHEMA_VERSION,
        per_range,
        unmatched: consumer.counts.unmatched,
        invalid: consumer.counts.invalid,
        records,
        bytes_read,
        elapsed,
        resumed_from: None,
        warnings: Vec::new(),
    }
}
