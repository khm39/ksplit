use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::{Destination, Sink};
use crate::config::Mode;
use crate::error::{ConfigError, Error, ResumeRejection};
use crate::range::{INVALID_NAME, UNMATCHED_NAME};
use crate::source::RawRecord;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OutputLength {
    pub dest: Destination,
    pub file: String,
    pub bytes: u64,
}

struct Output {
    path: PathBuf,
    writer: BufWriter<File>,
    bytes: u64,
}

impl Output {
    fn file_name(&self) -> String {
        self.path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

/// Writes records to one file per destination; used by `split`.
pub(crate) struct FileSink {
    ranges: Vec<Output>,
    unmatched: Option<Output>,
    invalid: Option<Output>,
}

pub(crate) struct Layout<'a> {
    pub dir: &'a Path,
    pub range_names: &'a [String],
    pub mode: Mode,
    pub write_invalid: bool,
}

impl Layout<'_> {
    pub(crate) fn file_name(name: &str) -> String {
        format!("{name}.csv")
    }

    pub(crate) fn range_paths(&self) -> Vec<PathBuf> {
        self.range_names
            .iter()
            .map(|n| self.dir.join(Self::file_name(n)))
            .collect()
    }

    pub(crate) fn unmatched_path(&self) -> Option<PathBuf> {
        (self.mode == Mode::Split).then(|| self.dir.join(Self::file_name(UNMATCHED_NAME)))
    }

    pub(crate) fn invalid_path(&self) -> Option<PathBuf> {
        self.write_invalid
            .then(|| self.dir.join(Self::file_name(INVALID_NAME)))
    }

    pub(crate) fn all_paths(&self) -> Vec<PathBuf> {
        let mut paths = self.range_paths();
        paths.extend(self.unmatched_path());
        paths.extend(self.invalid_path());
        paths
    }
}

impl FileSink {
    /// Creates all output files, refusing to touch existing ones unless `overwrite`.
    pub(crate) fn create(
        layout: &Layout<'_>,
        header: &[u8],
        buffer: usize,
        overwrite: bool,
    ) -> Result<Self, Error> {
        fs::create_dir_all(layout.dir).map_err(|e| Error::io(Some(layout.dir), e))?;
        if !overwrite {
            if let Some(existing) = layout.all_paths().into_iter().find(|p| p.exists()) {
                return Err(ConfigError::OutputExists(existing).into());
            }
        }
        let open = |path: PathBuf| -> Result<Output, Error> {
            let mut opts = OpenOptions::new();
            opts.write(true);
            if overwrite {
                opts.create(true).truncate(true);
            } else {
                opts.create_new(true);
            }
            let file = opts.open(&path).map_err(|e| Error::io(Some(&path), e))?;
            let mut writer = BufWriter::with_capacity(buffer, file);
            writer
                .write_all(header)
                .map_err(|e| Error::io(Some(&path), e))?;
            Ok(Output {
                path,
                writer,
                bytes: header.len() as u64,
            })
        };
        Ok(Self {
            ranges: layout
                .range_paths()
                .into_iter()
                .map(open)
                .collect::<Result<_, _>>()?,
            unmatched: layout.unmatched_path().map(open).transpose()?,
            invalid: layout.invalid_path().map(open).transpose()?,
        })
    }

    /// Reopens outputs at the lengths a checkpoint recorded, truncating anything written after it.
    pub(crate) fn reopen(
        layout: &Layout<'_>,
        recorded_bytes: impl Fn(&str) -> Option<u64>,
        buffer: usize,
    ) -> Result<Self, Error> {
        let reopen = |path: PathBuf| -> Result<Output, Error> {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let recorded = recorded_bytes(&name).ok_or_else(|| {
                Error::ResumeRejected(ResumeRejection::CorruptCheckpoint(format!(
                    "no entry for {name}"
                )))
            })?;
            let actual = fs::metadata(&path).map(|m| m.len()).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Error::ResumeRejected(ResumeRejection::OutputMissing(name.clone()))
                } else {
                    Error::io(Some(&path), e)
                }
            })?;
            if actual < recorded {
                return Err(Error::ResumeRejected(ResumeRejection::OutputTruncated {
                    file: name,
                    expected: recorded,
                    actual,
                }));
            }
            let mut file = OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(|e| Error::io(Some(&path), e))?;
            file.set_len(recorded)
                .map_err(|e| Error::io(Some(&path), e))?;
            file.seek(SeekFrom::End(0))
                .map_err(|e| Error::io(Some(&path), e))?;
            Ok(Output {
                path,
                writer: BufWriter::with_capacity(buffer, file),
                bytes: recorded,
            })
        };
        Ok(Self {
            ranges: layout
                .range_paths()
                .into_iter()
                .map(reopen)
                .collect::<Result<_, _>>()?,
            unmatched: layout.unmatched_path().map(reopen).transpose()?,
            invalid: layout.invalid_path().map(reopen).transpose()?,
        })
    }

    fn outputs(&mut self) -> impl Iterator<Item = (Destination, &mut Output)> {
        self.ranges
            .iter_mut()
            .enumerate()
            .map(|(i, o)| (Destination::Range(i), o))
            .chain(
                self.unmatched
                    .iter_mut()
                    .map(|o| (Destination::Unmatched, o)),
            )
            .chain(self.invalid.iter_mut().map(|o| (Destination::Invalid, o)))
    }

    fn flush_all(&mut self) -> Result<(), Error> {
        for (_, out) in self.outputs() {
            out.writer
                .flush()
                .map_err(|e| Error::io(Some(&out.path), e))?;
            out.writer
                .get_ref()
                .sync_all()
                .map_err(|e| Error::io(Some(&out.path), e))?;
        }
        Ok(())
    }
}

impl Sink for FileSink {
    fn accept(&mut self, record: &RawRecord<'_>, dest: Destination) -> Result<(), Error> {
        let out = match dest {
            Destination::Range(i) => &mut self.ranges[i],
            Destination::Unmatched => self
                .unmatched
                .as_mut()
                .expect("unmatched output exists in split mode"),
            Destination::Invalid => self
                .invalid
                .as_mut()
                .expect("invalid output exists unless strict"),
        };
        out.writer
            .write_all(record.bytes)
            .map_err(|e| Error::io(Some(&out.path), e))?;
        out.bytes += record.bytes.len() as u64;
        Ok(())
    }

    fn persist(&mut self) -> Result<Vec<OutputLength>, Error> {
        self.flush_all()?;
        Ok(self
            .outputs()
            .map(|(dest, o)| OutputLength {
                dest,
                file: o.file_name(),
                bytes: o.bytes,
            })
            .collect())
    }

    fn finish(&mut self) -> Result<Vec<Option<PathBuf>>, Error> {
        self.flush_all()?;
        Ok(self.ranges.iter().map(|o| Some(o.path.clone())).collect())
    }
}
