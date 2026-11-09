use std::io::Read;
use std::path::PathBuf;

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
        Self { delimiter: b',', has_header: true, max_record_bytes: 16 * 1024 * 1024 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    Utf8,
    ShiftJis,
}
