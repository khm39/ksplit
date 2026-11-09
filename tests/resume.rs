use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ksplit::{
    CancelToken, CheckpointPolicy, CsvDialect, Encoding, Error, ExistingOutput, Input, KeySelector,
    KeySpec, KeyType, KeyValue, Mode, OnInvalid, ResumeRejection, SplitConfig,
};

const CHECKPOINT: &str = ".ksplit-checkpoint.json";

fn write_input(dir: &Path) -> PathBuf {
    let mut data = b"id,payload\n".to_vec();
    for i in 0..3000u32 {
        let payload = if i % 7 == 0 {
            format!("\"line\nbreak {i}\"")
        } else {
            format!("row-{i}")
        };
        let key = if i % 97 == 0 {
            "bad".to_string()
        } else {
            (i * 3 % 1000).to_string()
        };
        data.extend_from_slice(format!("{key},{payload}\n").as_bytes());
    }
    let path = dir.join("in.csv");
    fs::write(&path, data).unwrap();
    path
}

fn config(input: &Path, out: &Path, existing: ExistingOutput) -> SplitConfig {
    SplitConfig {
        input: Input::Path(input.to_path_buf()),
        output_dir: out.to_path_buf(),
        dialect: CsvDialect::default(),
        encoding: Encoding::Utf8,
        key: KeySpec {
            selector: KeySelector::Index(0),
            key_type: KeyType::Int,
            trim: true,
            invalid_guard: None,
        },
        ranges: ksplit::ranges_from_bounds(vec![KeyValue::Int(300), KeyValue::Int(600)]).unwrap(),
        mode: Mode::Split,
        on_invalid: OnInvalid::WriteToFile,
        existing,
        checkpoint: Some(CheckpointPolicy {
            interval_bytes: 4096,
        }),
        write_buffer_bytes: 1024,
        progress_interval_bytes: 1024,
    }
}

fn dir_contents(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "csv"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read(&p).unwrap(),
            )
        })
        .collect();
    entries.sort();
    entries
}

fn cancel_after(input: &Path, out: &Path, callbacks: usize) {
    let cancel = CancelToken::new();
    let mut seen = 0;
    let err = ksplit::split(config(input, out, ExistingOutput::Fail), &cancel, |_| {
        seen += 1;
        if seen == callbacks {
            cancel.cancel();
        }
    })
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Cancelled {
                checkpoint_written: true
            }
        ),
        "{err}"
    );
    assert!(out.join(CHECKPOINT).exists());
}

#[test]
fn cancelled_split_resumes_to_byte_identical_output() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path());
    let reference = tmp.path().join("ref");
    let full = ksplit::split(
        config(&input, &reference, ExistingOutput::Fail),
        &CancelToken::new(),
        |_| {},
    )
    .unwrap();

    for callbacks in [1, 5, 30] {
        let out = tmp.path().join(format!("out{callbacks}"));
        cancel_after(&input, &out, callbacks);

        let status = ksplit::read_checkpoint(&out)
            .unwrap()
            .expect("checkpoint present");
        assert!(status.input_unchanged);
        assert!(status.bytes_done > 0 && status.bytes_done < status.total_bytes);

        let resumed = ksplit::split(
            config(&input, &out, ExistingOutput::Resume),
            &CancelToken::new(),
            |_| {},
        )
        .unwrap();
        assert_eq!(resumed.resumed_from, Some(status.bytes_done));
        assert_eq!(
            dir_contents(&out),
            dir_contents(&reference),
            "callbacks={callbacks}"
        );
        assert_eq!(
            (resumed.records, resumed.unmatched, resumed.invalid),
            (full.records, full.unmatched, full.invalid)
        );
        assert_eq!(
            resumed
                .per_range
                .iter()
                .map(|r| r.records)
                .collect::<Vec<_>>(),
            full.per_range.iter().map(|r| r.records).collect::<Vec<_>>()
        );
        assert!(!out.join(CHECKPOINT).exists());
        assert!(ksplit::read_checkpoint(&out).unwrap().is_none());
    }
}

