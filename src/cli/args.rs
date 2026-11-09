use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "ksplit",
    version,
    about = "Split large CSV files by key ranges in one streaming pass"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Look at the input: columns and samples, or (with a key) its distribution, or (with ranges) per-range counts
    Inspect(InspectArgs),
    /// Write one output file per range
    Split(SplitArgs),
    /// Show the checkpoint of an interrupted split
    Status(StatusArgs),
}

#[derive(Args, Debug, Clone)]
pub struct CsvOpts {
    /// Input encoding
    #[arg(long, value_enum, default_value_t = EncodingArg::Utf8)]
    pub encoding: EncodingArg,
    /// Field delimiter (one ASCII character; `\t` for tab)
    #[arg(long, default_value = ",")]
    pub delimiter: String,
    /// The input has no header row (the key must then be a #column number)
    #[arg(long)]
    pub no_header: bool,
    /// Maximum size of one record, e.g. 16MiB
    #[arg(long, default_value = "16MiB", value_parser = parse_size)]
    pub max_record_size: u64,
    /// Write the result as JSON to stdout
    #[arg(long)]
    pub json: bool,
    /// Suppress progress and notices on stderr
    #[arg(long)]
    pub quiet: bool,
}

#[derive(Args, Debug, Clone)]
pub struct KeyOpts {
    /// Key column: a header name, or #N for the 0-based column number
    #[arg(long)]
    pub key: Option<String>,
    /// Key type
    #[arg(long, value_enum)]
    pub r#type: Option<KeyTypeArg>,
    /// Decimal places (required for --type decimal)
    #[arg(long)]
    pub scale: Option<u32>,
    /// strftime format (required for --type date / datetime)
    #[arg(long)]
    pub format: Option<String>,
    /// Default time zone for datetime values without an offset (IANA name or +HH:MM)
    #[arg(long)]
    pub tz: Option<String>,
    /// Do not trim ASCII spaces and tabs around key values
    #[arg(long)]
    pub no_trim: bool,
    /// Stop if the first N records all have invalid keys
    #[arg(long, default_value_t = 10_000, conflicts_with = "no_invalid_guard")]
    pub invalid_guard: u64,
    #[arg(long)]
    pub no_invalid_guard: bool,
}

#[derive(Args, Debug, Clone)]
pub struct RangeOpts {
    /// Boundary values; N values make N+1 ranges
    #[arg(long, value_delimiter = ',', conflicts_with = "range")]
    pub bounds: Option<Vec<String>>,
    /// A named half-open range NAME=LO..HI (either side may be empty); repeatable
    #[arg(long = "range", value_name = "NAME=LO..HI")]
    pub range: Vec<String>,
    /// split writes unmatched rows to _unmatched.csv; extract drops them
    #[arg(long, value_enum, default_value_t = ModeArg::Split)]
    pub mode: ModeArg,
}

#[derive(Args, Debug)]
pub struct InspectArgs {
    /// Input file, or - for stdin
    pub input: String,
    /// Sample values per column
    #[arg(long, default_value_t = 20)]
    pub sample: usize,
    /// Histogram bins
    #[arg(long, default_value_t = 20)]
    pub bins: usize,
    /// Reservoir sample size for the histogram
    #[arg(long, default_value_t = 100_000)]
    pub sample_size: usize,
    /// Suggest boundaries for N ranges of roughly equal size
    #[arg(long)]
    pub suggest: Option<usize>,
    /// Stop at the first invalid key
    #[arg(long)]
    pub strict: bool,
    #[command(flatten)]
    pub csv: CsvOpts,
    #[command(flatten)]
    pub key: KeyOpts,
    #[command(flatten)]
    pub range: RangeOpts,
}

#[derive(Args, Debug)]
pub struct SplitArgs {
    /// Input file, or - for stdin
    pub input: String,
    /// Output directory
    #[arg(short, long)]
    pub out_dir: PathBuf,
    /// Stop at the first invalid key instead of writing it to _invalid.csv
    #[arg(long)]
    pub strict: bool,
    /// Continue an interrupted run from its checkpoint
    #[arg(long, conflicts_with = "overwrite")]
    pub resume: bool,
    /// Replace existing outputs
    #[arg(long)]
    pub overwrite: bool,
    /// Bytes of input between checkpoints
    #[arg(long, default_value = "256MiB", value_parser = parse_size)]
    pub checkpoint_interval: u64,
    /// Write buffer per output file
    #[arg(long, default_value = "64KiB", value_parser = parse_size)]
    pub write_buffer: u64,
    /// Save the report as JSON
    #[arg(long)]
    pub report: Option<PathBuf>,
    #[command(flatten)]
    pub csv: CsvOpts,
    #[command(flatten)]
    pub key: KeyOpts,
    #[command(flatten)]
    pub range: RangeOpts,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    pub out_dir: PathBuf,
    #[arg(long)]
    pub json: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodingArg {
    #[value(name = "utf-8")]
    Utf8,
    #[value(name = "shift_jis")]
    ShiftJis,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyTypeArg {
    Int,
    Decimal,
    Date,
    Datetime,
    String,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeArg {
    Split,
    Extract,
}

/// Parses sizes like `64KiB`, `256MiB`, `1GiB`, `16M` or plain bytes.
pub fn parse_size(text: &str) -> Result<u64, String> {
    let digits_end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (num, unit) = text.split_at(digits_end);
    let num: u64 = num.parse().map_err(|_| format!("invalid size {text:?}"))?;
    let shift = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 0,
        "k" | "kib" | "kb" => 10,
        "m" | "mib" | "mb" => 20,
        "g" | "gib" | "gb" => 30,
        _ => {
            return Err(format!(
                "unknown size unit in {text:?}; use KiB, MiB or GiB"
            ))
        }
    };
    num.checked_shl(shift)
        .filter(|v| v >> shift == num)
        .ok_or_else(|| format!("size {text:?} is too large"))
}
