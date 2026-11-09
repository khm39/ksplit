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

    let out = ksplit(
        tmp.path(),
        &[
            "inspect", "in.csv", "--quiet", "--key", "id", "--type", "int", "--strict",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "--strict applies to the profile pass too"
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
fn range_bounds_may_end_with_a_dot_or_contain_dots() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("dots.csv"), b"d\n+20260415.\n+20260515.\n").unwrap();
    let out = ksplit(
        tmp.path(),
        &[
            "inspect",
            "dots.csv",
            "--json",
            "--quiet",
            "--key",
            "d",
            "--type",
            "date",
            "--format",
            "+%Y%m%d.",
            "--range",
            "apr=+20260401...+20260501.",
            "--range",
            "may=+20260501...",
        ],
    );
    let both = json(&out);
    let counts: Vec<_> = both["plan"]["per_range"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["records"].as_u64().unwrap())
        .collect();
    assert_eq!(counts, [1, 1]);
}

#[test]
fn hyphen_values_blank_lines_and_resume_refusal_exit_codes() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("neg.csv"),
        b"id,t\n-10,2024-01-01 00:00:00\n\n0,2024-01-01 12:00:00\n10,2024-01-02 00:00:00\n\n",
    )
    .unwrap();
    let plan = json(&ksplit(
        tmp.path(),
        &[
            "inspect", "neg.csv", "--json", "--quiet", "--key", "id", "--type", "int", "--bounds",
            "-5,5",
        ],
    ));
    let counts: Vec<_> = plan["plan"]["per_range"]
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
            ("lt_-5".to_string(), 1),
            ("-5_5".to_string(), 1),
            ("ge_5".to_string(), 1)
        ]
    );
    assert_eq!(
        (&plan["plan"]["records"], &plan["plan"]["invalid"]),
        (&3.into(), &0.into()),
        "blank lines are not records"
    );

    let profile = json(&ksplit(
        tmp.path(),
        &[
            "inspect",
            "neg.csv",
            "--json",
            "--quiet",
            "--key",
            "t",
            "--type",
            "datetime",
            "--format",
            "%Y-%m-%d %H:%M:%S",
            "--tz",
            "-05:00",
            "--suggest",
            "2",
        ],
    ));
    assert_eq!(profile["min"], "2024-01-01T05:00:00Z");

    let out = ksplit(
        tmp.path(),
        &[
            "inspect",
            "neg.csv",
            "--quiet",
            "--key",
            "t",
            "--type",
            "datetime",
            "--format",
            "%Y-%m-%d %H:%M:%S",
            "--tz",
            "Asia/Tokyo",
            "--suggest",
            "2",
        ],
    );
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let suggested = stdout
        .lines()
        .find_map(|l| l.strip_prefix("suggested --bounds "))
        .expect("a suggestion line");
    assert_eq!(
        suggested, "2024-01-01 12:00:00",
        "suggestions use the user's format and zone, so they parse back"
    );

    let split = [
        "split", "neg.csv", "-o", "out", "--key", "id", "--type", "int", "--bounds", "-5,5",
    ];
    assert!(ksplit(tmp.path(), &split).status.success());
    let mut resume = split.to_vec();
    resume.push("--resume");
    assert_eq!(
        ksplit(tmp.path(), &resume).status.code(),
        Some(4),
        "outputs without a checkpoint are a resume rejection"
    );
}

#[test]
fn argument_validation_happens_before_any_work() {
    let tmp = setup();
    let cases: &[&[&str]] = &[
        &[
            "split", "-", "-o", "out", "--key", "#0", "--type", "int", "--bounds", "1", "--resume",
        ],
        &[
            "split",
            "in.csv",
            "-o",
            "out",
            "--key",
            "note",
            "--type",
            "string",
            "--range",
            "a=x..y..z",
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
