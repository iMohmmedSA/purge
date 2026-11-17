//! Runs purge for the user: the configuration and saved state on disk, the
//! run lock, and the history log, around the [`Engine`]. Shared by the CLI
//! and the daemon.

use std::{
    io,
    path::{Path, PathBuf},
};

use purge_config::{Config, Paths};
use purge_engine::{Engine, RunOptions, RunReport};
use purge_state::{History, RunLock, State};
use tracing::warn;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Config(#[from] purge_config::Error),
    #[error("cannot read {}", .path.display())]
    ReadState {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot save {}", .path.display())]
    SaveState {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write {}", .path.display())]
    WriteHistory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot take the run lock {}", .path.display())]
    Lock {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The user's configuration plus where purge keeps its files.
#[derive(Debug)]
pub struct Session {
    paths: Paths,
    config: Config,
}

impl Session {
    /// Loads the configuration from `paths`.
    pub fn open(paths: Paths) -> Result<Self> {
        let config = Config::load(&paths.config_file)?;
        Ok(Self { paths, config })
    }

    /// A session using the built-in defaults, for when the configuration
    /// cannot be loaded but its state and history are still of interest.
    pub fn with_defaults(paths: Paths) -> Self {
        Self {
            paths,
            config: Config::default(),
        }
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// What a run would do now, changing nothing. Scans `roots`, or the
    /// configured roots when `None`.
    pub fn preview(&self, roots: Option<&[PathBuf]>) -> Result<RunReport> {
        let ecosystems = purge_ecosystems::all();
        let engine = Engine::new(&ecosystems, &self.config)?;
        let options = RunOptions {
            dry_run: true,
            rebuild: true,
            max_rebuilds: self.config.schedule.max_rebuilds,
        };
        let mut state = self.load_state()?;
        Ok(engine.run(engine.discover(roots), &mut state, &options))
    }

    /// Applies the policy to the configured roots. With `rebuild` false,
    /// clean-and-rebuild actions are deferred. Returns `None` if another
    /// run holds the lock.
    pub fn run(&self, rebuild: bool) -> Result<Option<RunReport>> {
        // Fresh ecosystems for every run: they cache what they discover.
        let ecosystems = purge_ecosystems::all();
        let engine = Engine::new(&ecosystems, &self.config)?;
        let options = RunOptions {
            dry_run: false,
            rebuild,
            max_rebuilds: self.config.schedule.max_rebuilds,
        };
        self.with_saved_state(|state| engine.run(engine.discover(None), state, &options))
    }

    /// Runs `work` on the saved state while holding the run lock, then
    /// saves the state and appends the report to the history. Returns
    /// `None` if another run holds the lock.
    pub fn with_saved_state(
        &self,
        work: impl FnOnce(&mut State) -> RunReport,
    ) -> Result<Option<RunReport>> {
        let lock_path = self.paths.lock_file();
        let lock = RunLock::try_acquire(&lock_path).map_err(|source| Error::Lock {
            path: lock_path,
            source,
        })?;
        let Some(_lock) = lock else {
            return Ok(None);
        };
        let mut state = self.load_state()?;
        let report = work(&mut state);

        let state_path = self.paths.state_file();
        state.save(&state_path).map_err(|source| Error::SaveState {
            path: state_path,
            source,
        })?;
        let history = History::new(self.paths.history_file());
        history
            .append(&report.history())
            .map_err(|source| Error::WriteHistory {
                path: history.path().to_path_buf(),
                source,
            })?;
        Ok(Some(report))
    }

    /// The saved state. A damaged state file is reported and replaced by
    /// an empty state rather than stopping purge.
    pub fn load_state(&self) -> Result<State> {
        load_state(&self.paths.state_file())
    }

    /// Whether a run is in progress right now.
    pub fn is_running(&self) -> Result<bool> {
        let path = self.paths.lock_file();
        RunLock::is_held(&path).map_err(|source| Error::Lock { path, source })
    }

    pub fn history(&self) -> History {
        History::new(self.paths.history_file())
    }
}

fn load_state(path: &Path) -> Result<State> {
    match State::load(path) {
        Ok(state) => Ok(state),
        Err(err) if err.kind() == io::ErrorKind::InvalidData => {
            warn!(
                "state file {} is damaged, starting fresh: {err}",
                path.display()
            );
            Ok(State::default())
        }
        Err(source) => Err(Error::ReadState {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn session(dir: &Path) -> Session {
        Session::open(Paths {
            config_file: dir.join("config.toml"),
            data_dir: dir.join("data"),
        })
        .unwrap()
    }

    #[test]
    fn damaged_state_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let session = session(dir.path());
        fs::create_dir_all(&session.paths().data_dir).unwrap();
        fs::write(session.paths().state_file(), "{broken").unwrap();
        assert_eq!(session.load_state().unwrap(), State::default());
    }

    #[test]
    fn saved_state_waits_for_the_lock_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let session = session(dir.path());
        let report = || RunReport {
            started: std::time::SystemTime::UNIX_EPOCH,
            finished: std::time::SystemTime::UNIX_EPOCH,
            dry_run: false,
            units: Vec::new(),
            caches: Vec::new(),
        };

        let held = RunLock::try_acquire(&session.paths().lock_file()).unwrap();
        assert!(session.is_running().unwrap());
        assert!(session.with_saved_state(|_| report()).unwrap().is_none());
        drop(held);

        let done = session.with_saved_state(|state| {
            state.last_run = Some(std::time::SystemTime::UNIX_EPOCH);
            report()
        });
        assert!(done.unwrap().is_some());
        assert!(session.load_state().unwrap().last_run.is_some());
        assert!(!session.is_running().unwrap());
    }
}
