//! ksplit: split large CSV files by key ranges in a single streaming pass.

pub mod cancel;
pub mod config;
pub mod error;
pub mod key;
pub mod range;
pub(crate) mod scanner;
pub(crate) mod source;

pub use cancel::{CancelToken, Progress};
pub use config::{CsvDialect, Encoding, Input};
pub use error::{ConfigError, DataErrorKind, Error, ResumeRejection};
pub use key::{KeySelector, KeyType, KeyValue, ParseError, TimeZoneSpec};
pub use range::{Bound, Range, Routed, Router};
