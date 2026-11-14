//! Configuration: which folders to watch, when to run, and how aggressively
//! to clean. Loaded from a TOML file; see [`TEMPLATE`] for every option.

mod paths;
mod policy;
mod resolver;
mod schedule;

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::Deserialize;

pub use paths::{Paths, expand_tilde};
pub use policy::{Policy, PolicyPatch, ProfilePatch};
pub use resolver::Resolver;
pub use schedule::{Schedule, TimeWindow};

/// Commented starter configuration written by `purge config init`. The
/// `roots` line is a placeholder filled in by [`Config::template`].
pub const TEMPLATE: &str = include_str!("template.toml");

const ROOTS_PLACEHOLDER: &str = "roots = []";

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Folders searched for projects.
    pub roots: Vec<PathBuf>,
    /// Paths or globs that are never searched.
    pub exclude: Vec<String>,
    /// How many directory levels below each root are searched.
    pub max_depth: usize,
    pub schedule: Schedule,
    pub notify: Notify,
    /// Defaults for every project.
    pub policy: PolicyPatch,
    /// Overrides for matching projects, applied in order after `policy`.
    #[serde(rename = "project")]
    pub projects: Vec<ProjectRule>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            exclude: Vec::new(),
            max_depth: 6,
            schedule: Schedule::default(),
            notify: Notify::default(),
            policy: PolicyPatch::default(),
            projects: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Notify {
    /// Show a desktop notification after a run that changed something.
    pub enabled: bool,
}

impl Default for Notify {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Policy overrides for projects whose root matches `path`, either a path
/// prefix or a glob such as `~/Dev/work/**`. In the file, `path` sits next
/// to the policy keys it overrides.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "toml::Table")]
pub struct ProjectRule {
    pub path: String,
    pub policy: PolicyPatch,
}

impl TryFrom<toml::Table> for ProjectRule {
    type Error = String;

    // Split by hand rather than `#[serde(flatten)]`, which would silently
    // accept misspelled policy keys.
    fn try_from(mut table: toml::Table) -> Result<Self, Self::Error> {
        let path = match table.remove("path") {
            Some(toml::Value::String(path)) => path,
            Some(_) => return Err("`path` must be a string".into()),
            None => return Err("every [[project]] needs a `path`".into()),
        };
        let policy = toml::Value::Table(table)
            .try_into()
            .map_err(|err: toml::de::Error| err.message().to_owned())?;
        Ok(Self { path, policy })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read {}", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid configuration in {}", .path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid path pattern `{pattern}`")]
    Pattern {
        pattern: String,
        #[source]
        source: globset::Error,
    },
    #[error("{0}")]
    Invalid(String),
    #[error("could not determine the home directory")]
    NoHome,
}

impl Config {
    /// Loads `path`, or the defaults if it does not exist.
    pub fn load(path: &Path) -> Result<Self, Error> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let config = Self::parse(&text).map_err(|source| Error::Parse {
                    path: path.to_path_buf(),
                    source,
                })?;
                config.validate().map_err(|reason| {
                    Error::Invalid(format!(
                        "invalid configuration in {}: {reason}",
                        path.display()
                    ))
                })?;
                Ok(config)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        let mut config: Self = toml::from_str(text)?;
        config.roots = config.roots.iter().map(|root| expand_tilde(root)).collect();
        Ok(config)
    }

    /// Checks what parsing alone cannot.
    fn validate(&self) -> Result<(), String> {
        if let Some(root) = self.roots.iter().find(|root| !root.is_absolute()) {
            return Err(format!(
                "root `{}` must be an absolute path or start with `~`",
                root.display()
            ));
        }
        self.schedule.validate()
    }

    /// The starter configuration with `roots` filled in.
    pub fn template(roots: &[PathBuf]) -> String {
        let roots = roots
            .iter()
            .map(|root| toml::Value::from(root.to_string_lossy().into_owned()).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        TEMPLATE.replacen(ROOTS_PLACEHOLDER, &format!("roots = [{roots}]"), 1)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn template_parses_with_filled_roots() {
        assert!(TEMPLATE.contains(ROOTS_PLACEHOLDER));
        let text = Config::template(&[
            PathBuf::from("/home/me/Dev"),
            PathBuf::from(r"C:\Users\me\Dev"),
        ]);
        let config = Config::parse(&text).unwrap();
        assert_eq!(
            config.roots,
            [
                PathBuf::from("/home/me/Dev"),
                PathBuf::from(r"C:\Users\me\Dev")
            ]
        );
        Resolver::new(&config).unwrap();
    }

    #[test]
    fn empty_config_uses_defaults() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.roots, [] as [std::path::PathBuf; 0]);
        assert!(config.notify.enabled);
        assert_eq!(config.schedule.interval, Duration::from_hours(6));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Config::parse("rootz = []").is_err());
        assert!(Config::parse("[schedule]\nintervall = \"1h\"").is_err());
        assert!(Config::parse("[policy]\nmax_ag = \"1d\"").is_err());
        let typo = Config::parse("[[project]]\npath = \"/x\"\nmax_ag = \"1d\"").unwrap_err();
        assert!(typo.to_string().contains("max_ag"), "{typo}");
        assert!(Config::parse("[[project]]\nmax_age = \"1d\"").is_err());
    }

    #[test]
    fn parses_overrides() {
        let config = Config::parse(
            r#"
            roots = ["/dev"]
            [policy]
            max_age = "10d"
            min_savings = "1 GB"
            [policy.profiles.release]
            max_age = "30d"

            [[project]]
            path = "/dev/big/**"
            dormant_after = "60d"
            build_args = ["--workspace", "--all-features"]
            "#,
        )
        .unwrap();
        assert_eq!(config.policy.max_age, Some(Duration::from_hours(240)));
        assert_eq!(
            config.policy.min_savings.map(|b| b.as_u64()),
            Some(1_000_000_000)
        );
        assert_eq!(config.projects.len(), 1);
        assert_eq!(
            config.projects[0].policy.dormant_after,
            Some(Duration::from_hours(1440))
        );
    }

    #[test]
    fn rejects_relative_roots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, r#"roots = ["Dev"]"#).unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(err.to_string().contains("root `Dev`"), "{err}");
        fs::write(&path, r#"roots = ["~/Dev"]"#).unwrap();
        assert!(Config::load(&path).is_ok());
    }

    #[test]
    fn missing_file_means_defaults() {
        let config = Config::load(Path::new("/definitely/not/here.toml")).unwrap();
        assert_eq!(config.roots, [] as [std::path::PathBuf; 0]);
    }
}
