//! Memory between runs: fresh-build baselines per profile, an append-only
//! history of actions, and a lock so only one run happens at a time.

mod history;
mod lock;
mod stats;

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};

pub use history::{History, HistoryEntry};
pub use lock::RunLock;
pub use stats::{Failure, Period, ProjectTotals, Stats, Totals};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    #[serde(with = "humantime_serde", skip_serializing_if = "Option::is_none")]
    pub last_run: Option<SystemTime>,
    /// Build output left after the last run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footprint: Option<Footprint>,
    /// Keyed by project root.
    pub projects: BTreeMap<PathBuf, ProjectState>,
}

/// How much build output purge is watching.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Footprint {
    pub projects: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectState {
    /// When purge last removed all build output.
    #[serde(with = "humantime_serde", skip_serializing_if = "Option::is_none")]
    pub fully_cleaned_at: Option<SystemTime>,
    /// Keyed by artifact group id.
    pub groups: BTreeMap<String, GroupState>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupState {
    /// When purge last removed or rebuilt this group. The user's first
    /// build after this sets (or, after a rebuild, raises) the baseline.
    #[serde(with = "humantime_serde", skip_serializing_if = "Option::is_none")]
    pub cleaned_at: Option<SystemTime>,
    /// Size of a fresh build of the group, as purge last measured it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_bytes: Option<u64>,
    /// When purge last rebuilt the group, whether or not that succeeded.
    #[serde(with = "humantime_serde", skip_serializing_if = "Option::is_none")]
    pub refreshed_at: Option<SystemTime>,
    /// When the user last built the group before that rebuild, so purge's
    /// own build does not count as the user using it.
    #[serde(with = "humantime_serde", skip_serializing_if = "Option::is_none")]
    pub used_before_refresh: Option<SystemTime>,
}

impl State {
    /// Loads `path`, or an empty state if it does not exist.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    /// Writes atomically, so a crash never leaves a truncated file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&tmp, path)
    }

    /// Forgets projects that no longer exist on disk.
    pub fn prune(&mut self) {
        self.projects.retain(|root, _| root.exists());
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/state.json");
        assert_eq!(State::load(&path).unwrap(), State::default());

        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut state = State {
            last_run: Some(at),
            ..State::default()
        };
        state
            .projects
            .entry(dir.path().to_path_buf())
            .or_default()
            .groups
            .insert(
                "debug".into(),
                GroupState {
                    cleaned_at: None,
                    baseline_bytes: Some(42),
                    refreshed_at: Some(at),
                    used_before_refresh: None,
                },
            );
        state.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap(), state);
    }

    #[test]
    fn corrupt_state_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, "{nope").unwrap();
        assert_eq!(
            State::load(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn prune_drops_missing_projects() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::default();
        state
            .projects
            .insert(dir.path().to_path_buf(), ProjectState::default());
        state
            .projects
            .insert("/definitely/not/here".into(), ProjectState::default());
        state.prune();
        assert_eq!(state.projects.len(), 1);
    }
}
