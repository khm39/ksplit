use std::io::Read;
use std::path::PathBuf;

use crate::error::ConfigError;
use crate::key::{KeySelector, KeyType};
use crate::range::{Range, Router};

pub enum Input {
    Path(PathBuf),
    Reader(Box<dyn Read + Send>),
}

impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Input::Path(p) => f.debug_tuple("Path").field(p).finish(),
            Input::Reader(_) => f.write_str("Reader(..)"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CsvDialect {
    pub delimiter: u8,
    pub has_header: bool,
    pub max_record_bytes: usize,
}

impl Default for CsvDialect {
    fn default() -> Self {
        Self {
            delimiter: b',',
            has_header: true,
            max_record_bytes: 16 * 1024 * 1024,
        }
    }
}

impl CsvDialect {
    pub(crate) fn validate(&self, encoding: Encoding) -> Result<(), ConfigError> {
        if !self.delimiter.is_ascii() || matches!(self.delimiter, b'"' | b'\r' | b'\n') {
            return Err(ConfigError::InvalidDelimiter);
        }
        if encoding == Encoding::ShiftJis && self.delimiter >= 0x40 {
            return Err(ConfigError::DelimiterUnsafeForEncoding(self.delimiter));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    Utf8,
    ShiftJis,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidGuard {
    /// Stop if this many leading records are all invalid.
    pub window_records: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeySpec {
    pub selector: KeySelector,
    pub key_type: KeyType,
    pub trim: bool,
    pub invalid_guard: Option<InvalidGuard>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Split,
    Extract,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnInvalid {
    WriteToFile,
    Fail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExistingOutput {
    Fail,
    Resume,
    Overwrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointPolicy {
    pub interval_bytes: u64,
}

impl Default for CheckpointPolicy {
    fn default() -> Self {
        Self {
            interval_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub struct SplitConfig {
    pub input: Input,
    pub output_dir: PathBuf,
    pub dialect: CsvDialect,
    pub encoding: Encoding,
    pub key: KeySpec,
    pub ranges: Vec<Range>,
    pub mode: Mode,
    pub on_invalid: OnInvalid,
    pub existing: ExistingOutput,
    pub checkpoint: Option<CheckpointPolicy>,
    pub write_buffer_bytes: usize,
    pub progress_interval_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    LargeWriteBuffers { total_bytes: u64 },
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Warning::LargeWriteBuffers { total_bytes } => {
                write!(
                    f,
                    "write buffers total {} MiB across all outputs",
                    total_bytes >> 20
                )
            }
        }
    }
}

const WRITE_BUFFER_WARN_BYTES: u64 = 1 << 30;

impl SplitConfig {
    /// Checks everything that can be checked without reading the input.
    pub fn validate(&self) -> Result<Vec<Warning>, ConfigError> {
        self.router().map(|_| self.warnings())
    }

    pub(crate) fn router(&self) -> Result<Router, ConfigError> {
        self.dialect.validate(self.encoding)?;
        self.key.key_type.validate()?;
        if self.existing == ExistingOutput::Resume && matches!(self.input, Input::Reader(_)) {
            return Err(ConfigError::ResumeRequiresPath);
        }
        if self.existing == ExistingOutput::Resume && self.checkpoint.is_none() {
            return Err(ConfigError::ResumeRequiresCheckpoint);
        }
        Router::new(&self.key.key_type, self.ranges.clone())
    }

    fn warnings(&self) -> Vec<Warning> {
        let total = (self.ranges.len() as u64 + 2) * self.write_buffer_bytes as u64;
        if total > WRITE_BUFFER_WARN_BYTES {
            vec![Warning::LargeWriteBuffers { total_bytes: total }]
        } else {
            Vec::new()
        }
    }
}
