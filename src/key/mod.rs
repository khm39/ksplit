//! Key column selection, decoding and typed comparison.

mod decode;
mod parse;

use std::cmp::Ordering;
use std::str::FromStr;

use chrono::{DateTime, FixedOffset, NaiveDate, Utc};

pub(crate) use decode::{decode, unquote};
pub(crate) use parse::KeyParser;

use crate::config::Encoding;
use crate::error::ConfigError;
use crate::source::RawRecord;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySelector {
    Name(String),
    Index(usize),
}

impl KeySelector {
    /// Resolves the key column index against the header (if any).
    pub(crate) fn resolve(&self, header: Option<&[String]>) -> Result<usize, ConfigError> {
        match (self, header) {
            (KeySelector::Index(i), None) => Ok(*i),
            (KeySelector::Index(i), Some(h)) if *i < h.len() => Ok(*i),
            (KeySelector::Index(i), Some(h)) => Err(ConfigError::KeyColumnNotFound(format!("#{i}"), h.to_vec())),
            (KeySelector::Name(_), None) => Err(ConfigError::KeyNameRequiresHeader),
            (KeySelector::Name(name), Some(h)) => {
                let mut found = h.iter().enumerate().filter(|(_, n)| *n == name).map(|(i, _)| i);
                match (found.next(), found.next()) {
                    (Some(i), None) => Ok(i),
                    (Some(_), Some(_)) => Err(ConfigError::AmbiguousKeyColumn(name.clone())),
                    (None, _) => Err(ConfigError::KeyColumnNotFound(name.clone(), h.to_vec())),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimeZoneSpec {
    Named(chrono_tz::Tz),
    Fixed(FixedOffset),
}

impl FromStr for TimeZoneSpec {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || ConfigError::InvalidTimeZone(s.to_string());
        if let Some(rest) = s.strip_prefix(['+', '-']) {
            let (h, m) = rest.split_once(':').ok_or_else(invalid)?;
            let (h, m): (i32, i32) = (h.parse().map_err(|_| invalid())?, m.parse().map_err(|_| invalid())?);
            if h > 23 || m > 59 {
                return Err(invalid());
            }
            let secs = (h * 3600 + m * 60) * if s.starts_with('-') { -1 } else { 1 };
            return FixedOffset::east_opt(secs).map(TimeZoneSpec::Fixed).ok_or_else(invalid);
        }
        s.parse::<chrono_tz::Tz>().map(TimeZoneSpec::Named).map_err(|_| invalid())
    }
}

impl std::fmt::Display for TimeZoneSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimeZoneSpec::Named(tz) => write!(f, "{}", tz.name()),
            TimeZoneSpec::Fixed(off) => write!(f, "{off}"),
        }
    }
}

impl serde::Serialize for TimeZoneSpec {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum KeyType {
    Int,
    Decimal { scale: u32 },
    Date { format: String },
    DateTime { format: String, default_tz: Option<TimeZoneSpec> },
    String,
}

impl KeyType {
    pub fn validate(&self) -> Result<(), ConfigError> {
        KeyParser::new(self).map(|_| ())
    }

    /// Parses one trimmed, decoded key value. Prefer `KeyParser` in hot loops.
    pub fn parse(&self, text: &str) -> Result<KeyValue, ParseError> {
        KeyParser::new(self).map_err(|e| ParseError(e.to_string()))?.parse(text)
    }

    pub fn name(&self) -> &'static str {
        match self {
            KeyType::Int => "int",
            KeyType::Decimal { .. } => "decimal",
            KeyType::Date { .. } => "date",
            KeyType::DateTime { .. } => "datetime",
            KeyType::String => "string",
        }
    }

