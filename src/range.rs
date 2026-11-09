//! Half-open key ranges and the router that assigns a key to one of them.

use std::cmp::Ordering;

use crate::error::ConfigError;
use crate::key::{KeyType, KeyValue};

pub const UNMATCHED_NAME: &str = "_unmatched";
pub const INVALID_NAME: &str = "_invalid";

#[derive(Clone, Debug, PartialEq)]
pub enum Bound {
    Unbounded,
    Value(KeyValue),
}

/// A half-open range `[lo, hi)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Range {
    pub name: String,
    pub lo: Bound,
    pub hi: Bound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Routed {
    Range(usize),
    Unmatched,
}

/// Sorted, validated ranges with O(log n) lookup.
#[derive(Debug)]
pub struct Router {
    ranges: Vec<Range>,
    output_names: Vec<String>,
}

fn cmp_lo(a: &Bound, b: &Bound) -> Ordering {
    match (a, b) {
        (Bound::Unbounded, Bound::Unbounded) => Ordering::Equal,
        (Bound::Unbounded, _) => Ordering::Less,
        (_, Bound::Unbounded) => Ordering::Greater,
        (Bound::Value(x), Bound::Value(y)) => {
            x.partial_cmp(y).expect("bounds validated to share a type")
        }
    }
}

impl Router {
    pub fn new(key_type: &KeyType, mut ranges: Vec<Range>) -> Result<Self, ConfigError> {
        if ranges.is_empty() {
            return Err(ConfigError::NoRanges);
        }
        for r in &ranges {
            for b in [&r.lo, &r.hi] {
                if let Bound::Value(v) = b {
                    if !key_type.matches(v) {
                        return Err(ConfigError::BoundTypeMismatch(v.canonical()));
                    }
                }
            }
            if let (Bound::Value(lo), Bound::Value(hi)) = (&r.lo, &r.hi) {
                if lo >= hi {
                    return Err(ConfigError::EmptyRange(r.name.clone()));
                }
            }
        }
        ranges.sort_by(|a, b| cmp_lo(&a.lo, &b.lo));
        for pair in ranges.windows(2) {
            let overlaps = match (&pair[0].hi, &pair[1].lo) {
                (Bound::Unbounded, _) | (_, Bound::Unbounded) => true,
                (Bound::Value(hi), Bound::Value(lo)) => hi > lo,
            };
            if overlaps {
                return Err(ConfigError::OverlappingRanges(
                    pair[0].name.clone(),
                    pair[1].name.clone(),
                ));
            }
        }
        let mut output_names = Vec::with_capacity(ranges.len());
        for r in &ranges {
            let name = sanitize_output_name(&r.name)?;
            if name == UNMATCHED_NAME || name == INVALID_NAME {
                return Err(ConfigError::ReservedOutputName(r.name.clone()));
            }
            if let Some(i) = output_names
                .iter()
                .position(|n: &String| n.eq_ignore_ascii_case(&name))
            {
                return Err(ConfigError::OutputNameCollision(
                    ranges[i].name.clone(),
                    r.name.clone(),
                ));
            }
            output_names.push(name);
        }
        Ok(Self {
            ranges,
            output_names,
        })
    }

    pub fn route(&self, key: &KeyValue) -> Routed {
        let idx = self.ranges.partition_point(|r| match &r.lo {
            Bound::Unbounded => true,
            Bound::Value(lo) => lo <= key,
        });
        if idx == 0 {
            return Routed::Unmatched;
        }
        match &self.ranges[idx - 1].hi {
            Bound::Unbounded => Routed::Range(idx - 1),
            Bound::Value(hi) if key < hi => Routed::Range(idx - 1),
            _ => Routed::Unmatched,
        }
    }

    /// Ranges in routing order (ascending by lower bound).
    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    /// Sanitized output base names, parallel to `ranges()`.
    pub fn output_names(&self) -> &[String] {
        &self.output_names
    }
}

/// Builds N+1 ranges from N boundary values; names derive from the values.
pub fn ranges_from_bounds(mut bounds: Vec<KeyValue>) -> Result<Vec<Range>, ConfigError> {
    if bounds.is_empty() {
        return Err(ConfigError::NoRanges);
    }
    bounds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    if let Some(dup) = bounds.windows(2).find(|w| w[0] == w[1]) {
        return Err(ConfigError::DuplicateBound(dup[0].canonical()));
    }
    let name_of = |v: &KeyValue| v.canonical().replace(':', "-");
    let mut ranges = Vec::with_capacity(bounds.len() + 1);
    ranges.push(Range {
        name: format!("lt_{}", name_of(&bounds[0])),
        lo: Bound::Unbounded,
        hi: Bound::Value(bounds[0].clone()),
    });
    for pair in bounds.windows(2) {
        ranges.push(Range {
            name: format!("{}_{}", name_of(&pair[0]), name_of(&pair[1])),
            lo: Bound::Value(pair[0].clone()),
            hi: Bound::Value(pair[1].clone()),
        });
    }
    let last = bounds.last().unwrap();
    ranges.push(Range {
        name: format!("ge_{}", name_of(last)),
        lo: Bound::Value(last.clone()),
        hi: Bound::Unbounded,
    });
    Ok(ranges)
}

