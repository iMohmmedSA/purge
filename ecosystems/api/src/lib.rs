//! The contract every supported project kind (Rust, and later Node, Python,
//! ...) implements. The rest of purge only talks to ecosystems through
//! [`Ecosystem`], so adding one never touches the scanner or the engine.

pub mod command;
pub mod fsutil;

use std::{
    fmt, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use purge_process::ProcessTable;

/// A project on disk that owns reclaimable build artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// [`Ecosystem::name`] of the ecosystem that found it.
    pub ecosystem: &'static str,
    pub name: String,
    /// Root directory (for Cargo, the workspace root).
    pub root: PathBuf,
    /// Directory holding the bulk of the build output (for Cargo, the build
    /// directory: `target/` unless `build.build-dir` moves it). Projects
    /// with the same `artifact_dir` share their build output. It may lie
    /// outside `root`.
    pub artifact_dir: PathBuf,
    /// Further directories with the project's build output (for Cargo,
    /// `target/` when the build directory is elsewhere).
    pub extra_dirs: Vec<PathBuf>,
}

impl Project {
    /// Every directory holding the project's build output.
    pub fn artifact_dirs(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.artifact_dir.as_path())
            .chain(self.extra_dirs.iter().map(PathBuf::as_path))
    }

    /// Whether build output lives outside the project, where other projects
    /// may share it.
    pub fn has_external_artifacts(&self) -> bool {
        self.artifact_dirs().any(|dir| !dir.starts_with(&self.root))
    }
}

/// Size and freshness of a directory tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskUsage {
    /// Bytes that removing the tree would free: allocated space of every
    /// file, except files hard-linked from outside the tree (such as pnpm's
    /// shared store), which removing the tree does not free.
    pub bytes: u64,
    /// Allocated space of every file, counting each hard-linked file once,
    /// wherever its other links are.
    pub total: u64,
    /// Most recent modification time of any file in the tree. Directories
    /// do not count: removing files from one is not using it.
    pub newest: Option<SystemTime>,
}

/// An independently cleanable part of the build output, such as one Cargo
/// profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactGroup {
    /// Unique within the project, e.g. `debug` or `x86_64-pc-windows-gnu/release`.
    pub id: String,
    /// Profile name used for policy lookup, e.g. `debug` or `release`.
    pub profile: String,
    /// Every directory holding the group's output; never empty.
    pub paths: Vec<PathBuf>,
    pub usage: DiskUsage,
    /// When the group was last used, if the ecosystem knows better than
    /// the newest file in it (`usage.newest`), which only shows when it was
    /// last written. Installed dependencies, for one, are written once and
    /// then only read.
    pub last_used: Option<SystemTime>,
    /// Whether [`Ecosystem::rebuild_group`] can build this group again.
    /// Output written by other tools (such as an editor's private build
    /// directory) can be cleaned but not rebuilt.
    pub rebuildable: bool,
    /// Separately removable pieces of the group, for [`Ecosystem::trim`].
    /// Empty when the ecosystem cannot tell them apart.
    pub parts: Vec<Part>,
}

/// One separately removable piece of an artifact group, such as the output
/// of compiling one crate with one set of options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Identifies the part to [`Ecosystem::trim`].
    pub id: String,
    pub bytes: u64,
    /// When the part was last written.
    pub built: Option<SystemTime>,
    /// When the build tool last read the part, if the filesystem records
    /// access times reliably; `None` if unknown.
    pub used: Option<SystemTime>,
    /// The compiler that built the part, if known. Parts from a compiler no
    /// longer in use are dead weight.
    pub toolchain: Option<String>,
}

/// The contents of an artifact directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Artifacts {
    pub groups: Vec<ArtifactGroup>,
    /// Size of the whole directory, including data outside groups.
    pub total_bytes: u64,
}

impl Artifacts {
    pub fn group(&self, id: &str) -> Option<&ArtifactGroup> {
        self.groups.iter().find(|group| group.id == id)
    }
}

/// Everything the policy looks at for one artifact directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Last source change across every project using the directory.
    pub source_modified: Option<SystemTime>,
    pub artifacts: Artifacts,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("`{command}` failed ({status}){}", fmt_stderr(.stderr))]
    Command {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("`{command}` did not finish within {after:?}; stopped it")]
    Timeout { command: String, after: Duration },
    #[error("`{command}` was stopped because purge is shutting down")]
    Stopped { command: String },
    #[error("{0}")]
    Invalid(String),
}

fn fmt_stderr(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(":\n{stderr}")
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A cache an ecosystem's tools share between projects, outside any of
/// them, such as pnpm's package store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedCache {
    /// Stable short name, used in the configuration: `pnpm`, `npm`, `bun`.
    pub id: &'static str,
    /// For people, e.g. `pnpm store`.
    pub name: String,
    pub path: PathBuf,
    /// Allocated space of the whole cache.
    pub total_bytes: u64,
    /// Space that clearing it would free: files no project links to.
    pub unused_bytes: u64,
    /// Whether [`Ecosystem::prune_cache`] removes only what no project
    /// uses. Otherwise it clears everything, and projects only lose the
    /// chance to reinstall without downloading.
    pub unused_only: bool,
    /// Programs that use the cache; it is left alone while one runs.
    pub tools: &'static [&'static str],
}

/// A kind of project purge knows how to clean.
pub trait Ecosystem: fmt::Debug + Send + Sync {
    /// Short stable identifier, e.g. `rust`.
    fn name(&self) -> &'static str;

    /// Returns the project `dir` belongs to, if `dir` holds one with build
    /// output. Several directories may report the same project (for example
    /// every member of a Cargo workspace); the scanner deduplicates by root.
    fn detect(&self, dir: &Path) -> Result<Option<Project>>;

    /// Last change to the project's sources (not its build output).
    fn source_modified(&self, project: &Project) -> Option<SystemTime>;

    /// Measures the project's build output.
    fn inspect(&self, project: &Project) -> Result<Artifacts>;

    /// Explains why the project must not be touched right now, such as a
    /// build or language server running in it.
    fn busy(&self, project: &Project, processes: &ProcessTable) -> Option<String>;

    /// Removes all build output.
    fn clean_all(&self, project: &Project) -> Result<()>;

    /// Removes one artifact group.
    fn clean_group(&self, project: &Project, group: &ArtifactGroup) -> Result<()>;

    /// Removes some of a group's [`Part`]s, leaving the rest usable.
    fn trim(&self, project: &Project, group: &ArtifactGroup, parts: &[String]) -> Result<()>;

    /// Builds one artifact group again, with `extra_args` passed to the build tool.
    fn rebuild_group(
        &self,
        project: &Project,
        group: &ArtifactGroup,
        extra_args: &[String],
    ) -> Result<()>;

    /// The shared caches serving projects under `roots`, measured.
    fn shared_caches(&self, roots: &[PathBuf]) -> Vec<SharedCache> {
        let _ = roots;
        Vec::new()
    }

    /// Frees space in `cache` with its tool's own command: removing what no
    /// project uses, or everything if it cannot tell.
    fn prune_cache(&self, cache: &SharedCache) -> Result<()> {
        Err(Error::Invalid(format!("cannot prune {}", cache.name)))
    }
}
