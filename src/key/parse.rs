use chrono::format::{parse, Fixed, Item, Numeric, Pad, Parsed, StrftimeItems};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};

use super::{KeyType, KeyValue, ParseError, TimeZoneSpec};
use crate::error::ConfigError;

const MAX_SCALE: u32 = 38;

/// A `KeyType` with its strftime format pre-compiled.
#[derive(Clone, Debug)]
pub(crate) struct KeyParser {
    kind: Kind,
}

#[derive(Clone, Debug)]
enum Kind {
    Int,
    Decimal {
        scale: u32,
    },
    Date {
        items: Vec<Item<'static>>,
        shape: Option<Vec<Piece>>,
    },
    DateTime {
        items: Vec<Item<'static>>,
        shape: Option<Vec<Piece>>,
        instant: Instant,
        default_tz: Option<TimeZoneSpec>,
    },
    String,
}

/// How a datetime format pins down the absolute instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Instant {
    /// The value carries its own UTC offset (`%z` family, RFC 2822, RFC 3339).
    Offset,
    /// The value is a Unix timestamp (`%s`), i.e. already UTC.
    Timestamp,
    /// The value is a local time, interpreted in the default time zone.
    Local,
}

/// One element of a fixed-width format such as `%Y-%m-%d`.
#[derive(Clone, Debug)]
enum Piece {
    Literal(Vec<u8>),
    Digits(Field, usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

impl KeyParser {
    pub(crate) fn new(key_type: &KeyType) -> Result<Self, ConfigError> {
        let kind = match key_type {
            KeyType::Int => Kind::Int,
            KeyType::Decimal { scale } if *scale > MAX_SCALE => {
                return Err(ConfigError::ScaleTooLarge)
            }
            KeyType::Decimal { scale } => Kind::Decimal { scale: *scale },
            KeyType::Date { format } => {
                let items = compile(format)?;
                check_complete(format, &items, None)?;
                let shape = fixed_shape(&items, &[Field::Year, Field::Month, Field::Day]);
                Kind::Date { items, shape }
            }
            KeyType::DateTime { format, default_tz } => {
                let items = compile(format)?;
                let instant = instant_of(&items);
                check_complete(format, &items, Some(instant))?;
                let shape = fixed_shape(
                    &items,
                    &[
                        Field::Year,
                        Field::Month,
                        Field::Day,
                        Field::Hour,
                        Field::Minute,
                        Field::Second,
                    ],
                );
                Kind::DateTime {
                    items,
                    shape,
                    instant,
                    default_tz: default_tz.clone(),
                }
            }
            KeyType::String => Kind::String,
        };
        Ok(Self { kind })
    }

    pub(crate) fn parse(&self, text: &str) -> Result<KeyValue, ParseError> {
        match &self.kind {
            Kind::Int => parse_int(text).map(KeyValue::Int),
            Kind::Decimal { scale } => parse_decimal(text, *scale).map(|units| KeyValue::Decimal {
                units,
                scale: *scale,
            }),
            Kind::Date { items, shape } => {
                if let Some(date) = shape
                    .as_deref()
                    .and_then(|p| parse_fixed(text, p))
                    .and_then(|v| v.date())
                {
                    return Ok(KeyValue::Date(date));
                }
                let parsed = parse_items(text, items)?;
                resolve_date(&parsed).map(KeyValue::Date).map_err(err(text))
            }
            Kind::DateTime {
                items,
                shape,
                instant,
                default_tz,
            } => {
                if let Some(naive) = shape
                    .as_deref()
                    .and_then(|p| parse_fixed(text, p))
                    .and_then(|v| v.datetime())
                {
                    return naive_to_utc(naive, default_tz.as_ref());
                }
                let parsed = parse_items(text, items)?;
                match instant {
                    Instant::Offset | Instant::Timestamp => resolve_instant(&parsed, *instant)
                        .map(KeyValue::DateTime)
                        .map_err(err(text)),
                    Instant::Local => {
                        let naive = parsed.to_naive_datetime_with_offset(0).map_err(err(text))?;
                        naive_to_utc(naive, default_tz.as_ref())
                    }
                }
            }
            Kind::String => Ok(KeyValue::String(text.to_string())),
        }
    }
}

fn naive_to_utc(
    naive: NaiveDateTime,
    default_tz: Option<&TimeZoneSpec>,
) -> Result<KeyValue, ParseError> {
    let local = match default_tz {
        None => {
            return Err(ParseError(format!(
                "{naive}: value has no offset and no default time zone is set"
            )))
        }
        Some(TimeZoneSpec::Named(tz)) => tz
            .from_local_datetime(&naive)
            .earliest()
            .map(|dt| dt.with_timezone(&Utc)),
        Some(TimeZoneSpec::Fixed(off)) => off
            .from_local_datetime(&naive)
            .earliest()
            .map(|dt| dt.with_timezone(&Utc)),
    };
    local.map(KeyValue::DateTime).ok_or_else(|| {
        ParseError(format!(
            "{naive}: local time does not exist in the default time zone"
        ))
    })
}

/// Recognises formats made only of zero-padded numeric fields and literals, each field once.
fn fixed_shape(items: &[Item<'static>], required: &[Field]) -> Option<Vec<Piece>> {
    let mut pieces = Vec::with_capacity(items.len());
    let mut seen = Vec::new();
    for item in items {
        let piece = match item {
            Item::Literal(s) => Piece::Literal(s.as_bytes().to_vec()),
            Item::OwnedLiteral(s) => Piece::Literal(s.as_bytes().to_vec()),
            Item::Space(s) => Piece::Literal(s.as_bytes().to_vec()),
            Item::OwnedSpace(s) => Piece::Literal(s.as_bytes().to_vec()),
            Item::Numeric(numeric, Pad::Zero) => {
                let (field, width) = match numeric {
                    Numeric::Year => (Field::Year, 4),
                    Numeric::Month => (Field::Month, 2),
                    Numeric::Day => (Field::Day, 2),
                    Numeric::Hour => (Field::Hour, 2),
                    Numeric::Minute => (Field::Minute, 2),
                    Numeric::Second => (Field::Second, 2),
                    _ => return None,
                };
                if seen.contains(&field) {
                    return None;
                }
                seen.push(field);
                Piece::Digits(field, width)
            }
            _ => return None,
        };
        pieces.push(piece);
    }
    (seen.len() == required.len() && required.iter().all(|f| seen.contains(f))).then_some(pieces)
}

struct FixedValues([u32; 6]);

impl FixedValues {
    fn date(&self) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(self.0[0] as i32, self.0[1], self.0[2])
    }

    fn datetime(&self) -> Option<NaiveDateTime> {
        self.date()?.and_hms_opt(self.0[3], self.0[4], self.0[5])
    }
}

/// Matches `text` against a fixed shape exactly; any deviation returns None so chrono decides.
fn parse_fixed(text: &str, shape: &[Piece]) -> Option<FixedValues> {
    let bytes = text.as_bytes();
    let mut pos = 0;
    let mut values = [0u32; 6];
    for piece in shape {
        match piece {
            Piece::Literal(lit) => {
                if !bytes[pos..].starts_with(lit) {
                    return None;
                }
                pos += lit.len();
            }
            Piece::Digits(field, width) => {
                let digits = bytes.get(pos..pos + width)?;
                let mut value = 0u32;
                for &b in digits {
                    if !b.is_ascii_digit() {
                        return None;
                    }
                    value = value * 10 + (b - b'0') as u32;
                }
                values[*field as usize] = value;
                pos += width;
            }
        }
    }
    (pos == bytes.len()).then_some(FixedValues(values))
}

fn err(text: &str) -> impl Fn(chrono::ParseError) -> ParseError + '_ {
    move |e| ParseError(format!("{text:?}: {e}"))
}

fn compile(format: &str) -> Result<Vec<Item<'static>>, ConfigError> {
    let items = StrftimeItems::new(format)
        .parse_to_owned()
        .map_err(|_| ConfigError::InvalidFormat(format.to_string()))?;
    let has_specifier = items
        .iter()
        .any(|i| matches!(i, Item::Numeric(..) | Item::Fixed(_)));
    if !has_specifier || items.iter().any(|i| matches!(i, Item::Error)) {
        return Err(ConfigError::InvalidFormat(format.to_string()));
    }
    Ok(items)
}

