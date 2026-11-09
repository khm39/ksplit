use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RangeResult {
    pub name: String,
    pub path: Option<PathBuf>,
    pub records: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub per_range: Vec<RangeResult>,
    pub unmatched: u64,
    pub invalid: u64,
    pub records: u64,
    pub bytes_read: u64,
    #[serde(serialize_with = "secs")]
    pub elapsed: Duration,
    pub resumed_from: Option<u64>,
    pub warnings: Vec<String>,
}

pub(crate) fn secs<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}
