use std::fs;
use std::path::Path;

use ksplit::{
    Bound, CancelToken, CsvDialect, DataErrorKind, Encoding, Error, ExistingOutput, Input,
    InvalidGuard, KeySelector, KeySpec, KeyType, KeyValue, Mode, OnInvalid, Range, SplitConfig,
};

const INPUT: &[u8] =
    b"\xEF\xBB\xBFid,name\r\n5,a\r\n25,b\r\n15,gap\r\nx,bad\r\n\"7\",\"multi\nline\"\r\n9,last";

fn write_input(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("in.csv");
    fs::write(&path, INPUT).unwrap();
    path
}

fn int(v: i64) -> Bound {
    Bound::Value(KeyValue::Int(v))
}

fn config(input: Input, out: &Path) -> SplitConfig {
    SplitConfig {
        input,
        output_dir: out.to_path_buf(),
        dialect: CsvDialect::default(),
        encoding: Encoding::Utf8,
        key: KeySpec {
            selector: KeySelector::Name("id".into()),
            key_type: KeyType::Int,
            trim: true,
            invalid_guard: None,
        },
        ranges: vec![
            Range {
                name: "a".into(),
                lo: int(0),
                hi: int(10),
            },
            Range {
                name: "b".into(),
                lo: int(20),
                hi: int(30),
            },
        ],
        mode: Mode::Split,
        on_invalid: OnInvalid::WriteToFile,
        existing: ExistingOutput::Fail,
        checkpoint: None,
        write_buffer_bytes: 8,
        progress_interval_bytes: 1,
    }
}

fn read(dir: &Path, name: &str) -> Vec<u8> {
    fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn split_writes_byte_transparent_outputs_and_plan_reports_the_same_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path());
    let out = tmp.path().join("out");
    let mut progress = Vec::new();
    let report = ksplit::split(
        config(Input::Path(input.clone()), &out),
        &CancelToken::new(),
        |p| progress.push(*p),
    )
    .unwrap();

    assert_eq!(
        read(&out, "a.csv"),
        b"\xEF\xBB\xBFid,name\r\n5,a\r\n\"7\",\"multi\nline\"\r\n9,last"
    );
    assert_eq!(read(&out, "b.csv"), b"\xEF\xBB\xBFid,name\r\n25,b\r\n");
    assert_eq!(
        read(&out, "_unmatched.csv"),
        b"\xEF\xBB\xBFid,name\r\n15,gap\r\n"
    );
    assert_eq!(
        read(&out, "_invalid.csv"),
        b"\xEF\xBB\xBFid,name\r\nx,bad\r\n"
    );
    assert!(!out.join(".ksplit-checkpoint.json").exists());

    let counts: Vec<_> = report
        .per_range
        .iter()
        .map(|r| (r.name.as_str(), r.records))
        .collect();
    assert_eq!(counts, [("a", 3), ("b", 1)]);
    assert_eq!(
        (report.unmatched, report.invalid, report.records),
        (1, 1, 6)
    );
    assert_eq!(report.bytes_read, INPUT.len() as u64);
    assert_eq!(
        report.per_range[0].path.as_deref(),
        Some(out.join("a.csv").as_path())
    );
    assert_eq!(
        progress.last().map(|p| (p.bytes_read, p.total_bytes)),
        Some((INPUT.len() as u64, Some(INPUT.len() as u64)))
    );

    let plan = ksplit::plan(
        config(Input::Path(input), &tmp.path().join("plan")),
        &CancelToken::new(),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        plan.per_range.iter().map(|r| r.records).collect::<Vec<_>>(),
        [3, 1]
    );
    assert_eq!((plan.unmatched, plan.invalid), (1, 1));
    assert!(plan.per_range.iter().all(|r| r.path.is_none()));
    assert!(!tmp.path().join("plan").exists());
}

#[test]
fn extract_mode_drops_unmatched_rows_and_reads_from_stdin_like_readers() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    let mut cfg = config(Input::Reader(Box::new(std::io::Cursor::new(INPUT))), &out);
    cfg.mode = Mode::Extract;
    let report = ksplit::split(cfg, &CancelToken::new(), |_| {}).unwrap();
    assert!(!out.join("_unmatched.csv").exists());
    assert_eq!(read(&out, "b.csv"), b"\xEF\xBB\xBFid,name\r\n25,b\r\n");
    assert_eq!(
        (report.unmatched, report.invalid, report.records),
        (0, 1, 6)
    );
}