    pub(crate) fn matches(&self, value: &KeyValue) -> bool {
        matches!(
            (self, value),
            (KeyType::Int, KeyValue::Int(_))
                | (KeyType::Date { .. }, KeyValue::Date(_))
                | (KeyType::DateTime { .. }, KeyValue::DateTime(_))
                | (KeyType::String, KeyValue::String(_))
        ) || matches!((self, value), (KeyType::Decimal { scale }, KeyValue::Decimal { scale: s, .. }) if scale == s)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ParseError(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyValue {
    Int(i64),
    Decimal { units: i128, scale: u32 },
    Date(NaiveDate),
    DateTime(DateTime<Utc>),
    String(String),
}

impl KeyValue {
    /// Canonical text form: parseable by the same `KeyType`, and stable across versions.
    pub fn canonical(&self) -> String {
        match self {
            KeyValue::Int(v) => v.to_string(),
            KeyValue::Decimal { units, scale } => {
                if *scale == 0 {
                    return units.to_string();
                }
                let sign = if *units < 0 { "-" } else { "" };
                let digits = units.unsigned_abs().to_string();
                let scale = *scale as usize;
                let padded = format!("{digits:0>width$}", width = scale + 1);
                let (int, frac) = padded.split_at(padded.len() - scale);
                format!("{sign}{int}.{frac}")
            }
            KeyValue::Date(d) => d.format("%Y-%m-%d").to_string(),
            KeyValue::DateTime(dt) => dt.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            KeyValue::String(s) => s.clone(),
        }
    }
}

impl PartialOrd for KeyValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (KeyValue::Int(a), KeyValue::Int(b)) => Some(a.cmp(b)),
            (KeyValue::Decimal { units: a, scale: sa }, KeyValue::Decimal { units: b, scale: sb }) if sa == sb => {
                Some(a.cmp(b))
            }
            (KeyValue::Date(a), KeyValue::Date(b)) => Some(a.cmp(b)),
            (KeyValue::DateTime(a), KeyValue::DateTime(b)) => Some(a.cmp(b)),
            (KeyValue::String(a), KeyValue::String(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }
}

impl std::fmt::Display for KeyValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.canonical())
    }
}

impl serde::Serialize for KeyValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.canonical())
    }
}

#[derive(Debug)]
pub(crate) enum KeyOutcome {
    Valid(KeyValue),
    Invalid(InvalidReason),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InvalidReason {
    MissingColumn,
    Empty,
    BadEncoding,
    Parse(ParseError),
}

impl std::fmt::Display for InvalidReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvalidReason::MissingColumn => f.write_str("key column is missing"),
            InvalidReason::Empty => f.write_str("key is empty"),
            InvalidReason::BadEncoding => f.write_str("key is not valid in the input encoding"),
            InvalidReason::Parse(e) => write!(f, "{e}"),
        }
    }
}

/// Extracts and parses the key column of raw records, reusing scratch buffers.
pub(crate) struct KeyExtractor {
    column: usize,
    parser: KeyParser,
    trim: bool,
    encoding: Encoding,
    scratch: Vec<u8>,
}

impl KeyExtractor {
    pub(crate) fn new(column: usize, key_type: KeyType, trim: bool, encoding: Encoding) -> Self {
        let parser = KeyParser::new(&key_type).expect("key type validated before extraction");
        Self { column, parser, trim, encoding, scratch: Vec::new() }
    }

    pub(crate) fn column(&self) -> usize {
        self.column
    }

