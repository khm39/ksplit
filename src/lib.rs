//! ksplit: split large CSV files by key ranges in a single streaming pass.

pub mod cancel;
pub mod error;
pub(crate) mod scanner;

pub use cancel::{CancelToken, Progress};
pub use error::{ConfigError, DataErrorKind, Error, ResumeRejection};
