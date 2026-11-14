//! Rust support: finds Cargo workspaces, measures their build output per
//! profile, and cleans, trims or rebuilds it.

mod artifacts;
mod layout;
mod units;

use std::{
    collections::HashMap,
    env,
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, SystemTime},
};

use purge_ecosystem::{
    ArtifactGroup, Artifacts, Ecosystem, Error, Project, Result, command, fsutil,
};
use purge_process::{ProcessTable, configure_background};
use tracing::{debug, warn};

use crate::{
    artifacts::{BUILD_LOCKS, is_profile_dir},
    layout::{Layout, Metadata},
};

pub(crate) const NAME: &str = "rust";

/// Processes that mean someone is building or editing the project.
const TOOLS: &[&str] = &[
    "cargo",
    "rustc",
    "rustdoc",
    "clippy-driver",
    "rust-analyzer",
];

/// Time limits for the cargo commands purge runs. Generous: they only exist
/// so a hung command (say, a build script waiting on the network) cannot
/// stall purge and hold its lock forever.
const METADATA_TIMEOUT: Duration = Duration::from_mins(5);
const CLEAN_TIMEOUT: Duration = Duration::from_mins(30);
const BUILD_TIMEOUT: Duration = Duration::from_hours(3);

/// Git files whose modification marks commits, checkouts and staging.
const GIT_MARKERS: &[&str] = &[".git/HEAD", ".git/index", ".git/logs/HEAD"];

/// Cargo support. Create one per scan: it remembers the workspaces it has
/// seen so that workspace members cost no extra `cargo metadata` call.
#[derive(Debug)]
pub struct Rust {
    cargo: PathBuf,
    /// Package directory -> the project it belongs to (`None`: nothing built).
    known: Mutex<HashMap<PathBuf, Option<Project>>>,
    /// Artifact directory -> whether its filesystem records access times.
    access_times: Mutex<HashMap<PathBuf, bool>>,
}

impl Default for Rust {
    fn default() -> Self {
        Self {
            cargo: locate_cargo(),
            known: Mutex::default(),
            access_times: Mutex::default(),
        }
    }
}

impl Rust {
    pub fn new() -> Self {
        Self::default()
    }

    fn cargo(&self, dir: &Path) -> Command {
        let mut cmd = Command::new(&self.cargo);
        cmd.current_dir(dir)
            // Never download toolchains named by rust-toolchain.toml behind the user's back.
            .env("RUSTUP_AUTO_INSTALL", "0")
            .env("CARGO_TERM_COLOR", "never");
        configure_background(&mut cmd);
        cmd
    }

    fn known(&self) -> MutexGuard<'_, HashMap<PathBuf, Option<Project>>> {
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Where the package in `dir` belongs, per `cargo metadata`. A broken
    /// manifest falls back to the conventional layout, since its `target/`
    /// is still worth cleaning.
    fn layout(&self, dir: &Path) -> Option<Layout> {
        match self.metadata(dir) {
            Ok(meta) => Some(meta.into()),
            Err(err) => {
                debug!(dir = %dir.display(), "cargo metadata failed, assuming default layout: {err}");
                let layout = Layout::conventional(dir);
                fsutil::is_cache_dir(&layout.target).then_some(layout)
            }
        }
    }

    fn metadata(&self, dir: &Path) -> Result<Metadata> {
        let output = command::run(
            self.cargo(dir).args([
                "metadata",
                "--no-deps",
                "--offline",
                "--format-version",
                "1",
            ]),
            METADATA_TIMEOUT,
        )?;
        serde_json::from_slice(&output.stdout)
            .map_err(|err| Error::Invalid(format!("unexpected `cargo metadata` output: {err}")))
    }

    /// Whether access times in `dir` show when Cargo last used a unit,
    /// probed once per directory.
    fn records_access_time(&self, dir: &Path) -> bool {
        let mut probed = self
            .access_times
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *probed
            .entry(dir.to_path_buf())
            .or_insert_with(|| fsutil::records_access_time(dir))
    }
}

