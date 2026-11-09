use std::fs;
use std::io::Write;

use ksplit::{
    CancelToken, ConfigError, DataErrorKind, Error, Inspection, Profile, Report, ResumeRejection,
};
use serde::Serialize;

use super::args::{Cli, Command, InspectArgs, SplitArgs, StatusArgs};
use super::convert;
use super::progress::{group, human, ProgressPrinter};

pub fn run(cli: Cli) -> Result<(), Error> {
    match cli.command {
        Command::Inspect(args) => inspect(args),
        Command::Split(args) => split(args),
        Command::Status(args) => status(args),
    }
}

fn inspect(args: InspectArgs) -> Result<(), Error> {
    let has_key = convert::key_specified(&args.key);
    let has_ranges = convert::ranges_specified(&args.range);
    if has_ranges && !has_key {
        return Err(ConfigError::Other("ranges need a key: add --key and --type".into()).into());
    }
    let quiet = args.csv.quiet;
    if !has_key {
        let inspection = ksplit::inspect(convert::inspect_config(&args)?)?;
        return emit(args.csv.json, &inspection, |out| {
            print_inspection(out, &inspection)
        });
    }
    if !quiet {
        eprintln!(
            "キー指定があるため全件を読みます (reading the whole input because a key was given)"
        );
    }
    let cancel = CancelToken::new();
    super::signal::install(cancel.clone(), false);
    let mut printer = ProgressPrinter::new(quiet);
    if has_ranges {
        let config = convert::inspect_plan_config(&args)?;
        let options = convert::profile_options(&args);
        let (plan, profile) =
            ksplit::plan_with_profile(config, options, &cancel, |p| printer.update(p))?;
        printer.finish();
        #[derive(Serialize)]
        struct Both<'a> {
            schema_version: u32,
            profile: &'a Profile,
            plan: &'a Report,
        }
        return emit(
            args.csv.json,
            &Both {
                schema_version: 1,
                profile: &profile,
                plan: &plan,
            },
            |out| {
                print_profile(out, &profile)?;
                print_report(out, &plan, "plan")
            },
        );
    }
    let profile = ksplit::profile(convert::profile_config(&args)?, &cancel, |p| {
        printer.update(p)
    })?;
    printer.finish();
    emit(args.csv.json, &profile, |out| print_profile(out, &profile))
}

fn split(args: SplitArgs) -> Result<(), Error> {
    let config = convert::split_config(&args)?;
    let warnings = config.validate()?;
    for w in &warnings {
        if !args.csv.quiet {
            eprintln!("warning: {w}");
        }
    }
    let cancel = CancelToken::new();
    super::signal::install(cancel.clone(), true);
    let mut printer = ProgressPrinter::new(args.csv.quiet);
    let report = ksplit::split(config, &cancel, |p| printer.update(p))?;
    printer.finish();
    for w in &report.warnings {
        eprintln!("warning: {w}");
    }
    if let Some(path) = &args.report {
        let json = serde_json::to_vec_pretty(&report).expect("report is serializable");
        fs::write(path, json).map_err(|source| Error::Io {
            path: Some(path.clone()),
            source,
        })?;
    }
    if args.csv.json {
        write_json(&report)?;
    } else if !args.csv.quiet {
        print_report(&mut std::io::stderr().lock(), &report, "split")?;
    }
    Ok(())
}

fn status(args: StatusArgs) -> Result<(), Error> {
    let status = ksplit::read_checkpoint(&args.out_dir)?;
    #[derive(Serialize)]
    struct Out<'a> {
        schema_version: u32,
        checkpoint: Option<&'a ksplit::CheckpointStatus>,
    }
    if args.json {
        return write_json(&Out {
            schema_version: 1,
            checkpoint: status.as_ref(),
        });
    }
    let mut out = std::io::stdout().lock();
    let Some(s) = status else {
        writeln!(out, "no checkpoint in {}", args.out_dir.display())?;
        return Ok(());
    };
    writeln!(
        out,
        "interrupted run saved at {} (ksplit {})",
        s.saved_at, s.tool_version
    )?;
    writeln!(
        out,
        "input: {}{}",
        s.input_path.display(),
        if s.input_unchanged {
            ""
        } else {
            "  (CHANGED since the checkpoint)"
        }
    )?;
    if let Some(change) = &s.input_change {
        writeln!(out, "  {change}")?;
    }
    writeln!(
        out,
        "progress: {} / {} ({:.0}%), {} records",
        human(s.bytes_done),
        human(s.total_bytes),
        pct(s.bytes_done, s.total_bytes),
        group(s.records_done)
    )?;
    for r in &s.per_range {
        writeln!(out, "  {:<30} {:>14}", r.name, group(r.records))?;
    }
    if let Some(n) = s.unmatched {
        writeln!(out, "  {:<30} {:>14}", "_unmatched", group(n))?;
    }
    if let Some(n) = s.invalid {
        writeln!(out, "  {:<30} {:>14}", "_invalid", group(n))?;
    }
    Ok(())
}

fn emit<T: Serialize>(
    json: bool,
    value: &T,
    text: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
) -> Result<(), Error> {
    if json {
        write_json(value)
    } else {
        text(&mut std::io::stdout().lock())?;
        Ok(())
    }
}