fn instant_of(items: &[Item<'static>]) -> Instant {
    let offset = items.iter().any(|item| {
        matches!(
            item,
            Item::Fixed(
                Fixed::TimezoneOffset
                    | Fixed::TimezoneOffsetColon
                    | Fixed::TimezoneOffsetDoubleColon
                    | Fixed::TimezoneOffsetTripleColon
                    | Fixed::TimezoneOffsetZ
                    | Fixed::TimezoneOffsetColonZ
                    | Fixed::RFC2822
                    | Fixed::RFC3339
            )
        )
    });
    if offset {
        Instant::Offset
    } else if items
        .iter()
        .any(|i| matches!(i, Item::Numeric(Numeric::Timestamp, _)))
    {
        Instant::Timestamp
    } else {
        Instant::Local
    }
}

/// A date from parsed fields, or from a Unix timestamp when that is all the format gives.
fn resolve_date(parsed: &Parsed) -> Result<NaiveDate, chrono::ParseError> {
    parsed.to_naive_date().or_else(|e| {
        parsed
            .to_naive_datetime_with_offset(0)
            .map(|dt| dt.date())
            .map_err(|_| e)
    })
}

fn resolve_instant(parsed: &Parsed, instant: Instant) -> Result<DateTime<Utc>, chrono::ParseError> {
    match instant {
        Instant::Offset => parsed.to_datetime().map(|dt| dt.with_timezone(&Utc)),
        _ => parsed
            .to_naive_datetime_with_offset(0)
            .map(|naive| naive.and_utc()),
    }
}

/// Rejects formats that can never yield a date (or, for datetime, an instant), by formatting a
/// probe value with the items and parsing it back through the same resolution path chrono uses.
fn check_complete(
    format: &str,
    items: &[Item<'static>],
    instant: Option<Instant>,
) -> Result<(), ConfigError> {
    use std::fmt::Write;
    let probe_time = FixedOffset::east_opt(9 * 3600)
        .unwrap()
        .with_ymd_and_hms(2001, 2, 3, 4, 5, 6)
        .unwrap();
    let mut probe = String::new();
    let incomplete =
        |missing: &'static str| ConfigError::IncompleteFormat(format.to_string(), missing);
    if write!(probe, "{}", probe_time.format_with_items(items.iter())).is_err() {
        return Err(ConfigError::InvalidFormat(format.to_string()));
    }
    let mut parsed = Parsed::new();
    if parse(&mut parsed, &probe, items.iter()).is_err() {
        return Err(incomplete(
            "a value chrono can parse back (e.g. %Z cannot be parsed)",
        ));
    }
    if resolve_date(&parsed).is_err() {
        return Err(incomplete(
            "a full calendar date (year with month and day, ordinal day or week and weekday)",
        ));
    }
    if let Some(instant) = instant {
        let resolved = match instant {
            Instant::Local => parsed.to_naive_datetime_with_offset(0).map(|_| ()),
            _ => resolve_instant(&parsed, instant).map(|_| ()),
        };
        if resolved.is_err() {
            return Err(incomplete("a time of day (hour and minute)"));
        }
    }
    Ok(())
}

fn parse_items(text: &str, items: &[Item<'static>]) -> Result<Parsed, ParseError> {
    let mut parsed = Parsed::new();
    parse(&mut parsed, text, items.iter()).map_err(err(text))?;
    Ok(parsed)
}

fn parse_int(text: &str) -> Result<i64, ParseError> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ParseError(format!("{text:?} is not an integer")));
    }
    text.parse()
        .map_err(|_| ParseError(format!("{text:?} is out of range for a 64-bit integer")))
}

