use std::path::Path;

use ksplit::{
    Bound, CheckpointPolicy, ConfigError, CsvDialect, Encoding, Error, ExistingOutput, Input,
    InspectConfig, InvalidGuard, KeySelector, KeySpec, KeyType, Mode, OnInvalid, ProfileConfig,
    ProfileOptions, Range, SplitConfig,
};

use super::args::{
    CsvOpts, EncodingArg, InspectArgs, KeyOpts, KeyTypeArg, ModeArg, RangeOpts, SplitArgs,
};

fn usage(msg: impl Into<String>) -> Error {
    ConfigError::Other(msg.into()).into()
}

pub fn input(text: &str) -> Input {
    if text == "-" {
        Input::Reader(Box::new(std::io::stdin()))
    } else {
        Input::Path(Path::new(text).to_path_buf())
    }
}

pub fn encoding(arg: EncodingArg) -> Encoding {
    match arg {
        EncodingArg::Utf8 => Encoding::Utf8,
        EncodingArg::ShiftJis => Encoding::ShiftJis,
    }
}

pub fn dialect(opts: &CsvOpts) -> Result<CsvDialect, Error> {
    let delimiter = match opts.delimiter.as_str() {
        "\\t" | "tab" => b'\t',
        s if s.len() == 1 => s.as_bytes()[0],
        _ => return Err(ConfigError::InvalidDelimiter.into()),
    };
    Ok(CsvDialect {
        delimiter,
        has_header: !opts.no_header,
        max_record_bytes: opts.max_record_size as usize,
    })
}

pub fn key_specified(opts: &KeyOpts) -> bool {
    opts.key.is_some() || opts.r#type.is_some()
}

pub fn key_spec(opts: &KeyOpts, has_header: bool) -> Result<KeySpec, Error> {
    let (Some(key), Some(kind)) = (&opts.key, opts.r#type) else {
        return Err(usage("--key and --type must be given together"));
    };
    let selector = match key.strip_prefix('#') {
        Some(n) => KeySelector::Index(
            n.parse()
                .map_err(|_| usage(format!("invalid column number {key:?}")))?,
        ),
        None if has_header => KeySelector::Name(key.clone()),
        None => return Err(ConfigError::KeyNameRequiresHeader.into()),
    };
    let needs = |name: &str, present: bool| {
        if present {
            Ok(())
        } else {
            Err(usage(format!(
                "--{name} is required for --type {}",
                kind_name(kind)
            )))
        }
    };
    let rejects = |name: &str, present: bool| {
        if present {
            Err(usage(format!(
                "--{name} does not apply to --type {}",
                kind_name(kind)
            )))
        } else {
            Ok(())
        }
    };
    let key_type = match kind {
        KeyTypeArg::Int | KeyTypeArg::String => {
            rejects("scale", opts.scale.is_some())?;
            rejects("format", opts.format.is_some())?;
            rejects("tz", opts.tz.is_some())?;
            if kind == KeyTypeArg::Int {
                KeyType::Int
            } else {
                KeyType::String
            }
        }
        KeyTypeArg::Decimal => {
            needs("scale", opts.scale.is_some())?;
            rejects("format", opts.format.is_some())?;
            rejects("tz", opts.tz.is_some())?;
            KeyType::Decimal {
                scale: opts.scale.unwrap(),
            }
        }
        KeyTypeArg::Date => {
            needs("format", opts.format.is_some())?;
            rejects("scale", opts.scale.is_some())?;
            rejects("tz", opts.tz.is_some())?;
            KeyType::Date {
                format: opts.format.clone().unwrap(),
            }
        }
        KeyTypeArg::Datetime => {
            needs("format", opts.format.is_some())?;
            rejects("scale", opts.scale.is_some())?;
            KeyType::DateTime {
                format: opts.format.clone().unwrap(),
                default_tz: opts.tz.as_deref().map(str::parse).transpose()?,
            }
        }
    };
    key_type.validate()?;
    let invalid_guard = (!opts.no_invalid_guard).then_some(InvalidGuard {
        window_records: opts.invalid_guard,
    });
    Ok(KeySpec {
        selector,
        key_type,
        trim: !opts.no_trim,
        invalid_guard,
    })
}

fn kind_name(kind: KeyTypeArg) -> &'static str {
    match kind {
        KeyTypeArg::Int => "int",
        KeyTypeArg::Decimal => "decimal",
        KeyTypeArg::Date => "date",
        KeyTypeArg::Datetime => "datetime",
        KeyTypeArg::String => "string",
    }
}

pub fn ranges_specified(opts: &RangeOpts) -> bool {
    opts.bounds.is_some() || !opts.range.is_empty()
}