#[test]
fn existing_outputs_are_refused_unless_overwrite_is_requested() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path());
    let out = tmp.path().join("out");
    ksplit::split(
        config(Input::Path(input.clone()), &out),
        &CancelToken::new(),
        |_| {},
    )
    .unwrap();

    let err = ksplit::split(
        config(Input::Path(input.clone()), &out),
        &CancelToken::new(),
        |_| {},
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::Config(ksplit::ConfigError::OutputExists(_))),
        "{err}"
    );

    let mut cfg = config(Input::Path(input), &out);
    cfg.existing = ExistingOutput::Overwrite;
    cfg.ranges.truncate(1);
    ksplit::split(cfg, &CancelToken::new(), |_| {}).unwrap();
    assert!(
        out.join("b.csv").exists(),
        "files from the previous run outside the new config are left alone"
    );
    assert_eq!(
        read(&out, "_unmatched.csv"),
        b"\xEF\xBB\xBFid,name\r\n25,b\r\n15,gap\r\n"
    );
}

#[test]
fn strict_mode_and_invalid_guard_stop_with_data_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path());

    let mut cfg = config(Input::Path(input.clone()), &tmp.path().join("strict"));
    cfg.on_invalid = OnInvalid::Fail;
    match ksplit::split(cfg, &CancelToken::new(), |_| {}).unwrap_err() {
        Error::Data {
            record: 4,
            column: Some(0),
            kind: DataErrorKind::InvalidKey(_),
            ..
        } => {}
        other => panic!("{other:?}"),
    }

    let mut cfg = config(Input::Path(input), &tmp.path().join("guard"));
    cfg.key.selector = KeySelector::Name("name".into());
    cfg.key.invalid_guard = Some(InvalidGuard { window_records: 2 });
    match ksplit::split(cfg, &CancelToken::new(), |_| {}).unwrap_err() {
        Error::Data {
            record: 2,
            kind:
                DataErrorKind::InvalidGuardTripped {
                    window: 2,
                    last_reason,
                },
            ..
        } => assert!(last_reason.contains("\"b\""), "{last_reason}"),
        other => panic!("{other:?}"),
    }

    let mut cfg = config(
        Input::Path(write_input(tmp.path())),
        &tmp.path().join("guard0"),
    );
    cfg.key.selector = KeySelector::Name("name".into());
    cfg.key.invalid_guard = Some(InvalidGuard { window_records: 0 });
    let report = ksplit::split(cfg, &CancelToken::new(), |_| {}).unwrap();
    assert_eq!(report.invalid, 6, "a zero window disables the guard");
}

#[test]
fn configuration_problems_are_reported_before_any_output_is_created() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path());
    let out = tmp.path().join("out");

    let mut cfg = config(Input::Path(input.clone()), &out);
    cfg.key.selector = KeySelector::Name("nope".into());
    let err = ksplit::split(cfg, &CancelToken::new(), |_| {}).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Config(ksplit::ConfigError::KeyColumnNotFound(..))
        ),
        "{err}"
    );

    let mut cfg = config(Input::Path(input.clone()), &out);
    cfg.existing = ExistingOutput::Resume;
    let err = ksplit::split(cfg, &CancelToken::new(), |_| {}).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Config(ksplit::ConfigError::ResumeRequiresCheckpoint)
        ),
        "{err}"
    );
    let mut cfg = config(Input::Path(input.clone()), &out);
    cfg.existing = ExistingOutput::Resume;
    assert!(
        ksplit::plan(cfg, &CancelToken::new(), |_| {}).is_ok(),
        "plan never resumes"
    );

    let mut cfg = config(Input::Path(input), &out);
    cfg.encoding = Encoding::ShiftJis;
    cfg.dialect.delimiter = b'|';
    let err = ksplit::split(cfg, &CancelToken::new(), |_| {}).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Config(ksplit::ConfigError::DelimiterUnsafeForEncoding(b'|'))
        ),
        "{err}"
    );
    assert!(!out.exists());
}
