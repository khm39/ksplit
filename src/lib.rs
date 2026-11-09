//! ksplit: split large CSV files by key ranges in a single streaming pass.

pub mod cancel;
pub mod config;
pub mod error;
pub(crate) mod scanner;
pub(crate) mod source;

pub use cancel::{CancelToken, Progress};
pub use config::{CsvDialect, Encoding, Input};
pub use error::{ConfigError, DataErrorKind, Error, ResumeRejection};