pub fn ranges(opts: &RangeOpts, key_type: &KeyType) -> Result<Vec<Range>, Error> {
    let parse = |text: &str| {
        key_type
            .parse(text.trim())
            .map_err(|e| ConfigError::InvalidBound(text.to_string(), e.to_string()))
    };
    if let Some(bounds) = &opts.bounds {
        let values = bounds
            .iter()
            .map(|b| parse(b))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(ksplit::ranges_from_bounds(values)?);
    }
    if opts.range.is_empty() {
        return Err(ConfigError::NoRanges.into());
    }
    opts.range
        .iter()
        .map(|spec| {
            let (name, span) = spec
                .split_once('=')
                .ok_or_else(|| usage(format!("range {spec:?} must look like NAME=LO..HI")))?;
            let (lo, hi) = span
                .split_once("..")
                .ok_or_else(|| usage(format!("range {spec:?} must look like NAME=LO..HI")))?;
            if name.is_empty() {
                return Err(usage(format!("range {spec:?} has an empty name")));
            }
            let bound = |text: &str| -> Result<Bound, Error> {
                Ok(if text.is_empty() {
                    Bound::Unbounded
                } else {
                    Bound::Value(parse(text)?)
                })
            };
            Ok(Range {
                name: name.to_string(),
                lo: bound(lo)?,
                hi: bound(hi)?,
            })
        })
        .collect()
}

pub fn mode(arg: ModeArg) -> Mode {
    match arg {
        ModeArg::Split => Mode::Split,
        ModeArg::Extract => Mode::Extract,
    }
}

pub fn split_config(args: &SplitArgs) -> Result<SplitConfig, Error> {
    let dialect = dialect(&args.csv)?;
    let key = key_spec(&args.key, dialect.has_header)?;
    let ranges = ranges(&args.range, &key.key_type)?;
    let existing = match (args.resume, args.overwrite) {
        (true, _) => ExistingOutput::Resume,
        (_, true) => ExistingOutput::Overwrite,
        _ => ExistingOutput::Fail,
    };
    Ok(SplitConfig {
        input: input(&args.input),
        output_dir: args.out_dir.clone(),
        dialect,
        encoding: encoding(args.csv.encoding),
        key,
        ranges,
        mode: mode(args.range.mode),
        on_invalid: if args.strict {
            OnInvalid::Fail
        } else {
            OnInvalid::WriteToFile
        },
        existing,
        checkpoint: Some(CheckpointPolicy {
            interval_bytes: args.checkpoint_interval,
        }),
        write_buffer_bytes: args.write_buffer as usize,
        progress_interval_bytes: 16 << 20,
    })
}

pub fn inspect_config(args: &InspectArgs) -> Result<InspectConfig, Error> {
    Ok(InspectConfig {
        input: input(&args.input),
        dialect: dialect(&args.csv)?,
        encoding: encoding(args.csv.encoding),
        sample_rows: args.sample,
    })
}

pub fn profile_options(args: &InspectArgs) -> ProfileOptions {
    ProfileOptions {
        bins: args.bins,
        sample_size: args.sample_size,
        suggest: args.suggest,
        seed: None,
    }
}

pub fn profile_config(args: &InspectArgs) -> Result<ProfileConfig, Error> {
    let dialect = dialect(&args.csv)?;
    let key = key_spec(&args.key, dialect.has_header)?;
    let options = profile_options(args);
    Ok(ProfileConfig {
        input: input(&args.input),
        dialect,
        encoding: encoding(args.csv.encoding),
        key,
        bins: options.bins,
        sample_size: options.sample_size,
        suggest: options.suggest,
        seed: None,
        progress_interval_bytes: 16 << 20,
    })
}

/// `inspect` with key and ranges behaves like `plan`; this builds that config.
pub fn inspect_plan_config(args: &InspectArgs) -> Result<SplitConfig, Error> {
    let dialect = dialect(&args.csv)?;
    let key = key_spec(&args.key, dialect.has_header)?;
    let ranges = ranges(&args.range, &key.key_type)?;
    Ok(SplitConfig {
        input: input(&args.input),
        output_dir: Path::new(".").to_path_buf(),
        dialect,
        encoding: encoding(args.csv.encoding),
        key,
        ranges,
        mode: mode(args.range.mode),
        on_invalid: if args.strict {
            OnInvalid::Fail
        } else {
            OnInvalid::WriteToFile
        },
        existing: ExistingOutput::Fail,
        checkpoint: None,
        write_buffer_bytes: 0,
        progress_interval_bytes: 16 << 20,
    })
}
