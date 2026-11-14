use std::{
    env,
    path::{Path, PathBuf},
};

use directories::{BaseDirs, ProjectDirs};

use crate::Error;

/// Where purge keeps its configuration and data.
///
/// | | Linux | macOS | Windows |
/// |---|---|---|---|
/// | config | `~/.config/purge` | `~/Library/Application Support/purge` | `%APPDATA%\purge\config` |
/// | data | `~/.local/share/purge` | `~/Library/Application Support/purge` | `%APPDATA%\purge\data` |
///
/// `PURGE_CONFIG` and `PURGE_DATA_DIR` override the defaults.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self, Error> {
        let dirs = ProjectDirs::from("", "", "purge").ok_or(Error::NoHome)?;
        let config_file = env::var_os("PURGE_CONFIG")
            .map_or_else(|| dirs.config_dir().join("config.toml"), PathBuf::from);
        let data_dir = env::var_os("PURGE_DATA_DIR")
            .map_or_else(|| dirs.data_dir().to_path_buf(), PathBuf::from);
        Ok(Self {
            config_file,
            data_dir,
        })
    }

    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }

    pub fn history_file(&self) -> PathBuf {
        self.data_dir.join("history.jsonl")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.data_dir.join("purge.lock")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
}

/// Replaces a leading `~` component with the home directory.
pub fn expand_tilde(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), BaseDirs::new()) {
        (Ok(rest), Some(dirs)) => dirs.home_dir().join(rest),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_only_a_leading_tilde_component() {
        let home = BaseDirs::new().unwrap().home_dir().to_path_buf();
        assert_eq!(expand_tilde(Path::new("~/Dev")), home.join("Dev"));
        assert_eq!(expand_tilde(Path::new("~")), home);
        assert_eq!(
            expand_tilde(Path::new("~bob/Dev")),
            PathBuf::from("~bob/Dev")
        );
        assert_eq!(expand_tilde(Path::new("/srv/~")), PathBuf::from("/srv/~"));
    }
}
