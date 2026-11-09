//! ksplit: split large CSV files by key ranges in a single streaming pass.

pub mod cancel;
pub mod checkpoint;
pub mod config;
pub mod error;
pub mod key;
pub mod range;
pub mod report;

pub(crate) mod consumer;
pub(crate) mod ops;
pub(crate) mod pipeline;
pub(crate) mod scanner;
pub(crate) mod sink;
pub(crate) mod source;

pub use cancel::{CancelToken, Progress};
pub use checkpoint::{read_checkpoint, CheckpointStatus};
pub use config::{CheckpointPolicy, CsvDialect, Encoding, ExistingOutput, Input, InvalidGuard, KeySpec, Mode, OnInvalid, SplitConfig, Warning};
pub use error::{ConfigError, DataErrorKind, Error, ResumeRejection};
pub use key::{KeySelector, KeyType, KeyValue, ParseError, TimeZoneSpec};
pub use ops::split::{plan, split};
pub use range::{ranges_from_bounds, sanitize_output_name, Bound, Range, Routed, Router};
pub use report::{RangeResult, Report};
