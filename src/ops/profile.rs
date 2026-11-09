use std::time::Duration;

use chrono::{DateTime, Days, NaiveDate, Utc};
use serde::Serialize;

use crate::cancel::{CancelToken, Progress};
use crate::config::{CsvDialect, Encoding, Input, KeySpec, OnInvalid, SplitConfig};
use crate::consumer::{RouterConsumer, StatsCollector, Tee};
use crate::error::Error;
use crate::key::KeyValue;
use crate::pipeline::{self, RunOptions};
use crate::report::{Report, SCHEMA_VERSION};
use crate::sink::CountSink;

#[derive(Debug)]
pub struct ProfileConfig {
    pub input: Input,
    pub dialect: CsvDialect,
    pub encoding: Encoding,
    pub key: KeySpec,
    pub on_invalid: OnInvalid,
    pub bins: usize,
    pub sample_size: usize,
    pub suggest: Option<usize>,
    pub seed: Option<u64>,
    pub progress_interval_bytes: u64,
}

/// The sampling-related part of `ProfileConfig`, for `plan_with_profile`.
#[derive(Clone, Debug)]
pub struct ProfileOptions {
    pub bins: usize,
    pub sample_size: usize,
    pub suggest: Option<usize>,
    pub seed: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HistogramBin {
    pub lo: KeyValue,
    pub hi: KeyValue,
    pub estimated_count: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Histogram {
    pub approximate: bool,
    pub sampled: u64,
    pub bins: Vec<HistogramBin>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Profile {
    pub schema_version: u32,
    pub records: u64,
    pub invalid: u64,
    pub min: Option<KeyValue>,
    pub max: Option<KeyValue>,
    pub histogram: Option<Histogram>,
    pub suggested_bounds: Option<Vec<KeyValue>>,
    pub bytes_read: u64,
    #[serde(serialize_with = "crate::report::secs")]
    pub elapsed: Duration,
}

/// Reads the whole input once and summarises the key column.
pub fn profile(
    config: ProfileConfig,
    cancel: &CancelToken,
    progress: impl FnMut(&Progress),
) -> Result<Profile, Error> {
    config.dialect.validate(config.encoding)?;
    config.key.key_type.validate()?;
    let options = ProfileOptions {
        bins: config.bins,
        sample_size: config.sample_size,
        suggest: config.suggest,
        seed: config.seed,
    };
    let (mut source, mut extractor) =
        super::open_with_key(config.input, &config.dialect, config.encoding, &config.key)?;
    let mut stats = new_collector(&config.key, &options);
    let run = RunOptions {
        progress_interval_bytes: config.progress_interval_bytes,
        checkpoint_interval_bytes: None,
        invalid_guard: config.key.invalid_guard.clone(),
        on_invalid: config.on_invalid,
        records_done: 0,
    };
    let outcome = pipeline::run(
        &mut source,
        &mut extractor,
        &mut stats,
        &run,
        cancel,
        progress,
    )?;
    Ok(finish(
        stats,
        &options,
        outcome.records,
        outcome.bytes_read,
        outcome.elapsed,
    ))
}

/// `plan` and `profile` in one pass over the input.
pub fn plan_with_profile(
    config: SplitConfig,
    options: ProfileOptions,
    cancel: &CancelToken,
    progress: impl FnMut(&Progress),
) -> Result<(Report, Profile), Error> {
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
    let counts = crate::consumer::Counts {
        per_range: vec![0; router.ranges().len()],
        ..Default::default()
    };
    let planner = RouterConsumer::new(router, mode, CountSink::new(counts.per_range.len()), counts);
    let mut tee = Tee(planner, new_collector(&key, &options));
    let run = RunOptions {
        progress_interval_bytes,
        checkpoint_interval_bytes: None,
        invalid_guard: key.invalid_guard.clone(),
        on_invalid,
        records_done: 0,
    };
    let outcome = pipeline::run(
        &mut source,
        &mut extractor,
        &mut tee,
        &run,
        cancel,
        progress,
    )?;
    let Tee(planner, stats) = tee;
    let report = super::split::report_for(
        &planner,
        vec![None; planner.counts.per_range.len()],
        outcome.records,
        outcome.bytes_read,
        outcome.elapsed,
    );
    Ok((
        report,
        finish(
            stats,
            &options,
            outcome.records,
            outcome.bytes_read,
            outcome.elapsed,
        ),
    ))
}

fn new_collector(key: &KeySpec, options: &ProfileOptions) -> StatsCollector {
    let sampling = !matches!(key.key_type, crate::key::KeyType::String);
    StatsCollector::new(sampling.then_some(options.sample_size), options.seed)
}

fn finish(
    stats: StatsCollector,
    options: &ProfileOptions,
    records: u64,
    bytes_read: u64,
    elapsed: Duration,
) -> Profile {
    let (histogram, suggested_bounds) = match (stats.sample(), &stats.min, &stats.max) {
        (Some((sample, _seen)), Some(min), Some(max)) if !sample.is_empty() => {
            let mut sorted = sample.to_vec();
            sorted.sort_by(|a, b| a.partial_cmp(b).expect("same key type"));
            (
                Some(histogram(&sorted, stats.valid, min, max, options.bins)),
                options.suggest.and_then(|n| suggest(&sorted, n)),
            )
        }
        _ => (None, None),
    };
    Profile {
        schema_version: SCHEMA_VERSION,
        records,
        invalid: stats.invalid,
        min: stats.min,
        max: stats.max,
        histogram,
        suggested_bounds,
        bytes_read,
        elapsed,
    }
}

fn histogram(
    sorted: &[KeyValue],
    valid: u64,
    min: &KeyValue,
    max: &KeyValue,
    bins: usize,
) -> Histogram {
    let sampled = sorted.len() as u64;
    let (lo, hi) = (scalar(min), scalar(max));
    let span = hi - lo + 1;
    let width = (span + bins.max(1) as i128 - 1) / bins.max(1) as i128;
    let bin_count = ((span + width - 1) / width) as usize;
    let mut counts = vec![0u64; bin_count];
    for v in sorted {
        let idx = ((scalar(v) - lo) / width) as usize;
        counts[idx.min(bin_count - 1)] += 1;
    }
    // Scale cumulative counts so that the estimates sum exactly to `valid`.
    let scaled = |cumulative: u64| -> u64 {
        if sampled == 0 {
            return 0;
        }
        ((cumulative as u128 * valid as u128 + sampled as u128 / 2) / sampled as u128) as u64
    };
    let (mut cumulative, mut assigned) = (0u64, 0u64);
    let bins = counts
        .iter()
        .enumerate()
        .map(|(i, &n)| {
            cumulative += n;
            let estimated_count = scaled(cumulative) - assigned;
            assigned += estimated_count;
            HistogramBin {
                lo: from_scalar(min, lo + i as i128 * width),
                hi: from_scalar(min, (lo + (i as i128 + 1) * width).min(hi)),
                estimated_count,
            }
        })
        .collect();
    Histogram {
        approximate: true,
        sampled,
        bins,
    }
}

fn suggest(sorted: &[KeyValue], n: usize) -> Option<Vec<KeyValue>> {
    if n < 2 || sorted.len() < n {
        return None;
    }
    let mut out: Vec<KeyValue> = Vec::new();
    for k in 1..n {
        let v = &sorted[k * sorted.len() / n];
        if out.last() != Some(v) {
            out.push(v.clone());
        }
    }
    Some(out)
}

fn scalar(v: &KeyValue) -> i128 {
    match v {
        KeyValue::Int(i) => *i as i128,
        KeyValue::Decimal { units, .. } => *units,
        KeyValue::Date(d) => d
            .signed_duration_since(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
            .num_days() as i128,
        KeyValue::DateTime(dt) => dt.timestamp() as i128,
        KeyValue::String(_) => unreachable!("strings are not sampled"),
    }
}

fn from_scalar(like: &KeyValue, s: i128) -> KeyValue {
    match like {
        KeyValue::Int(_) => KeyValue::Int(s.clamp(i64::MIN as i128, i64::MAX as i128) as i64),
        KeyValue::Decimal { scale, .. } => KeyValue::Decimal {
            units: s,
            scale: *scale,
        },
        KeyValue::Date(_) => {
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
            let days = s.clamp(-(1 << 40), 1 << 40);
            KeyValue::Date(
                if days >= 0 {
                    epoch.checked_add_days(Days::new(days as u64))
                } else {
                    epoch.checked_sub_days(Days::new((-days) as u64))
                }
                .unwrap_or(epoch),
            )
        }
        KeyValue::DateTime(_) => KeyValue::DateTime(
            DateTime::<Utc>::from_timestamp(s.clamp(i64::MIN as i128, i64::MAX as i128) as i64, 0)
                .unwrap_or_default(),
        ),
        KeyValue::String(_) => unreachable!("strings are not sampled"),
    }
}