#[test]
fn resume_is_refused_when_anything_relevant_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path());
    let out = tmp.path().join("out");
    cancel_after(&input, &out, 3);

    let err = ksplit::split(
        config(&input, &out, ExistingOutput::Fail),
        &CancelToken::new(),
        |_| {},
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Config(ksplit::ConfigError::InterruptedRunExists(_))
        ),
        "{err}"
    );

    let mut changed = config(&input, &out, ExistingOutput::Resume);
    changed.mode = Mode::Extract;
    let err = ksplit::split(changed, &CancelToken::new(), |_| {}).unwrap_err();
    assert!(
        matches!(err, Error::ResumeRejected(ResumeRejection::ConfigMismatch)),
        "{err}"
    );

    let truncated = out.join("lt_300.csv");
    let len = fs::metadata(&truncated).unwrap().len();
    fs::OpenOptions::new()
        .write(true)
        .open(&truncated)
        .unwrap()
        .set_len(len - 1)
        .unwrap();
    let err = ksplit::split(
        config(&input, &out, ExistingOutput::Resume),
        &CancelToken::new(),
        |_| {},
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::ResumeRejected(ResumeRejection::OutputTruncated { .. })
        ),
        "{err}"
    );

    let file = fs::File::options().write(true).open(&input).unwrap();
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000))
        .unwrap();
    let err = ksplit::split(
        config(&input, &out, ExistingOutput::Resume),
        &CancelToken::new(),
        |_| {},
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::ResumeRejected(ResumeRejection::InputMismatch { field: "mtime", .. })
        ),
        "{err}"
    );
    assert!(
        !ksplit::read_checkpoint(&out)
            .unwrap()
            .unwrap()
            .input_unchanged
    );

    ksplit::split(
        config(&input, &out, ExistingOutput::Overwrite),
        &CancelToken::new(),
        |_| {},
    )
    .unwrap();
    assert!(!out.join(CHECKPOINT).exists());
}

#[test]
fn resume_rejects_missing_outputs_changed_input_size_and_incompatible_checkpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path());
    let out = tmp.path().join("out");
    cancel_after(&input, &out, 3);
    let resume = |cfg: SplitConfig| ksplit::split(cfg, &CancelToken::new(), |_| {}).unwrap_err();
    let checkpoint = fs::read_to_string(out.join(CHECKPOINT)).unwrap();

    for (needle, replacement, expected) in [
        (
            "\"tool_version\": \"",
            "\"tool_version\": \"0.0.0-",
            "written by ksplit 0.0.0-",
        ),
        (
            "\"schema_version\": 1",
            "\"schema_version\": 99",
            "schema_version 99",
        ),
        ("\"schema_version\"", "\"schema_versio\"", "corrupt"),
    ] {
        assert!(checkpoint.contains(needle), "{needle}");
        fs::write(
            out.join(CHECKPOINT),
            checkpoint.replacen(needle, replacement, 1),
        )
        .unwrap();
        let err = resume(config(&input, &out, ExistingOutput::Resume));
        assert!(matches!(err, Error::ResumeRejected(_)), "{err}");
        assert!(err.to_string().contains(expected), "{err}");
    }
    fs::write(out.join(CHECKPOINT), &checkpoint).unwrap();

    let removed = out.join("ge_600.csv");
    let bytes = fs::read(&removed).unwrap();
    fs::remove_file(&removed).unwrap();
    let err = resume(config(&input, &out, ExistingOutput::Resume));
    assert!(
        matches!(err, Error::ResumeRejected(ResumeRejection::OutputMissing(ref f)) if f == "ge_600.csv"),
        "{err}"
    );
    fs::write(&removed, bytes).unwrap();

    let mut grown = fs::read(&input).unwrap();
    grown.extend_from_slice(b"1,extra\n");
    fs::write(&input, grown).unwrap();
    let err = resume(config(&input, &out, ExistingOutput::Resume));
    assert!(
        matches!(
            err,
            Error::ResumeRejected(ResumeRejection::InputMismatch { field: "size", .. })
        ),
        "{err}"
    );
}

#[test]
fn resume_without_a_checkpoint_depends_on_whether_outputs_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path());

    let fresh = tmp.path().join("fresh");
    let report = ksplit::split(
        config(&input, &fresh, ExistingOutput::Resume),
        &CancelToken::new(),
        |_| {},
    )
    .unwrap();
    assert_eq!(report.resumed_from, None);
    assert_eq!(report.warnings.len(), 1);

    let err = ksplit::split(
        config(&input, &fresh, ExistingOutput::Resume),
        &CancelToken::new(),
        |_| {},
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::ResumeRejected(ResumeRejection::OutputsWithoutCheckpoint)
        ),
        "{err}"
    );
}