impl Ecosystem for Rust {
    fn name(&self) -> &'static str {
        NAME
    }

    fn detect(&self, dir: &Path) -> Result<Option<Project>> {
        if !dir.join("Cargo.toml").is_file() {
            return Ok(None);
        }
        if let Some(project) = self.known().get(dir) {
            return Ok(project.clone());
        }
        let Some(layout) = self.layout(dir) else {
            return Ok(None);
        };
        let members = layout.members.clone();
        let project = layout.project();
        let mut known = self.known();
        for member in members.into_iter().chain([dir.to_path_buf()]) {
            known.insert(member, project.clone());
        }
        Ok(project)
    }

    fn source_modified(&self, project: &Project) -> Option<SystemTime> {
        let tree = fsutil::newest_mtime(&project.root, &|dir| {
            project.artifact_dirs().any(|output| output == dir) || fsutil::is_ignored_dir(dir)
        });
        let git = GIT_MARKERS
            .iter()
            .filter_map(|marker| {
                fs::metadata(project.root.join(marker))
                    .and_then(|m| m.modified())
                    .ok()
            })
            .max();
        tree.max(git)
    }

    fn inspect(&self, project: &Project) -> Result<Artifacts> {
        let used_times = self.records_access_time(&project.artifact_dir);
        Ok(artifacts::inspect(project, used_times)?)
    }

    fn busy(&self, project: &Project, processes: &ProcessTable) -> Option<String> {
        if let Some(process) = processes.find(TOOLS, &project.root) {
            return Some(format!("`{}` is running in the project", process.name));
        }
        if project.artifact_dirs().any(build_lock_held) {
            return Some("a cargo build holds the target directory lock".into());
        }
        None
    }

    fn clean_all(&self, project: &Project) -> Result<()> {
        match command::run(self.cargo(&project.root).arg("clean"), CLEAN_TIMEOUT) {
            Ok(_) => Ok(()),
            Err(err) if project.artifact_dirs().all(fsutil::is_cache_dir) => {
                warn!(project = %project.root.display(), "cargo clean failed, removing build output directly: {err}");
                for dir in project.artifact_dirs() {
                    fs::remove_dir_all(dir)?;
                }
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn clean_group(&self, project: &Project, group: &ArtifactGroup) -> Result<()> {
        if group.rebuildable {
            let mut cmd = self.cargo(&project.root);
            cmd.arg("clean").args(profile_args(group));
            if let Err(err) = command::run(&mut cmd, CLEAN_TIMEOUT) {
                warn!(project = %project.root.display(), group = group.id, "cargo clean failed, removing profile directly: {err}");
            }
        }
        // Cargo succeeds without removing anything when it does not know
        // the directory, so make sure the profile is gone.
        for path in owned_paths(project, group)? {
            if path.is_dir() {
                fs::remove_dir_all(path)?;
            }
        }
        Ok(())
    }

    fn trim(&self, project: &Project, group: &ArtifactGroup, parts: &[String]) -> Result<()> {
        let paths: Vec<PathBuf> = owned_paths(project, group)?.cloned().collect();
        Ok(units::trim(&paths, parts)?)
    }

    fn rebuild_group(
        &self,
        project: &Project,
        group: &ArtifactGroup,
        extra_args: &[String],
    ) -> Result<()> {
        if !group.rebuildable {
            return Err(Error::Invalid(format!(
                "cargo cannot rebuild `{}`",
                group.id
            )));
        }
        let mut cmd = self.cargo(&project.root);
        // `--locked`: never rewrite the user's Cargo.lock behind their back.
        cmd.arg("build")
            .arg("--locked")
            .args(profile_args(group))
            .args(extra_args);
        command::run(&mut cmd, BUILD_TIMEOUT).map(drop)
    }
}

/// The group's directories, after checking each lies inside one of the
/// project's artifact directories: purge removes nothing anywhere else.
fn owned_paths<'g>(
    project: &Project,
    group: &'g ArtifactGroup,
) -> Result<impl Iterator<Item = &'g PathBuf>> {
    for path in &group.paths {
        let inside = project
            .artifact_dirs()
            .any(|dir| path.starts_with(dir) && path != dir);
        if !inside {
            return Err(Error::Invalid(format!(
                "{} is not inside the project's build output",
                path.display()
            )));
        }
    }
    Ok(group.paths.iter())
}

/// Profile directories directly inside `dir`.
fn profile_dirs(dir: &Path) -> impl Iterator<Item = PathBuf> + use<> {
    fsutil::children(dir).filter(|path| is_profile_dir(path))
}