fn parse_decimal(text: &str, scale: u32) -> Result<i128, ParseError> {
    let invalid = || ParseError(format!("{text:?} is not a decimal number"));
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if int_part.is_empty()
        || !all_digits(int_part)
        || !all_digits(frac_part)
        || (body.contains('.') && frac_part.is_empty())
    {
        return Err(invalid());
    }
    if frac_part.len() > scale as usize {
        return Err(ParseError(format!(
            "{text:?} has more than {scale} decimal places"
        )));
    }
    let out_of_range = || ParseError(format!("{text:?} is out of range"));
    let int: i128 = int_part.parse().map_err(|_| out_of_range())?;
    let frac: i128 = if frac_part.is_empty() {
        0
    } else {
        frac_part.parse().map_err(|_| out_of_range())?
    };
    let frac = frac * 10i128.pow(scale - frac_part.len() as u32);
    let units = int
        .checked_mul(10i128.pow(scale))
        .and_then(|v| v.checked_add(frac))
        .ok_or_else(out_of_range)?;
    Ok(if negative { -units } else { units })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveDateTime};

    #[test]
    fn date_and_datetime_parsing_agree_with_chrono_for_strict_and_lenient_inputs() {
        let date = KeyParser::new(&KeyType::Date {
            format: "%Y-%m-%d".into(),
        })
        .unwrap();
        for text in [
            "2024-01-31",
            "2024-1-5",
            "2024-02-30",
            "2024-01-31x",
            "20240131",
            "0001-01-01",
            "+12024-01-01",
        ] {
            let expected = NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .ok()
                .map(KeyValue::Date);
            assert_eq!(date.parse(text).ok(), expected, "{text}");
        }
        let dt = KeyParser::new(&KeyType::DateTime {
            format: "%Y/%m/%d %H:%M:%S".into(),
            default_tz: Some("UTC".parse().unwrap()),
        })
        .unwrap();
        for text in [
            "2024/01/31 23:59:59",
            "2024/01/31 24:00:00",
            "2024/01/31 3:04:05",
            "2024/01/31 03:04:60",
        ] {
            let expected = NaiveDateTime::parse_from_str(text, "%Y/%m/%d %H:%M:%S")
                .ok()
                .map(|n| KeyValue::DateTime(n.and_utc()));
            assert_eq!(dt.parse(text).ok(), expected, "{text}");
        }
    }
}
