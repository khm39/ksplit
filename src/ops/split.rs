use std::fs;

use crate::cancel::{CancelToken, Progress};
use crate::config::{ExistingOutput, OnInvalid, SplitConfig};
use crate::consumer::{Counts, RouterConsumer};
use crate::error::{ConfigError, Error};
use crate::pipeline::{self, RunOptions};
use crate::report::{RangeResult, Report, SCHEMA_VERSION};
use crate::sink::{CountSink, FileSink, Sink};
use crate::sink::file::Layout;

/// Routes every record to one output file per range.
pub fn split(config: SplitConfig, cancel: &CancelToken, progress: impl FnMut(&Progress)) -> Result<Report, Error> {
    let router = config.router()?;
    let SplitConfig { input, output_dir, dialect, encoding, key, mode, on_invalid, existing, checkpoint: _, write_buffer_bytes, progress_interval_bytes, .. } = config;
    let (mut source, mut extractor) = super::open_with_key(input, &dialect, encoding, &key)?;

    let layout = Layout { dir: &output_dir, range_names: router.output_names(), mode, write_invalid: on_invalid == OnInvalid::WriteToFile };
    let header = source.header().map(|h| h.to_vec()).unwrap_or_else(|| if source.has_bom() { b"\xEF\xBB\xBF".to_vec() } else { Vec::new() });
    let sink = match existing {
        ExistingOutput::Fail | ExistingOutput::Overwrite => {
            if existing == ExistingOutput::Overwrite {
                for path in layout.all_paths() {
                    if let Err(e) = fs::remove_file(&path) {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            return Err(Error::io(Some(&path), e));
                        }
                    }
                }
            }
            FileSink::create(&layout, &header, write_buffer_bytes, existing == ExistingOutput::Overwrite)?
        }
        ExistingOutput::Resume => return Err(ConfigError::Other("resume is not implemented yet".into()).into()),
    };

    let counts = Counts { per_range: vec![0; router.ranges().len()], ..Counts::default() };
    let mut consumer = RouterConsumer::new(router, mode, sink, counts);
    let options = RunOptions {
        progress_interval_bytes,
        checkpoint_interval_bytes: None,
        invalid_guard: key.invalid_guard.clone(),
        on_invalid,
        records_done: 0,
    };
    let outcome = pipeline::run(&mut source, &mut extractor, &mut consumer, &options, cancel, progress)?;
    let paths = consumer.sink.finish()?;
    Ok(report(&consumer, paths, outcome.records, outcome.bytes_read, outcome.elapsed, None))
}

/// Counts what `split` would write, without creating any output.
pub fn plan(config: SplitConfig, cancel: &CancelToken, progress: impl FnMut(&Progress)) -> Result<Report, Error> {
    let router = config.router()?;
    let SplitConfig { input, dialect, encoding, key, mode, on_invalid, progress_interval_bytes, .. } = config;
    let (mut source, mut extractor) = super::open_with_key(input, &dialect, encoding, &key)?;
    let counts = Counts { per_range: vec![0; router.ranges().len()], ..Counts::default() };
    let mut consumer = RouterConsumer::new(router, mode, CountSink::new(counts.per_range.len()), counts);
    let options = RunOptions { progress_interval_bytes, checkpoint_interval_bytes: None, invalid_guard: key.invalid_guard.clone(), on_invalid, records_done: 0 };
    let outcome = pipeline::run(&mut source, &mut extractor, &mut consumer, &options, cancel, progress)?;
    let paths = consumer.sink.finish()?;
    Ok(report(&consumer, paths, outcome.records, outcome.bytes_read, outcome.elapsed, None))
}

fn report<S: Sink>(consumer: &RouterConsumer<S>, paths: Vec<Option<std::path::PathBuf>>, records: u64, bytes_read: u64, elapsed: std::time::Duration, resumed_from: Option<u64>) -> Report {
    let per_range = consumer
        .router
        .ranges()
        .iter()
        .zip(paths)
        .zip(&consumer.counts.per_range)
        .map(|((r, path), &records)| RangeResult { name: r.name.clone(), path, records })
        .collect();
    Report { schema_version: SCHEMA_VERSION, per_range, unmatched: consumer.counts.unmatched, invalid: consumer.counts.invalid, records, bytes_read, elapsed, resumed_from }
}
