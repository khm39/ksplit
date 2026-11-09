use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use ksplit::Progress;

/// Throttled progress line on stderr.
pub struct ProgressPrinter {
    started: Instant,
    last_draw: Option<Instant>,
    interval: Duration,
    is_tty: bool,
    quiet: bool,
    drew_line: bool,
}

impl ProgressPrinter {
    pub fn new(quiet: bool) -> Self {
        let is_tty = std::io::stderr().is_terminal();
        Self {
            started: Instant::now(),
            last_draw: None,
            interval: if is_tty {
                Duration::from_millis(200)
            } else {
                Duration::from_secs(5)
            },
            is_tty,
            quiet,
            drew_line: false,
        }
    }

    pub fn update(&mut self, p: &Progress) {
        if self.quiet || self.last_draw.is_some_and(|t| t.elapsed() < self.interval) {
            return;
        }
        self.last_draw = Some(Instant::now());
        let secs = self.started.elapsed().as_secs_f64().max(1e-3);
        let rate = p.bytes_read as f64 / secs / 1e6;
        let line = match p.total_bytes {
            Some(total) if total > 0 => format!(
                "read {} / {} ({:.0}%)  {} records  {rate:.0} MB/s",
                human(p.bytes_read),
                human(total),
                p.bytes_read as f64 * 100.0 / total as f64,
                group(p.records)
            ),
            _ => format!(
                "read {}  {} records  {rate:.0} MB/s",
                human(p.bytes_read),
                group(p.records)
            ),
        };
        let mut err = std::io::stderr().lock();
        if self.is_tty {
            let _ = write!(err, "\r\x1b[2K{line}");
            self.drew_line = true;
        } else {
            let _ = writeln!(err, "{line}");
        }
        let _ = err.flush();
    }

    /// Ends the in-place progress line so later output starts on a fresh line.
    pub fn finish(&mut self) {
        if self.drew_line {
            let _ = writeln!(std::io::stderr());
            self.drew_line = false;
        }
    }
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
