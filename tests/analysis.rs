use std::io::Cursor;

use ksplit::{
    CancelToken, CsvDialect, Encoding, Input, InspectConfig, KeySelector, KeySpec, KeyType,
    KeyValue, LineEnding, ProfileConfig,
};

const INPUT: &[u8] = b"\xEF\xBB\xBFid,day,note\r\n3,2024-01-01,x\r\n1,2024-02-01,\"y,z\"\r\nbad,2024-03-01,w\r\n2,2024-04-01,v\r\n";

fn input() -> Input {
    Input::Reader(Box::new(Cursor::new(INPUT)))
}

#[test]
fn inspect_reads_only_the_head_and_proposes_key_types() {
    let inspection = ksplit::inspect(InspectConfig {
        input: input(),
        dialect: CsvDialect::default(),
        encoding: Encoding::Utf8,
        sample_rows: 2,
    })
    .unwrap();
    assert!(inspection.has_bom);
    assert_eq!(inspection.line_ending, LineEnding::CrLf);
    assert_eq!(inspection.sampled_records, 2);
    let names: Vec<_> = inspection
        .columns
        .iter()
        .map(|c| c.name.as_deref())
        .collect();
    assert_eq!(names, [Some("id"), Some("day"), Some("note")]);
    assert_eq!(inspection.columns[2].samples, ["x", "y,z"]);
    assert_eq!(
        inspection.columns[0].type_candidates,
        [KeyType::Int, KeyType::Decimal { scale: 0 }]
    );
    assert_eq!(
        inspection.columns[1].type_candidates,
        [KeyType::Date {
            format: "%Y-%m-%d".into()
        }]
    );
    assert!(inspection.columns[2].type_candidates.is_empty());
}

fn profile_config(key_type: KeyType, column: &str) -> ProfileConfig {
    ProfileConfig {
        input: input(),
        dialect: CsvDialect::default(),
        encoding: Encoding::Utf8,
        key: KeySpec {
            selector: KeySelector::Name(column.into()),
            key_type,
            trim: true,
            invalid_guard: None,
        },
        bins: 2,
        sample_size: 2,
        suggest: Some(3),
        seed: Some(1),
        progress_interval_bytes: 1 << 20,
    }
}

#[test]
fn profile_reports_exact_extremes_and_an_approximate_histogram() {
    let profile = ksplit::profile(
        profile_config(KeyType::Int, "id"),
        &CancelToken::new(),
        |_| {},
    )
    .unwrap();
    assert_eq!((profile.records, profile.invalid), (4, 1));
    assert_eq!(profile.min, Some(KeyValue::Int(1)));
    assert_eq!(profile.max, Some(KeyValue::Int(3)));
    let hist = profile.histogram.unwrap();
    assert!(hist.approximate);
    assert_eq!(hist.sampled, 2, "reservoir holds sample_size values");
    assert_eq!(hist.bins.len(), 2);
    assert_eq!(
        hist.bins.iter().map(|b| b.estimated_count).sum::<u64>(),
        3,
        "estimates scale up to the valid count"
    );
    assert_eq!(hist.bins[0].lo, KeyValue::Int(1));
    assert_eq!(hist.bins[1].hi, KeyValue::Int(3));
    assert_eq!(
        profile.suggested_bounds, None,
        "fewer samples than requested ranges"
    );

    let strings = ksplit::profile(
        profile_config(KeyType::String, "note"),
        &CancelToken::new(),
        |_| {},
    )
    .unwrap();
    assert_eq!(strings.min, Some(KeyValue::String("v".into())));
    assert!(strings.histogram.is_none() && strings.suggested_bounds.is_none());
}
