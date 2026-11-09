//! Streaming CSV record source over a file or reader.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::config::{CsvDialect, Input};
use crate::error::{DataErrorKind, Error};
use crate::scanner::{RecordScanner, ScanResult};

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
const DEFAULT_CAPACITY: usize = 1024 * 1024;

/// One CSV record as raw bytes, with field start offsets.
#[derive(Debug)]
pub(crate) struct RawRecord<'a> {
    /// Raw bytes including the terminating LF (and a preceding CR) when present.
    pub bytes: &'a [u8],
    /// Field start offsets relative to `bytes`.
    pub fields: &'a [u32],
    /// Absolute byte offset of the record start in the input.
    pub offset: u64,
}

impl<'a> RawRecord<'a> {
    /// Raw field bytes (quotes included), without the record terminator.
    pub(crate) fn field(&self, index: usize) -> Option<&'a [u8]> {
        field_at(self.bytes, self.fields, index)
    }
}

fn field_at<'a>(bytes: &'a [u8], fields: &[u32], index: usize) -> Option<&'a [u8]> {
    let start = *fields.get(index)? as usize;
    let end = match fields.get(index + 1) {
        Some(next) => *next as usize - 1,
        None => {
            let mut end = bytes.len();
            if bytes.ends_with(b"\n") {
                end -= 1;
                if bytes[..end].ends_with(b"\r") {
                    end -= 1;
                }
            }
            end
        }
    };
    Some(&bytes[start..end.max(start)])
}

enum Reader {
    File(File),
    Other(Box<dyn Read + Send>),
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Reader::File(f) => f.read(buf),
            Reader::Other(r) => r.read(buf),
        }
    }
}

pub(crate) struct CsvSource {
    reader: Reader,
    path: Option<PathBuf>,
    total_bytes: Option<u64>,
    max_record_bytes: usize,
    buf: Vec<u8>,
    start: usize,
    filled: usize,
    scanner: RecordScanner,
    position: u64,
    record_number: u64,
    has_bom: bool,
    header: Option<Vec<u8>>,
    header_fields: Vec<u32>,
}

impl CsvSource {
    pub(crate) fn open(input: Input, dialect: &CsvDialect) -> Result<Self, Error> {
        Self::open_with_capacity(input, dialect, DEFAULT_CAPACITY)
    }

    pub(crate) fn open_with_capacity(
        input: Input,
        dialect: &CsvDialect,
        capacity: usize,
    ) -> Result<Self, Error> {
        let (reader, path, total_bytes) = match input {
            Input::Path(p) => {
                let file = File::open(&p).map_err(|e| Error::io(Some(&p), e))?;
                let len = file.metadata().map_err(|e| Error::io(Some(&p), e))?.len();
                (Reader::File(file), Some(p), Some(len))
            }
            Input::Reader(r) => (Reader::Other(r), None, None),
        };
        let mut src = Self {
            reader,
            path,
            total_bytes,
            max_record_bytes: dialect.max_record_bytes,
            buf: vec![0; capacity.max(4)],
            start: 0,
            filled: 0,
            scanner: RecordScanner::new(dialect.delimiter),
            position: 0,
            record_number: 0,
            has_bom: false,
            header: None,
            header_fields: Vec::new(),
        };
        src.read_prelude(dialect.has_header)?;
        Ok(src)
    }

    fn read_prelude(&mut self, has_header: bool) -> Result<(), Error> {
        while self.filled < UTF8_BOM.len() && self.fill()? {}
        if self.buf[..self.filled].starts_with(UTF8_BOM) {
            self.has_bom = true;
            self.start = UTF8_BOM.len();
            self.position = UTF8_BOM.len() as u64;
        }
        if has_header {
            let mut header = Vec::new();
            if self.has_bom {
                header.extend_from_slice(UTF8_BOM);
            }
            // An input with no header record at all (empty or BOM-only) has no header to
            // resolve names against or to copy; leave `header` as None.
            if let Some(rec) = self.next_record()? {
                header.extend_from_slice(rec.bytes);
                let fields = rec.fields.to_vec();
                self.header = Some(header);
                self.header_fields = fields;
            }
            self.record_number = 0;
        }
        Ok(())
    }

    pub(crate) fn has_bom(&self) -> bool {
        self.has_bom
    }

    /// Header line as raw bytes, BOM included, when the dialect has a header.
    pub(crate) fn header(&self) -> Option<&[u8]> {
        self.header.as_deref()
    }

