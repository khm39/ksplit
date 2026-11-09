use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),
    #[error("data error at record {record} (byte offset {byte_offset}): {kind}")]
    Data {
        record: u64,
        byte_offset: u64,
        column: Option<usize>,
        kind: DataErrorKind,
    },
    #[error("I/O error{}: {source}", path.as_ref().map(|p| format!(" ({})", p.display())).unwrap_or_default())]
    Io {
        path: Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },
    #[error("resume rejected: {0}")]
    ResumeRejected(ResumeRejection),
    #[error("cancelled (checkpoint written: {checkpoint_written})")]
    Cancelled { checkpoint_written: bool },
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Config(_) => 1,
            Error::Data { .. } => 2,
            Error::Io { .. } => 3,
            Error::ResumeRejected(_) => 4,
            Error::Cancelled { .. } => 5,
        }
    }

    pub(crate) fn io(path: Option<&std::path::Path>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.map(|p| p.to_path_buf()),
            source,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io { path: None, source }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DataErrorKind {
    #[error("malformed CSV: a quoted field is followed by an unexpected byte")]
    Malformed,
    #[error("record exceeds the maximum size{}", if *unclosed_quote { " (a quote may be unclosed)" } else { "" })]
    RecordTooLarge { unclosed_quote: bool },
    #[error("invalid key value: {0}")]
    InvalidKey(String),
    #[error("the first {window} records all had invalid keys (last: {last_reason}); check --key, --type and --format")]
    InvalidGuardTripped { window: u64, last_reason: String },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("delimiter must be a single ASCII byte other than quote, CR or LF")]
    InvalidDelimiter,
    #[error("delimiter 0x{0:02x} is unsafe with Shift_JIS input (collides with second bytes)")]
    DelimiterUnsafeForEncoding(u8),
    #[error("key column {0:?} not found; available columns: {1:?}")]
    KeyColumnNotFound(String, Vec<String>),
    #[error("key column name {0:?} matches more than one column")]
    AmbiguousKeyColumn(String),
    #[error("column names require a header row")]
    KeyNameRequiresHeader,
    #[error("invalid strftime format {0:?}")]
    InvalidFormat(String),
    #[error("invalid time zone {0:?}")]
    InvalidTimeZone(String),
    #[error("decimal scale must be at most 38")]
    ScaleTooLarge,
    #[error("at least one range is required")]
    NoRanges,
    #[error("bound {0:?} does not match the key type")]
    BoundTypeMismatch(String),
    #[error("range {0:?} is empty (lo must be less than hi)")]
    EmptyRange(String),
    #[error("ranges {0:?} and {1:?} overlap")]
    OverlappingRanges(String, String),
    #[error("duplicate bound {0:?}")]
    DuplicateBound(String),
    #[error("bound {0:?} cannot be parsed as the key type: {1}")]
    InvalidBound(String, String),
    #[error("output names {0:?} and {1:?} collide after sanitizing")]
    OutputNameCollision(String, String),
    #[error("output name {0:?} is reserved")]
    ReservedOutputName(String),
    #[error("output name {0:?} is empty after sanitizing")]
    EmptyOutputName(String),
    #[error("resume requires a file path input")]
    ResumeRequiresPath,
    #[error("resume requires a checkpoint policy")]
    ResumeRequiresCheckpoint,
    #[error("output {0} already exists; use resume or overwrite")]
    OutputExists(PathBuf),
    #[error(
        "a previous run was interrupted in {0}; use resume to continue or overwrite to restart"
    )]
    InterruptedRunExists(PathBuf),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResumeRejection {
    #[error("checkpoint file is corrupt: {0}")]
    CorruptCheckpoint(String),
    #[error("checkpoint was written by ksplit {saved}, current is {current}")]
    VersionMismatch { saved: String, current: String },
    #[error("configuration differs from the checkpointed run")]
    ConfigMismatch,
    #[error("input {field} changed since the checkpoint (saved {saved}, now {current})")]
    InputMismatch {
        field: &'static str,
        saved: String,
        current: String,
    },
    #[error("output file {0} is missing")]
    OutputMissing(String),
    #[error(
        "output file {file} is shorter than the checkpoint recorded ({actual} < {expected} bytes)"
    )]
    OutputTruncated {
        file: String,
        expected: u64,
        actual: u64,
    },
    #[error("output files exist but no checkpoint was found; use overwrite to restart")]
    OutputsWithoutCheckpoint,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_cli_contract() {
        let io = std::io::Error::other("x");
        let cases = [
            (Error::Config(ConfigError::NoRanges), 1),
            (
                Error::Data {
                    record: 1,
                    byte_offset: 0,
                    column: None,
                    kind: DataErrorKind::Malformed,
                },
                2,
            ),
            (
                Error::Io {
                    path: None,
                    source: io,
                },
                3,
            ),
            (Error::ResumeRejected(ResumeRejection::ConfigMismatch), 4),
            (
                Error::Cancelled {
                    checkpoint_written: true,
                },
                5,
            ),
        ];
        for (err, code) in cases {
            assert_eq!(err.exit_code(), code, "{err}");
        }
    }
}