    pub(crate) fn extract(&mut self, record: &RawRecord<'_>) -> KeyOutcome {
        let Some(raw) = record.field(self.column) else {
            return KeyOutcome::Invalid(InvalidReason::MissingColumn);
        };
        let bytes = unquote(raw, &mut self.scratch);
        let Some(text) = decode(self.encoding, bytes) else {
            return KeyOutcome::Invalid(InvalidReason::BadEncoding);
        };
        let text = if self.trim { text.trim_matches([' ', '\t']) } else { &text };
        if text.is_empty() {
            return KeyOutcome::Invalid(InvalidReason::Empty);
        }
        match self.parser.parse(text) {
            Ok(v) => KeyOutcome::Valid(v),
            Err(e) => KeyOutcome::Invalid(InvalidReason::Parse(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::RawRecord;

    fn parse(t: &KeyType, s: &str) -> Result<KeyValue, ParseError> {
        t.validate().unwrap();
        t.parse(s)
    }

    #[test]
    fn int_accepts_only_plain_ascii_decimal_within_i64() {
        let t = KeyType::Int;
        assert_eq!(parse(&t, "-12"), Ok(KeyValue::Int(-12)));
        for bad in ["+1", "1,000", "１２", "1e3", "9223372036854775808", ""] {
            assert!(parse(&t, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn decimal_is_fixed_point_with_explicit_scale() {
        let t = KeyType::Decimal { scale: 2 };
        assert_eq!(parse(&t, "1"), Ok(KeyValue::Decimal { units: 100, scale: 2 }));
        assert_eq!(parse(&t, "-1.5"), Ok(KeyValue::Decimal { units: -150, scale: 2 }));
        assert_eq!(parse(&t, "1.50"), parse(&t, "1.5"));
        for bad in ["1.500", ".5", "1.", "1,000.00"] {
            assert!(parse(&t, bad).is_err(), "{bad}");
        }
        assert_eq!(KeyType::Decimal { scale: 39 }.validate(), Err(ConfigError::ScaleTooLarge));
    }

    #[test]
    fn date_must_match_the_whole_value() {
        let t = KeyType::Date { format: "%Y-%m-%d".into() };
        assert!(parse(&t, "2024-01-31").is_ok());
        assert!(parse(&t, "2024-01-31x").is_err());
        assert!(matches!(KeyType::Date { format: "%Q".into() }.validate(), Err(ConfigError::InvalidFormat(_))));
    }

    #[test]
    fn datetime_uses_the_value_offset_or_the_default_zone() {
        let with_offset = KeyType::DateTime { format: "%Y-%m-%dT%H:%M:%S%:z".into(), default_tz: None };
        let tokyo = KeyType::DateTime {
            format: "%Y-%m-%d %H:%M:%S".into(),
            default_tz: Some("Asia/Tokyo".parse().unwrap()),
        };
        let fixed = KeyType::DateTime {
            format: "%Y-%m-%d %H:%M:%S".into(),
            default_tz: Some("+09:00".parse().unwrap()),
        };
        let a = parse(&with_offset, "2024-01-01T09:00:00+09:00").unwrap();
        assert_eq!(parse(&tokyo, "2024-01-01 09:00:00").unwrap(), a);
        assert_eq!(parse(&fixed, "2024-01-01 09:00:00").unwrap(), a);
        assert_eq!(a.canonical(), "2024-01-01T00:00:00Z");

        let no_tz = KeyType::DateTime { format: "%Y-%m-%d %H:%M:%S".into(), default_tz: None };
        assert!(parse(&no_tz, "2024-01-01 09:00:00").is_err());
        assert!("Mars/Olympus".parse::<TimeZoneSpec>().is_err());
    }

    #[test]
    fn values_order_within_a_type_and_not_across_types() {
        assert!(KeyValue::String("b".into()) > KeyValue::String("a".into()));
        assert!(KeyValue::String("あ".into()) > KeyValue::String("z".into()));
        assert_eq!(KeyValue::Int(1).partial_cmp(&KeyValue::String("1".into())), None);
    }

    fn record(bytes: &[u8]) -> (Vec<u8>, Vec<u32>) {
        let mut fields = vec![0u32];
        for (i, b) in bytes.iter().enumerate() {
            if *b == b',' {
                fields.push(i as u32 + 1);
            }
        }
        (bytes.to_vec(), fields)
    }

    fn extract(ext: &mut KeyExtractor, bytes: &[u8]) -> KeyOutcome {
        let (bytes, fields) = record(bytes);
        ext.extract(&RawRecord { bytes: &bytes, fields: &fields, number: 1, offset: 0 })
    }

    #[test]
    fn extractor_unquotes_trims_and_decodes_the_key_field_only() {
        let mut ext = KeyExtractor::new(1, KeyType::Int, true, Encoding::Utf8);
        assert!(matches!(extract(&mut ext, b"x, 42 ,y\r\n"), KeyOutcome::Valid(KeyValue::Int(42))));
        assert!(matches!(extract(&mut ext, b"x,\"7\"\n"), KeyOutcome::Valid(KeyValue::Int(7))));
        assert!(matches!(extract(&mut ext, b"x\n"), KeyOutcome::Invalid(InvalidReason::MissingColumn)));
        assert!(matches!(extract(&mut ext, b"x,,y\n"), KeyOutcome::Invalid(InvalidReason::Empty)));
        assert!(matches!(extract(&mut ext, b"x,\xff\n"), KeyOutcome::Invalid(InvalidReason::BadEncoding)));

        let mut no_trim = KeyExtractor::new(0, KeyType::Int, false, Encoding::Utf8);
        assert!(matches!(extract(&mut no_trim, b" 1\n"), KeyOutcome::Invalid(InvalidReason::Parse(_))));
    }

    #[test]
    fn extractor_decodes_shift_jis_including_bytes_that_look_like_backslash() {
        let mut ext = KeyExtractor::new(0, KeyType::String, true, Encoding::ShiftJis);
        let hyou = b"\x95\x5c"; // 表 in CP932; second byte is 0x5C
        let outcome = extract(&mut ext, &[&hyou[..], b",\"\"\"\x88\xa4\"\"\"\n"].concat());
        assert!(matches!(outcome, KeyOutcome::Valid(KeyValue::String(s)) if s == "表"));
        assert_eq!(decode(Encoding::ShiftJis, b"\x87\x40").as_deref(), Some("①"));
    }
}