/// `--profile` (and `--target` for cross builds) selecting one group.
fn profile_args(group: &ArtifactGroup) -> Vec<String> {
    // The `dev` profile builds into `debug/`; every other profile uses its own name.
    let profile = if group.profile == "debug" {
        "dev"
    } else {
        &group.profile
    };
    let mut args = vec!["--profile".to_owned(), profile.to_owned()];
    if let Some((triple, _)) = group.id.split_once('/') {
        args.extend(["--target".to_owned(), triple.to_owned()]);
    }
    args
}

/// Whether a build currently holds any of Cargo's locks under `target`.
fn build_lock_held(target: &Path) -> bool {
    fsutil::children(target)
        .filter(|path| path.is_dir())
        .flat_map(|path| profile_dirs(&path).chain([path]))
        .any(|dir| BUILD_LOCKS.iter().any(|lock| lock_held(&dir.join(lock))))
}

fn lock_held(path: &Path) -> bool {
    let Ok(file) = File::options().read(true).write(true).open(path) else {
        return false;
    };
    matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock))
}

/// Finds `cargo` on `PATH`, falling back to rustup's default location so the
/// daemon works even when started with a minimal environment.
fn locate_cargo() -> PathBuf {
    let exe = format!("cargo{}", env::consts::EXE_SUFFIX);
    env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(&exe))
        .chain(env::home_dir().map(|home| home.join(".cargo/bin").join(&exe)))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from(exe))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group_with(id: &str, profile: &str) -> ArtifactGroup {
        ArtifactGroup {
            id: id.into(),
            profile: profile.into(),
            paths: Vec::new(),
            usage: purge_ecosystem::DiskUsage::default(),
            rebuildable: true,
            parts: Vec::new(),
        }
    }

    #[test]
    fn maps_groups_to_cargo_arguments() {
        assert_eq!(
            profile_args(&group_with("debug", "debug")),
            ["--profile", "dev"]
        );
        assert_eq!(
            profile_args(&group_with("release", "release")),
            ["--profile", "release"]
        );
        assert_eq!(
            profile_args(&group_with("aarch64-apple-darwin/release", "release")),
            ["--profile", "release", "--target", "aarch64-apple-darwin"]
        );
    }

    #[test]
    fn editor_groups_are_removed_directly_and_never_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let profile = target.join("rust-analyzer/debug");
        fs::create_dir_all(profile.join(".fingerprint")).unwrap();
        fs::write(target.join("rust-analyzer/CACHEDIR.TAG"), "").unwrap();
        let project = Project {
            ecosystem: NAME,
            name: "p".into(),
            root: dir.path().to_path_buf(),
            artifact_dir: target.clone(),
            extra_dirs: Vec::new(),
        };
        let rust = Rust::new();
        let artifacts = rust.inspect(&project).unwrap();
        let analyzer = artifacts.group("rust-analyzer/debug").unwrap();
        assert!(!analyzer.rebuildable);
        // Never through `cargo clean --target rust-analyzer`, which removes nothing.
        rust.clean_group(&project, analyzer).unwrap();
        assert!(!profile.exists());
        assert!(rust.rebuild_group(&project, analyzer, &[]).is_err());
    }

    #[test]
    fn refuses_to_touch_paths_outside_the_build_output() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            ecosystem: NAME,
            name: "p".into(),
            root: dir.path().to_path_buf(),
            artifact_dir: dir.path().join("target"),
            extra_dirs: Vec::new(),
        };
        let mut group = group_with("debug", "debug");
        group.paths = vec![dir.path().join("src")];
        assert!(Rust::new().clean_group(&project, &group).is_err());
        group.paths = vec![dir.path().join("target")];
        assert!(Rust::new().trim(&project, &group, &[]).is_err());
    }

    #[test]
    fn detects_lock_held_by_another_handle() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("debug");
        fs::create_dir_all(&profile).unwrap();
        let lock = profile.join(".cargo-build-lock");
        let holder = File::create(&lock).unwrap();
        assert!(!build_lock_held(dir.path()));
        holder.lock().unwrap();
        assert!(build_lock_held(dir.path()));
    }
}
