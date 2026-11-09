use serde::Serialize;

use crate::config::{CsvDialect, Encoding, Input};
use crate::error::Error;
use crate::key::{decode, unquote, KeyParser, KeyType};
use crate::report::SCHEMA_VERSION;
use crate::source::CsvSource;

#[derive(Debug)]
pub struct InspectConfig {
    pub input: Input,
    pub dialect: CsvDialect,
    pub encoding: Encoding,
    pub sample_rows: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineEnding {
    Lf,
    CrLf,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ColumnInfo {
    pub index: usize,
    pub name: Option<String>,
    pub samples: Vec<String>,
    pub undecodable: usize,
    pub type_candidates: Vec<KeyType>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Inspection {
    pub schema_version: u32,
    pub has_bom: bool,
    pub line_ending: LineEnding,
    pub column_count: usize,
    pub columns: Vec<ColumnInfo>,
    pub sampled_records: usize,
}

const DATE_FORMATS: [&str; 3] = ["%Y-%m-%d", "%Y/%m/%d", "%Y%m%d"];
const DATETIME_FORMATS: [&str; 5] = [
    "%Y-%m-%d %H:%M:%S",
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%dT%H:%M:%S%z",
    "%Y-%m-%dT%H:%M:%S%:z",
    "%Y/%m/%d %H:%M:%S",
];

/// Reads the head of the input: BOM, line ending, columns, sample values and type candidates.
pub fn inspect(config: InspectConfig) -> Result<Inspection, Error> {
    config.dialect.validate(config.encoding)?;
    let mut source = CsvSource::open(config.input, &config.dialect)?;
    let names = super::header_names(&source, config.encoding)?;
    let mut columns: Vec<ColumnInfo> = names
        .iter()
        .flatten()
        .enumerate()
        .map(|(index, name)| ColumnInfo {
            index,
            name: Some(name.clone()),
            samples: Vec::new(),
            undecodable: 0,
            type_candidates: Vec::new(),
        })
        .collect();

    let mut line_ending = LineEnding::Unknown;
    let mut sampled = 0;
    let mut scratch = Vec::new();
    while sampled < config.sample_rows {
        let Some(record) = source.next_record()? else {
            break;
        };
        sampled += 1;
        if line_ending == LineEnding::Unknown && record.bytes.ends_with(b"\n") {
            line_ending = if record.bytes.ends_with(b"\r\n") {
                LineEnding::CrLf
            } else {
                LineEnding::Lf
            };
        }
        for index in 0..record.fields.len() {
            if columns.len() <= index {
                columns.push(ColumnInfo {
                    index,
                    name: None,
                    samples: Vec::new(),
                    undecodable: 0,
                    type_candidates: Vec::new(),
                });
            }
            let raw = record.field(index).unwrap_or_default();
            match decode(config.encoding, unquote(raw, &mut scratch)) {
                Some(text) => columns[index].samples.push(text.into_owned()),
                None => columns[index].undecodable += 1,
            }
        }
    }
    for column in &mut columns {
        column.type_candidates = type_candidates(&column.samples);
    }
    Ok(Inspection {
        schema_version: SCHEMA_VERSION,
        has_bom: source.has_bom(),
        line_ending,
        column_count: columns.len(),
        columns,
        sampled_records: sampled,
    })
}

fn type_candidates(samples: &[String]) -> Vec<KeyType> {
    let values: Vec<&str> = samples
        .iter()
        .map(|s| s.trim_matches([' ', '\t']))
        .filter(|s| !s.is_empty())
        .collect();
    if values.is_empty() {
        return Vec::new();
    }
    let all_parse = |t: &KeyType| {
        KeyParser::new(t)
            .map(|p| values.iter().all(|v| p.parse(v).is_ok()))
            .unwrap_or(false)
    };
    let mut out = Vec::new();
    if all_parse(&KeyType::Int) {
        out.push(KeyType::Int);
    }
    let scale = values
        .iter()
        .map(|v| v.split_once('.').map_or(0, |(_, f)| f.len() as u32))
        .max()
        .unwrap_or(0);
    let decimal = KeyType::Decimal { scale };
    if all_parse(&decimal) {
        out.push(decimal);
    }
    for format in DATE_FORMATS {
        let t = KeyType::Date {
            format: format.into(),
        };
        if all_parse(&t) {
            out.push(t);
        }
    }
    for format in DATETIME_FORMATS {
        let t = KeyType::DateTime {
            format: format.into(),
            default_tz: None,
        };
        let t_utc = KeyType::DateTime {
            format: format.into(),
            default_tz: Some("UTC".parse().unwrap()),
        };
        if all_parse(&t) || all_parse(&t_utc) {
            out.push(t);
        }
    }
    out
}