const WINDOWS_RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

/// Makes a range name safe as a file base name on Linux, macOS and Windows.
pub fn sanitize_output_name(name: &str) -> Result<String, ConfigError> {
    let replaced: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = replaced.trim_matches([' ', '.']);
    if trimmed.is_empty() {
        return Err(ConfigError::EmptyOutputName(name.to_string()));
    }
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or(trimmed)
        .to_ascii_uppercase();
    let reserved = WINDOWS_RESERVED.contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    Ok(if reserved {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::{KeyType, KeyValue};

    fn int(v: i64) -> Bound {
        Bound::Value(KeyValue::Int(v))
    }

    fn range(name: &str, lo: Bound, hi: Bound) -> Range {
        Range {
            name: name.into(),
            lo,
            hi,
        }
    }

    #[test]
    fn routes_with_half_open_semantics_gaps_and_unbounded_ends() {
        let router = Router::new(
            &KeyType::Int,
            vec![
                range("mid", int(10), int(20)),
                range("low", Bound::Unbounded, int(0)),
                range("high", int(100), Bound::Unbounded),
            ],
        )
        .unwrap();
        let names: Vec<_> = router.ranges().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["low", "mid", "high"]);
        let route = |v| router.route(&KeyValue::Int(v));
        assert_eq!(route(-5), Routed::Range(0));
        assert_eq!(route(0), Routed::Unmatched);
        assert_eq!(route(10), Routed::Range(1));
        assert_eq!(route(20), Routed::Unmatched);
        assert_eq!(route(1_000_000), Routed::Range(2));
    }

    #[test]
    fn construction_rejects_bad_range_sets() {
        let new = |ranges| Router::new(&KeyType::Int, ranges).err().unwrap();
        assert_eq!(new(vec![]), ConfigError::NoRanges);
        assert_eq!(
            new(vec![range("a", int(5), int(5))]),
            ConfigError::EmptyRange("a".into())
        );
        assert_eq!(
            new(vec![
                range("a", int(0), int(10)),
                range("b", int(9), int(20))
            ]),
            ConfigError::OverlappingRanges("a".into(), "b".into())
        );
        assert_eq!(
            new(vec![
                range("a", Bound::Unbounded, int(1)),
                range("b", Bound::Unbounded, int(0))
            ]),
            ConfigError::OverlappingRanges("a".into(), "b".into())
        );
        assert!(matches!(
            new(vec![range(
                "a",
                Bound::Value(KeyValue::String("x".into())),
                Bound::Unbounded
            )]),
            ConfigError::BoundTypeMismatch(_)
        ));
        assert_eq!(
            new(vec![range("A", int(0), int(1)), range("a", int(1), int(2))]),
            ConfigError::OutputNameCollision("A".into(), "a".into())
        );
        assert_eq!(
            new(vec![range("_unmatched", int(0), int(1))]),
            ConfigError::ReservedOutputName("_unmatched".into())
        );
    }

    #[test]
    fn bounds_expand_to_n_plus_one_named_ranges() {
        let ranges = ranges_from_bounds(vec![KeyValue::Int(1000), KeyValue::Int(0)]).unwrap();
        let names: Vec<_> = ranges.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["lt_0", "0_1000", "ge_1000"]);
        assert_eq!(ranges[0].lo, Bound::Unbounded);
        assert_eq!(ranges[2].hi, Bound::Unbounded);
        assert_eq!(
            ranges_from_bounds(vec![KeyValue::Int(1), KeyValue::Int(1)])
                .err()
                .unwrap(),
            ConfigError::DuplicateBound("1".into())
        );
    }

    #[test]
    fn output_names_are_made_filesystem_safe() {
        assert_eq!(sanitize_output_name("a/b:c").unwrap(), "a_b_c");
        assert_eq!(sanitize_output_name("Con").unwrap(), "_Con");
        assert_eq!(sanitize_output_name(" x. ").unwrap(), "x");
        assert_eq!(
            sanitize_output_name("..").unwrap_err(),
            ConfigError::EmptyOutputName("..".into())
        );
        assert_eq!(
            sanitize_output_name("2024-01-01T00:00:00Z").unwrap(),
            "2024-01-01T00_00_00Z"
        );
    }
}
