//! Checkpoint file: what split has durably written so far, and how to resume from it.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::SplitConfig;
use crate::consumer::Counts;
use crate::error::{Error, ResumeRejection};
use crate::range::{Bound, Router};
use crate::report::RangeResult;
use crate::sink::{Destination, OutputLength};

pub(crate) const FILE_NAME: &str = ".ksplit-checkpoint.json";
const SCHEMA_VERSION: u32 = 1;
const HASH_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub schema_version: u32,
    pub tool_version: String,
    pub config_hash: String,
    pub input: InputIdentity,
    pub input_offset: u64,
    pub records_done: u64,
    pub outputs: Vec<OutputEntry>,
    pub saved_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InputIdentity {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_unix_nanos: i128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutputRole {
    Range,
    Unmatched,
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OutputEntry {
    pub role: OutputRole,
    pub name: String,
    pub file: String,
    pub bytes: u64,
    pub records: u64,
}

impl InputIdentity {
    pub(crate) fn of(path: &Path) -> Result<Self, Error> {
        let canonical = fs::canonicalize(path).map_err(|e| Error::io(Some(path), e))?;
        let meta = fs::metadata(&canonical).map_err(|e| Error::io(Some(path), e))?;
        let mtime = meta.modified().map_err(|e| Error::io(Some(path), e))?;
        let nanos = match mtime.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        };
        Ok(Self {
            path: canonical,
            size: meta.len(),
            mtime_unix_nanos: nanos,
        })
    }

    pub(crate) fn check_against(&self, saved: &Self) -> Result<(), ResumeRejection> {
        let mismatch = |field, saved: String, current: String| {
            Err(ResumeRejection::InputMismatch {
                field,
                saved,
                current,
            })
        };
        if self.path != saved.path {
            return mismatch(
                "path",
                saved.path.display().to_string(),
                self.path.display().to_string(),
            );
        }
        if self.size != saved.size {
            return mismatch("size", saved.size.to_string(), self.size.to_string());
        }
        if self.mtime_unix_nanos != saved.mtime_unix_nanos {
            return mismatch(
                "mtime",
                saved.mtime_unix_nanos.to_string(),
                self.mtime_unix_nanos.to_string(),
            );
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct HashedRange<'a> {
    name: &'a str,
    lo: Option<String>,
    hi: Option<String>,
}

#[derive(Serialize)]
struct HashedConfig<'a> {
    hash_version: u32,
    dialect: &'a crate::config::CsvDialect,
    encoding: crate::config::Encoding,
    key_selector: &'a crate::key::KeySelector,
    key_type: &'a crate::key::KeyType,
    trim: bool,
    ranges: Vec<HashedRange<'a>>,
    mode: crate::config::Mode,
    on_invalid: crate::config::OnInvalid,
}

/// Hash of every setting that affects the output bytes.
pub(crate) fn config_hash(config: &SplitConfig, router: &Router) -> String {
    let bound = |b: &Bound| match b {
        Bound::Unbounded => None,
        Bound::Value(v) => Some(v.canonical()),
    };
    let ranges = router
        .ranges()
        .iter()
        .zip(router.output_names())
        .map(|(r, name)| HashedRange {
            name,
            lo: bound(&r.lo),
            hi: bound(&r.hi),
        })
        .collect();
    let hashed = HashedConfig {
        hash_version: HASH_VERSION,
        dialect: &config.dialect,
        encoding: config.encoding,
        key_selector: &config.key.selector,
        key_type: &config.key.key_type,
        trim: config.key.trim,
        ranges,
        mode: config.mode,
        on_invalid: config.on_invalid,
    };
    let json = serde_json::to_vec(&hashed).expect("config is serializable");
    format!("sha256:{:x}", Sha256::digest(json))
}

pub(crate) fn tool_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

impl Checkpoint {
    pub(crate) fn new(
        config_hash: String,
        input: InputIdentity,
        input_offset: u64,
        records_done: u64,
        outputs: Vec<OutputEntry>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            tool_version: tool_version(),
            config_hash,
            input,
            input_offset,
            records_done,
            outputs,
            saved_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }
    }

    pub(crate) fn entries(
        lengths: &[OutputLength],
        router: &Router,
        counts: &Counts,
    ) -> Vec<OutputEntry> {
        lengths
            .iter()
            .map(|l| {
                let (role, name, records) = match l.dest {
                    Destination::Range(i) => (
                        OutputRole::Range,
                        router.ranges()[i].name.clone(),
                        counts.per_range[i],
                    ),
                    Destination::Unmatched => (
                        OutputRole::Unmatched,
                        crate::range::UNMATCHED_NAME.to_string(),
                        counts.unmatched,
                    ),
                    Destination::Invalid => (
                        OutputRole::Invalid,
                        crate::range::INVALID_NAME.to_string(),
                        counts.invalid,
                    ),
                };
                OutputEntry {
                    role,
                    name,
                    file: l.file.clone(),
                    bytes: l.bytes,
                    records,
                }
            })
            .collect()
    }

    /// Validates version and config, returning the checkpoint for resume.
    pub(crate) fn check_compatible(&self, config_hash: &str) -> Result<(), ResumeRejection> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ResumeRejection::CorruptCheckpoint(format!(
                "unsupported schema_version {}",
                self.schema_version
            )));
        }
        if self.tool_version != tool_version() {
            return Err(ResumeRejection::VersionMismatch {
                saved: self.tool_version.clone(),
                current: tool_version(),
            });
        }
        if self.config_hash != config_hash {
            return Err(ResumeRejection::ConfigMismatch);
        }
        Ok(())
    }

    pub(crate) fn lengths(&self) -> Vec<OutputLength> {
        let mut range_index = 0;
        self.outputs
            .iter()
            .map(|e| {
                let dest = match e.role {
                    OutputRole::Range => {
                        range_index += 1;
                        Destination::Range(range_index - 1)
                    }
                    OutputRole::Unmatched => Destination::Unmatched,
                    OutputRole::Invalid => Destination::Invalid,
                };
                OutputLength {
                    dest,
                    file: e.file.clone(),
                    bytes: e.bytes,
                }
            })
            .collect()
    }

    pub(crate) fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for e in &self.outputs {
            match e.role {
                OutputRole::Range => counts.per_range.push(e.records),
                OutputRole::Unmatched => counts.unmatched = e.records,
                OutputRole::Invalid => counts.invalid = e.records,
            }
        }
        counts
    }
}

