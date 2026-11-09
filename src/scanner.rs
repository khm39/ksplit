//! Byte-level CSV record scanner: finds record and field boundaries without copying.
//!
//! A record ends at an LF outside quotes. CR bytes are left in place (they are
//! part of the record's raw bytes). Quotes are only significant at the start of a
//! field; a quote in the middle of an unquoted field is literal.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    FieldStart,
    Unquoted,
    Quoted,
    QuoteSeen,
    CrAfterQuote,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ScanResult {
    /// A record ends at `len` bytes from the record start (the LF is included).
    Record { len: usize },
    /// The buffer ended before the record did.
    NeedMore,
    /// A closed quoted field is followed by something other than a delimiter or LF.
    Malformed { offset: usize },
}

pub(crate) struct RecordScanner {
    delimiter: u8,
    state: State,
    field_starts: Vec<u32>,
}

impl RecordScanner {
    pub(crate) fn new(delimiter: u8) -> Self {
        Self {
            delimiter,
            state: State::FieldStart,
            field_starts: vec![0],
        }
    }

    /// Scans `buf[from..]`, where `buf[0]` is the first byte of the current record.
    pub(crate) fn scan(&mut self, buf: &[u8], from: usize) -> ScanResult {
        let mut pos = from;
        while pos < buf.len() {
            match self.state {
                State::FieldStart | State::Unquoted => {
                    if self.state == State::FieldStart && buf[pos] == b'"' {
                        self.state = State::Quoted;
                        pos += 1;
                        continue;
                    }
                    self.state = State::Unquoted;
                    match memchr::memchr2(self.delimiter, b'\n', &buf[pos..]) {
                        None => return self.need_more(),
                        Some(i) => {
                            pos += i;
                            if buf[pos] == b'\n' {
                                return ScanResult::Record { len: pos + 1 };
                            }
                            pos += 1;
                            self.field_starts.push(pos as u32);
                            self.state = State::FieldStart;
                        }
                    }
                }
                State::Quoted => match memchr::memchr(b'"', &buf[pos..]) {
                    None => return self.need_more(),
                    Some(i) => {
                        pos += i + 1;
                        self.state = State::QuoteSeen;
                    }
                },
                State::QuoteSeen => {
                    let b = buf[pos];
                    pos += 1;
                    if b == b'"' {
                        self.state = State::Quoted;
                    } else if b == self.delimiter {
                        self.field_starts.push(pos as u32);
                        self.state = State::FieldStart;
                    } else if b == b'\n' {
                        return ScanResult::Record { len: pos };
                    } else if b == b'\r' {
                        self.state = State::CrAfterQuote;
                    } else {
                        return ScanResult::Malformed { offset: pos - 1 };
                    }
                }
                State::CrAfterQuote => {
                    if buf[pos] == b'\n' {
                        return ScanResult::Record { len: pos + 1 };
                    }
                    return ScanResult::Malformed { offset: pos };
                }
            }
        }
        self.need_more()
    }

    fn need_more(&self) -> ScanResult {
        ScanResult::NeedMore
    }

    /// Field start offsets (relative to the record start) of the current record.
    pub(crate) fn fields(&self) -> &[u32] {
        &self.field_starts
    }

    pub(crate) fn in_quotes(&self) -> bool {
        self.state == State::Quoted
    }

    /// True when the bytes scanned so far form a complete record if the input ends here.
    pub(crate) fn can_end_at_eof(&self) -> bool {
        !matches!(self.state, State::Quoted | State::CrAfterQuote)
    }

    pub(crate) fn reset(&mut self) {
        self.state = State::FieldStart;
        self.field_starts.clear();
        self.field_starts.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_all(input: &[u8], delimiter: u8) -> Vec<(usize, Vec<u32>)> {
        let mut scanner = RecordScanner::new(delimiter);
        let mut out = Vec::new();
        let mut start = 0;
        while start < input.len() {
            match scanner.scan(&input[start..], 0) {
                ScanResult::Record { len } => {
                    out.push((len, scanner.fields().to_vec()));
                    scanner.reset();
                    start += len;
                }
                other => panic!("unexpected {other:?} at {start}"),
            }
        }
        out
    }

    #[test]
    fn splits_records_on_lf_and_tracks_field_starts() {
        let recs = scan_all(b"a,b,c\n1,2,3\n", b',');
        assert_eq!(recs, vec![(6, vec![0, 2, 4]), (6, vec![0, 2, 4])]);
    }

    #[test]
    fn quoted_fields_may_contain_delimiters_newlines_and_doubled_quotes() {
        let recs = scan_all(b"\"x,y\n\"\"z\",2\n", b',');
        assert_eq!(recs, vec![(12, vec![0, 10])]);
    }

    #[test]
    fn crlf_is_part_of_the_record_and_unquoted_quotes_are_literal() {
        let recs = scan_all(b"a\"b,c\r\n", b',');
        assert_eq!(recs, vec![(7, vec![0, 4])]);
    }

    #[test]
    fn byte_after_closing_quote_is_malformed() {
        let mut scanner = RecordScanner::new(b',');
        assert_eq!(
            scanner.scan(b"\"a\"b,c\n", 0),
            ScanResult::Malformed { offset: 3 }
        );
    }

    #[test]
    fn resumes_scanning_across_chunk_boundaries() {
        let input = b"\"ab\ncd\",ef\n";
        let mut scanner = RecordScanner::new(b',');
        for split in 1..input.len() {
            scanner.reset();
            assert_eq!(
                scanner.scan(&input[..split], 0),
                ScanResult::NeedMore,
                "split {split}"
            );
            assert_eq!(
                scanner.scan(input, split),
                ScanResult::Record { len: input.len() },
                "split {split}"
            );
            assert_eq!(scanner.fields(), &[0, 8]);
        }
    }

    #[test]
    fn reports_whether_the_scanner_is_inside_quotes() {
        let mut scanner = RecordScanner::new(b',');
        assert_eq!(scanner.scan(b"\"abc", 0), ScanResult::NeedMore);
        assert!(scanner.in_quotes());
        scanner.reset();
        assert_eq!(scanner.scan(b"abc", 0), ScanResult::NeedMore);
        assert!(!scanner.in_quotes());
    }
}