    /// Raw header fields (BOM excluded).
    pub(crate) fn header_fields(&self) -> Option<Vec<&[u8]>> {
        let header = self.header.as_deref()?;
        let line = if self.has_bom {
            &header[UTF8_BOM.len()..]
        } else {
            header
        };
        if line.is_empty() {
            return Some(Vec::new());
        }
        Some(
            (0..self.header_fields.len())
                .filter_map(|i| field_at(line, &self.header_fields, i))
                .collect(),
        )
    }

    /// Byte offset of the next record to be returned.
    pub(crate) fn position(&self) -> u64 {
        self.position
    }

    pub(crate) fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    /// Moves to `offset` (a record boundary) and continues numbering after `records_done`.
    pub(crate) fn seek(&mut self, offset: u64, records_done: u64) -> Result<(), Error> {
        let Reader::File(file) = &mut self.reader else {
            return Err(crate::error::ConfigError::ResumeRequiresPath.into());
        };
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| Error::io(self.path.as_deref(), e))?;
        self.start = 0;
        self.filled = 0;
        self.position = offset;
        self.record_number = records_done;
        Ok(())
    }

    pub(crate) fn next_record(&mut self) -> Result<Option<RawRecord<'_>>, Error> {
        loop {
            let len = match self.next_record_len()? {
                Some(len) => len,
                None => return Ok(None),
            };
            // Blank lines (only CR/LF bytes) are not records: skip them without numbering them.
            if self.buf[self.start..self.start + len]
                .iter()
                .all(|b| matches!(b, b'\r' | b'\n'))
            {
                self.start += len;
                self.position += len as u64;
                continue;
            }
            return Ok(Some(self.take_record(len)));
        }
    }

    /// Scans the next record and returns its length (terminator included), enforcing the size limit.
    fn next_record_len(&mut self) -> Result<Option<usize>, Error> {
        self.scanner.reset();
        let mut from = 0;
        let len = loop {
            match self.scanner.scan(&self.buf[self.start..self.filled], from) {
                ScanResult::Record { len } => break len,
                ScanResult::Malformed { offset } => {
                    return Err(self.data_error(offset as u64, DataErrorKind::Malformed));
                }
                ScanResult::NeedMore => {
                    from = self.filled - self.start;
                    if from > self.max_record_bytes {
                        return Err(self.too_large());
                    }
                    if !self.fill()? {
                        if from == 0 {
                            return Ok(None);
                        }
                        if !self.scanner.can_end_at_eof() {
                            return Err(self.data_error(0, DataErrorKind::Malformed));
                        }
                        break from;
                    }
                }
            }
        };
        if len > self.max_record_bytes {
            return Err(self.too_large());
        }
        Ok(Some(len))
    }

    fn too_large(&self) -> Error {
        let kind = DataErrorKind::RecordTooLarge {
            unclosed_quote: self.scanner.in_quotes(),
        };
        self.data_error(0, kind)
    }

    fn take_record(&mut self, len: usize) -> RawRecord<'_> {
        let offset = self.position;
        self.record_number += 1;
        self.position += len as u64;
        let start = self.start;
        self.start += len;
        RawRecord {
            bytes: &self.buf[start..start + len],
            fields: self.scanner.fields(),
            offset,
        }
    }

    fn data_error(&self, rel_offset: u64, kind: DataErrorKind) -> Error {
        Error::Data {
            record: self.record_number + 1,
            byte_offset: self.position + rel_offset,
            column: None,
            kind,
        }
    }

    /// Reads more bytes into the buffer. Returns false at EOF.
    fn fill(&mut self) -> Result<bool, Error> {
        if self.start > 0 {
            self.buf.copy_within(self.start..self.filled, 0);
            self.filled -= self.start;
            self.start = 0;
        }
        if self.filled == self.buf.len() {
            let new_len = (self.buf.len() * 2)
                .min(self.max_record_bytes.saturating_add(1))
                .max(self.buf.len() + 1);
            self.buf.resize(new_len, 0);
        }
        let n = self
            .reader
            .read(&mut self.buf[self.filled..])
            .map_err(|e| Error::io(self.path.as_deref(), e))?;
        self.filled += n;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn source(bytes: &'static [u8], has_header: bool, capacity: usize) -> CsvSource {
        let dialect = CsvDialect {
            has_header,
            max_record_bytes: 64,
            ..CsvDialect::default()
        };
        CsvSource::open_with_capacity(
            Input::Reader(Box::new(Cursor::new(bytes))),
            &dialect,
            capacity,
        )
        .unwrap()
    }

    fn collect(src: &mut CsvSource) -> Vec<(Vec<u8>, u64)> {
        let mut out = Vec::new();
        while let Some(rec) = src.next_record().unwrap() {
            out.push((rec.bytes.to_vec(), rec.offset));
        }
        out
    }

    #[test]
    fn header_includes_bom_and_records_report_absolute_offsets() {
        let mut src = source(b"\xEF\xBB\xBFid,name\n1,a\n2,b\n", true, 8);
        assert!(src.has_bom());
        assert_eq!(src.header(), Some(&b"\xEF\xBB\xBFid,name\n"[..]));
        assert_eq!(src.header_fields(), Some(vec![&b"id"[..], &b"name"[..]]));
        assert_eq!(src.position(), 11);
        assert_eq!(
            collect(&mut src),
            vec![(b"1,a\n".to_vec(), 11), (b"2,b\n".to_vec(), 15)]
        );
        assert_eq!(src.position(), 19);
    }

    #[test]
    fn records_larger_than_the_buffer_are_read_by_growing_it() {
        let mut src = source(b"\"abc,def\nghi\",x\r\n1,2", false, 4);
        assert_eq!(src.header(), None);
        let recs = collect(&mut src);
        assert_eq!(recs[0].0, b"\"abc,def\nghi\",x\r\n");
        assert_eq!(recs[1], (b"1,2".to_vec(), 17));
    }

    #[test]
    fn field_access_strips_the_line_terminator_only() {
        let mut src = source(b"a,\"b\",c\r\n", false, 16);
        let rec = src.next_record().unwrap().unwrap();
        assert_eq!(rec.field(1), Some(&b"\"b\""[..]));
        assert_eq!(rec.field(2), Some(&b"c"[..]));
        assert_eq!(rec.field(3), None);
    }

    #[test]
    fn oversized_record_reports_offset_and_unclosed_quote() {
        let mut src = source(
            b"ok\n\"never closed, ..........................................................\n",
            false,
            4,
        );
        src.next_record().unwrap();
        match src.next_record().unwrap_err() {
            Error::Data {
                record: 2,
                byte_offset: 3,
                kind:
                    DataErrorKind::RecordTooLarge {
                        unclosed_quote: true,
                    },
                ..
            } => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plain_long_record_inside_the_buffer_is_rejected_too() {
        // Larger than max_record_bytes (64) but smaller than the buffer: no refill is involved.
        let mut src = source(
            b"ok\n1,........................................................................\n",
            false,
            1024,
        );
        src.next_record().unwrap();
        match src.next_record().unwrap_err() {
            Error::Data {
                record: 2,
                byte_offset: 3,
                kind:
                    DataErrorKind::RecordTooLarge {
                        unclosed_quote: false,
                    },
                ..
            } => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn blank_lines_are_skipped_and_an_empty_input_has_no_header() {
        let mut src = source(b"\xEF\xBB\xBFid,v\r\n\r\n1,a\n\n\n2,b\n\r", true, 16);
        assert_eq!(src.header(), Some(&b"\xEF\xBB\xBFid,v\r\n"[..]));
        assert_eq!(
            collect(&mut src),
            vec![(b"1,a\n".to_vec(), 11), (b"2,b\n".to_vec(), 17)]
        );
        assert_eq!(src.position(), 22);
        let src = source(b"\xEF\xBB\xBF", true, 16);
        assert!(src.has_bom());
        assert_eq!(src.header(), None);
        assert_eq!(src.header_fields(), None);
    }

    #[test]
    fn unclosed_quote_at_eof_and_bad_byte_after_quote_are_malformed() {
        let mut src = source(b"\"abc", false, 16);
        assert!(matches!(
            src.next_record(),
            Err(Error::Data {
                kind: DataErrorKind::Malformed,
                ..
            })
        ));
        let mut src = source(b"x\n\"a\"b\n", false, 16);
        src.next_record().unwrap();
        assert!(matches!(
            src.next_record(),
            Err(Error::Data {
                record: 2,
                byte_offset: 5,
                kind: DataErrorKind::Malformed,
                ..
            })
        ));
    }

    #[test]
    fn seek_on_a_file_resumes_at_a_record_boundary() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"h\n1\n2\n3\n").unwrap();
        let mut src = CsvSource::open(
            Input::Path(file.path().to_path_buf()),
            &CsvDialect::default(),
        )
        .unwrap();
        assert_eq!(src.total_bytes(), Some(8));
        src.next_record().unwrap();
        src.seek(6, 2).unwrap();
        let rec = src.next_record().unwrap().unwrap();
        assert_eq!((rec.bytes, rec.offset), (&b"3\n"[..], 6));
        assert!(src.next_record().unwrap().is_none());
        assert!(matches!(
            src.seek(0, 0).and_then(|_| {
                src.next_record()?;
                src.next_record()?;
                src.next_record()?;
                src.next_record()
            }),
            Ok(Some(_))
        ));
    }
}
