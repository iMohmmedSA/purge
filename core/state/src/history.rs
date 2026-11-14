use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};

/// One action purge took, as recorded in the history file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    #[serde(with = "humantime_serde")]
    pub at: SystemTime,
    pub project: PathBuf,
    pub ecosystem: String,
    /// `clean-all`, `clean` or `refresh`.
    pub action: String,
    /// Artifact group the action applied to, if not the whole project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub reason: String,
    pub freed_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// An append-only JSON Lines log of [`HistoryEntry`].
#[derive(Debug, Clone)]
pub struct History {
    path: PathBuf,
}

impl History {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, entries: &[HistoryEntry]) -> io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut buffer = Vec::new();
        for entry in entries {
            serde_json::to_writer(&mut buffer, entry)?;
            buffer.push(b'\n');
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?
            .write_all(&buffer)
    }

    /// All readable entries, oldest first. Damaged lines are skipped.
    pub fn read(&self) -> io::Result<Vec<HistoryEntry>> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let mut entries = Vec::new();
        for line in BufReader::new(file).split(b'\n') {
            if let Ok(entry) = serde_json::from_slice(&line?) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(freed: u64) -> HistoryEntry {
        HistoryEntry {
            at: SystemTime::UNIX_EPOCH,
            project: "/dev/app".into(),
            ecosystem: "rust".into(),
            action: "clean".into(),
            group: Some("debug".into()),
            reason: "not built for 20d".into(),
            freed_bytes: freed,
            error: None,
        }
    }

    #[test]
    fn appends_and_reads_back_skipping_damage() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(dir.path().join("history.jsonl"));
        assert!(history.read().unwrap().is_empty());
        history.append(&[entry(1)]).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(history.path())
            .unwrap()
            .write_all(b"garbage\n\xff\xfe not utf-8\n")
            .unwrap();
        history.append(&[entry(2)]).unwrap();
        assert_eq!(history.read().unwrap(), [entry(1), entry(2)]);
    }
}
