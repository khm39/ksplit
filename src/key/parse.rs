use chrono::format::{parse, Fixed, Item, Parsed, StrftimeItems};
use chrono::{TimeZone, Utc};

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
    Decimal { scale: u32 },
    Date { items: Vec<Item<'static>> },
    DateTime { items: Vec<Item<'static>>, has_offset: bool, default_tz: Option<TimeZoneSpec> },
    String,
}

impl KeyParser {
    pub(crate) fn new(key_type: &KeyType) -> Result<Self, ConfigError> {
        let kind = match key_type {
            KeyType::Int => Kind::Int,
            KeyType::Decimal { scale } if *scale > MAX_SCALE => return Err(ConfigError::ScaleTooLarge),
            KeyType::Decimal { scale } => Kind::Decimal { scale: *scale },
            KeyType::Date { format } => Kind::Date { items: compile(format)? },
            KeyType::DateTime { format, default_tz } => {
                let items = compile(format)?;
                let has_offset = items.iter().any(|item| {
                    matches!(
                        item,
                        Item::Fixed(
                            Fixed::TimezoneOffset
                                | Fixed::TimezoneOffsetColon
                                | Fixed::TimezoneOffsetDoubleColon
                                | Fixed::TimezoneOffsetTripleColon
                                | Fixed::TimezoneOffsetZ
                                | Fixed::TimezoneOffsetColonZ
                        )
                    )
                });
                Kind::DateTime { items, has_offset, default_tz: default_tz.clone() }
            }
            KeyType::String => Kind::String,
        };
        Ok(Self { kind })
    }

    pub(crate) fn parse(&self, text: &str) -> Result<KeyValue, ParseError> {
        match &self.kind {
            Kind::Int => parse_int(text).map(KeyValue::Int),
            Kind::Decimal { scale } => parse_decimal(text, *scale).map(|units| KeyValue::Decimal { units, scale: *scale }),
            Kind::Date { items } => {
                let parsed = parse_items(text, items)?;
                parsed.to_naive_date().map(KeyValue::Date).map_err(|e| ParseError(e.to_string()))
            }
            Kind::DateTime { items, has_offset, default_tz } => {
                let parsed = parse_items(text, items)?;
                if *has_offset {
                    return parsed.to_datetime().map(|dt| KeyValue::DateTime(dt.with_timezone(&Utc))).map_err(err);
                }
                let naive = parsed.to_naive_datetime_with_offset(0).map_err(err)?;
                let local = match default_tz {
                    None => return Err(ParseError("value has no offset and no default time zone is set".into())),
                    Some(TimeZoneSpec::Named(tz)) => tz.from_local_datetime(&naive).earliest().map(|dt| dt.with_timezone(&Utc)),
                    Some(TimeZoneSpec::Fixed(off)) => off.from_local_datetime(&naive).earliest().map(|dt| dt.with_timezone(&Utc)),
                };
                local.map(KeyValue::DateTime).ok_or_else(|| ParseError("local time does not exist in the default time zone".into()))
            }
            Kind::String => Ok(KeyValue::String(text.to_string())),
        }
    }
}

fn err(e: chrono::ParseError) -> ParseError {
    ParseError(e.to_string())
}

fn compile(format: &str) -> Result<Vec<Item<'static>>, ConfigError> {
    let items = StrftimeItems::new(format).parse_to_owned().map_err(|_| ConfigError::InvalidFormat(format.to_string()))?;
    if items.iter().any(|i| matches!(i, Item::Error)) {
        return Err(ConfigError::InvalidFormat(format.to_string()));
    }
    Ok(items)
}

fn parse_items(text: &str, items: &[Item<'static>]) -> Result<Parsed, ParseError> {
    let mut parsed = Parsed::new();
    parse(&mut parsed, text, items.iter()).map_err(err)?;
    Ok(parsed)
}

fn parse_int(text: &str) -> Result<i64, ParseError> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ParseError(format!("{text:?} is not an integer")));
    }
    text.parse().map_err(|_| ParseError(format!("{text:?} is out of range for a 64-bit integer")))
}

fn parse_decimal(text: &str, scale: u32) -> Result<i128, ParseError> {
    let invalid = || ParseError(format!("{text:?} is not a decimal number"));
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if int_part.is_empty() || !all_digits(int_part) || !all_digits(frac_part) || (body.contains('.') && frac_part.is_empty()) {
        return Err(invalid());
    }
    if frac_part.len() > scale as usize {
        return Err(ParseError(format!("{text:?} has more than {scale} decimal places")));
    }
    let out_of_range = || ParseError(format!("{text:?} is out of range"));
    let int: i128 = int_part.parse().map_err(|_| out_of_range())?;
    let frac: i128 = if frac_part.is_empty() { 0 } else { frac_part.parse().map_err(|_| out_of_range())? };
    let frac = frac * 10i128.pow(scale - frac_part.len() as u32);
    let units = int.checked_mul(10i128.pow(scale)).and_then(|v| v.checked_add(frac)).ok_or_else(out_of_range)?;
    Ok(if negative { -units } else { units })
}