/// Writes the checkpoint durably: temp file, fsync, rename, directory fsync.
pub(crate) fn write(dir: &Path, checkpoint: &Checkpoint) -> Result<(), Error> {
    let tmp = dir.join(format!("{FILE_NAME}.tmp"));
    let path = dir.join(FILE_NAME);
    let json = serde_json::to_vec_pretty(checkpoint).expect("checkpoint is serializable");
    // Sync through the writable handle: on Windows, flushing a read-only handle is denied.
    File::create(&tmp)
        .and_then(|mut f| {
            f.write_all(&json)?;
            f.sync_all()
        })
        .map_err(|e| Error::io(Some(&tmp), e))?;
    fs::rename(&tmp, &path).map_err(|e| Error::io(Some(&path), e))?;
    #[cfg(unix)]
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| Error::io(Some(dir), e))?;
    Ok(())
}

pub(crate) fn read(dir: &Path) -> Result<Option<Checkpoint>, Error> {
    let path = dir.join(FILE_NAME);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(Some(&path), e)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| Error::ResumeRejected(ResumeRejection::CorruptCheckpoint(e.to_string())))
}

pub(crate) fn remove(dir: &Path) -> Result<(), Error> {
    let path = dir.join(FILE_NAME);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(Some(&path), e)),
    }
}

/// What `status` reports about an interrupted run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CheckpointStatus {
    pub schema_version: u32,
    pub tool_version: String,
    pub input_path: PathBuf,
    pub input_unchanged: bool,
    pub input_change: Option<String>,
    pub bytes_done: u64,
    pub total_bytes: u64,
    pub records_done: u64,
    pub per_range: Vec<RangeResult>,
    pub unmatched: Option<u64>,
    pub invalid: Option<u64>,
    pub saved_at: String,
}

/// Reads the checkpoint in `output_dir`, if any, and checks whether the input is unchanged.
pub fn read_checkpoint(output_dir: &Path) -> Result<Option<CheckpointStatus>, Error> {
    let Some(cp) = read(output_dir)? else {
        return Ok(None);
    };
    let input_change = match InputIdentity::of(&cp.input.path) {
        Ok(current) => current
            .check_against(&cp.input)
            .err()
            .map(|e| e.to_string()),
        Err(_) => Some("input file is missing".to_string()),
    };
    let mut per_range = Vec::new();
    let (mut unmatched, mut invalid) = (None, None);
    for e in &cp.outputs {
        match e.role {
            OutputRole::Range => per_range.push(RangeResult {
                name: e.name.clone(),
                path: Some(output_dir.join(&e.file)),
                records: e.records,
            }),
            OutputRole::Unmatched => unmatched = Some(e.records),
            OutputRole::Invalid => invalid = Some(e.records),
        }
    }
    Ok(Some(CheckpointStatus {
        schema_version: crate::report::SCHEMA_VERSION,
        tool_version: cp.tool_version,
        input_path: cp.input.path,
        input_unchanged: input_change.is_none(),
        input_change,
        bytes_done: cp.input_offset,
        total_bytes: cp.input.size,
        records_done: cp.records_done,
        per_range,
        unmatched,
        invalid,
        saved_at: cp.saved_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let input = InputIdentity {
            path: dir.path().join("in.csv"),
            size: 10,
            mtime_unix_nanos: 42,
        };
        let entry = OutputEntry {
            role: OutputRole::Range,
            name: "a".into(),
            file: "a.csv".into(),
            bytes: 7,
            records: 1,
        };
        let checkpoint = Checkpoint::new("sha256:x".into(), input, 5, 1, vec![entry]);
        write(dir.path(), &checkpoint).unwrap();
        write(dir.path(), &checkpoint).unwrap();
        assert!(!dir.path().join(format!("{FILE_NAME}.tmp")).exists());
        let back = read(dir.path()).unwrap().unwrap();
        assert_eq!(
            (back.input_offset, back.records_done, back.outputs.len()),
            (5, 1, 1)
        );
        remove(dir.path()).unwrap();
        assert!(read(dir.path()).unwrap().is_none());
    }
}
