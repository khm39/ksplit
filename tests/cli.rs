use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const INPUT: &[u8] =
    b"id,day,note\n5,2024-01-05,a\n25,2024-02-01,b\n15,2024-03-01,c\nx,2024-04-01,bad\n";

fn ksplit(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ksplit"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

fn setup() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("in.csv"), INPUT).unwrap();
    tmp
}

#[test]
fn inspect_escalates_from_head_to_profile_to_plan_with_more_arguments() {
    let tmp = setup();
    let head = json(&ksplit(tmp.path(), &["inspect", "in.csv", "--json"]));
    assert_eq!(head["columns"][1]["name"], "day");
    assert_eq!(
        head["columns"][1]["type_candidates"][0]["format"],
        "%Y-%m-%d"
    );

    let out = ksplit(
        tmp.path(),
        &[
            "inspect", "in.csv", "--json", "--key", "id", "--type", "int",
        ],
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("全件"),
        "announces the full scan"
    );
    let profile = json(&out);
    assert_eq!(
        (&profile["records"], &profile["invalid"], &profile["min"]),
        (&4.into(), &1.into(), &"5".into())
    );

    let both = json(&ksplit(
        tmp.path(),
        &[
            "inspect", "in.csv", "--json", "--quiet", "--key", "id", "--type", "int", "--bounds",
            "10,20",
        ],
    ));
    assert_eq!(both["profile"]["max"], "25");
    let counts: Vec<_> = both["plan"]["per_range"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap().to_string(),
                r["records"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        counts,
        [
            ("lt_10".to_string(), 1),
            ("10_20".to_string(), 1),
            ("ge_20".to_string(), 1)
        ]
    );

    let out = ksplit(tmp.path(), &["inspect", "in.csv", "--bounds", "10"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "ranges without a key are a usage error"
    );
}

#[test]
fn split_round_trip_with_report_status_and_exit_codes() {
    let tmp = setup();
    let key = ["--key", "day", "--type", "date", "--format", "%Y-%m-%d"];
    let args = [
        &[
            "split",
            "in.csv",
            "-o",
            "out",
            "--range",
            "q1=2024-01-01..2024-04-01",
            "--report",
            "report.json",
        ][..],
        &key,
    ]
    .concat();

    let out = ksplit(tmp.path(), &args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read(tmp.path().join("out/q1.csv")).unwrap(),
        b"id,day,note\n5,2024-01-05,a\n25,2024-02-01,b\n15,2024-03-01,c\n"
    );
    assert_eq!(
        fs::read(tmp.path().join("out/_unmatched.csv")).unwrap(),
        b"id,day,note\nx,2024-04-01,bad\n"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(tmp.path().join("report.json")).unwrap()).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["per_range"][0]["records"], 3);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("q1"),
        "human summary on stderr"
    );

    assert_eq!(
        ksplit(tmp.path(), &args).status.code(),
        Some(1),
        "existing outputs are refused"
    );
    let mut overwrite = args.clone();
    overwrite.push("--overwrite");
    assert!(ksplit(tmp.path(), &overwrite).status.success());

    let status = json(&ksplit(tmp.path(), &["status", "out", "--json"]));
    assert_eq!(status["checkpoint"], serde_json::Value::Null);

    let strict = [
        "split",
        "in.csv",
        "-o",
        "out",
        "--overwrite",
        "--strict",
        "--key",
        "id",
        "--type",
        "int",
        "--range",
        "a=0..10",
    ];
    let out = ksplit(tmp.path(), &strict);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("record 4"));
}

#[test]
fn argument_validation_happens_before_any_work() {
    let tmp = setup();
    let cases: &[&[&str]] = &[
        &[
            "split", "-", "-o", "out", "--key", "#0", "--type", "int", "--bounds", "1", "--resume",
        ],
        &[
            "split", "in.csv", "-o", "out", "--key", "id", "--type", "decimal", "--bounds", "1",
        ],
        &[
            "split", "in.csv", "-o", "out", "--key", "id", "--type", "int", "--bounds", "1",
            "--range", "a=1..2",
        ],
        &[
            "split",
            "in.csv",
            "-o",
            "out",
            "--no-header",
            "--key",
            "id",
            "--type",
            "int",
            "--bounds",
            "1",
        ],
        &[
            "split", "in.csv", "-o", "out", "--key", "nope", "--type", "int", "--bounds", "1",
        ],
    ];
    for args in cases {
        let out = ksplit(tmp.path(), args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!tmp.path().join("out").exists(), "{args:?}");
    }
}