fn write_json<T: Serialize>(value: &T) -> Result<(), Error> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value).map_err(std::io::Error::other)?;
    writeln!(out)?;
    Ok(())
}

fn pct(done: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        done as f64 * 100.0 / total as f64
    }
}

fn print_inspection(out: &mut dyn Write, i: &Inspection) -> std::io::Result<()> {
    writeln!(
        out,
        "bom: {}  line ending: {:?}  columns: {}  sampled records: {}",
        if i.has_bom { "yes" } else { "no" },
        i.line_ending,
        i.column_count,
        i.sampled_records
    )?;
    for c in &i.columns {
        let candidates: Vec<String> = c.type_candidates.iter().map(describe_type).collect();
        match &c.name {
            Some(name) => writeln!(out, "\n#{} {name}", c.index)?,
            None => writeln!(out, "\n#{}", c.index)?,
        }
        if !candidates.is_empty() {
            writeln!(out, "  type candidates: {}", candidates.join(", "))?;
        }
        if c.undecodable > 0 {
            writeln!(out, "  {} sample(s) could not be decoded", c.undecodable)?;
        }
        let shown: Vec<&str> = c.samples.iter().take(5).map(String::as_str).collect();
        writeln!(out, "  samples: {shown:?}")?;
    }
    Ok(())
}

fn describe_type(t: &ksplit::KeyType) -> String {
    match t {
        ksplit::KeyType::Int => "int".into(),
        ksplit::KeyType::Decimal { scale } => format!("decimal --scale {scale}"),
        ksplit::KeyType::Date { format } => format!("date --format {format}"),
        ksplit::KeyType::DateTime { format, .. } => format!("datetime --format {format}"),
        ksplit::KeyType::String => "string".into(),
    }
}

fn print_profile(out: &mut dyn Write, p: &Profile) -> std::io::Result<()> {
    writeln!(
        out,
        "records: {}  invalid: {}  read: {} in {:.1}s",
        group(p.records),
        group(p.invalid),
        human(p.bytes_read),
        p.elapsed.as_secs_f64()
    )?;
    match (&p.min, &p.max) {
        (Some(min), Some(max)) => writeln!(out, "min: {min}  max: {max}")?,
        _ => writeln!(out, "no valid key values")?,
    }
    if let Some(h) = &p.histogram {
        writeln!(
            out,
            "\nhistogram (approximate, from {} sampled values):",
            group(h.sampled)
        )?;
        let widest = h
            .bins
            .iter()
            .map(|b| b.estimated_count)
            .max()
            .unwrap_or(0)
            .max(1);
        for b in &h.bins {
            let bar = "#".repeat((b.estimated_count * 40 / widest) as usize);
            writeln!(
                out,
                "  [{} .. {}) {:>14} {bar}",
                b.lo,
                b.hi,
                group(b.estimated_count)
            )?;
        }
    }
    if let Some(bounds) = &p.suggested_bounds {
        let text: Vec<String> = bounds.iter().map(|b| b.canonical()).collect();
        writeln!(out, "\nsuggested --bounds {}", text.join(","))?;
    }
    Ok(())
}

fn print_report(out: &mut dyn Write, r: &Report, verb: &str) -> std::io::Result<()> {
    let secs = r.elapsed.as_secs_f64();
    let rate = if secs > 0.0 {
        r.bytes_read as f64 / secs / 1e6
    } else {
        0.0
    };
    writeln!(
        out,
        "\n{verb} complete: {} records, {} in {secs:.1}s ({rate:.0} MB/s){}",
        group(r.records),
        human(r.bytes_read),
        r.resumed_from
            .map(|o| format!(", resumed from byte {o}"))
            .unwrap_or_default()
    )?;
    writeln!(out, "  {:<30} {:>14}  file", "range", "records")?;
    for row in &r.per_range {
        writeln!(
            out,
            "  {:<30} {:>14}  {}",
            row.name,
            group(row.records),
            row.path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        )?;
    }
    writeln!(out, "  {:<30} {:>14}", "_unmatched", group(r.unmatched))?;
    writeln!(out, "  {:<30} {:>14}", "_invalid", group(r.invalid))?;
    Ok(())
}

pub fn hints(err: &Error) -> Vec<String> {
    match err {
        Error::Data { byte_offset, kind: DataErrorKind::RecordTooLarge { unclosed_quote: true }, .. } => {
            vec![format!("a quote may be unclosed; the record starts at byte offset {byte_offset}. If the file uses CR-only line endings, convert it first.")]
        }
        Error::Config(ConfigError::InterruptedRunExists(_)) => vec!["add --resume to continue, or --overwrite to start again".into()],
        Error::ResumeRejected(ResumeRejection::OutputsWithoutCheckpoint) => vec!["the previous run stopped before its first checkpoint, or the files belong to another run; use --overwrite".into()],
        Error::ResumeRejected(_) => vec!["resume needs the same input file and the same key/range options; use --overwrite to start again".into()],
        Error::Cancelled { checkpoint_written: true } => vec!["run the same command with --resume to continue".into()],
        _ => Vec::new(),
    }
}
