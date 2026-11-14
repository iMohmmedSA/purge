//! Where a workspace keeps its build output, according to `cargo metadata`.

use std::path::{Path, PathBuf};

use purge_ecosystem::{Project, fsutil};
use serde::Deserialize;
use tracing::debug;

use crate::NAME;

/// A workspace as Cargo sees it.
#[derive(Debug)]
pub(crate) struct Layout {
    pub(crate) root: PathBuf,
    /// Final artifacts (`target/`).
    pub(crate) target: PathBuf,
    /// Intermediate artifacts (`build.build-dir`); usually `target` too.
    pub(crate) build: PathBuf,
    /// Directories of every member package.
    pub(crate) members: Vec<PathBuf>,
}

impl Layout {
    /// The conventional layout, for when `cargo metadata` cannot run.
    pub(crate) fn conventional(dir: &Path) -> Self {
        let target = dir.join("target");
        Self {
            root: dir.to_path_buf(),
            build: target.clone(),
            target,
            members: Vec::new(),
        }
    }

    /// The project, if it has build output purge may touch.
    pub(crate) fn project(self) -> Option<Project> {
        // Canonical, so projects sharing a directory compare equal. A
        // directory that does not exist yet simply holds nothing.
        let target = dunce::canonicalize(&self.target).ok();
        let build = dunce::canonicalize(&self.build)
            .ok()
            .filter(|build| target.as_ref() != Some(build));
        let dirs: Vec<PathBuf> = build.into_iter().chain(target).collect();
        if let Some(untrusted) = dirs.iter().find(|dir| !self.trusts(dir)) {
            // An output directory outside the project is only trusted when
            // Cargo has tagged it as its own, so a misconfigured setting can
            // never point purge at something else.
            debug!(root = %self.root.display(), dir = %untrusted.display(), "build directory outside the project not tagged by cargo, skipping");
            return None;
        }
        let mut dirs = dirs.into_iter();
        let artifact_dir = dirs.next()?;
        let name = self.root.file_name().map_or_else(
            || self.root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        Some(Project {
            ecosystem: NAME,
            name,
            root: self.root,
            artifact_dir,
            extra_dirs: dirs.collect(),
        })
    }

    fn trusts(&self, dir: &Path) -> bool {
        dir.starts_with(&self.root) || fsutil::is_cache_dir(dir)
    }
}

/// The parts of `cargo metadata --no-deps` purge reads.
#[derive(Debug, Deserialize)]
pub(crate) struct Metadata {
    workspace_root: PathBuf,
    target_directory: PathBuf,
    /// Reported since Cargo 1.91, which can keep intermediate artifacts
    /// apart from `target/`.
    #[serde(default)]
    build_directory: Option<PathBuf>,
    /// Every workspace member.
    packages: Vec<Package>,
}

#[derive(Debug, Deserialize)]
struct Package {
    manifest_path: PathBuf,
}

impl From<Metadata> for Layout {
    fn from(meta: Metadata) -> Self {
        Self {
            members: meta
                .packages
                .iter()
                .filter_map(|package| package.manifest_path.parent())
                .map(Path::to_path_buf)
                .collect(),
            root: meta.workspace_root,
            build: meta
                .build_directory
                .unwrap_or_else(|| meta.target_directory.clone()),
            target: meta.target_directory,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn layout(root: &Path, target: &Path, build: &Path) -> Layout {
        Layout {
            root: root.to_path_buf(),
            target: target.to_path_buf(),
            build: build.to_path_buf(),
            members: Vec::new(),
        }
    }

    #[test]
    fn separate_build_dir_is_the_main_artifact_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let root = base.join("app");
        let cache = base.join("cache/app");
        fs::create_dir_all(root.join("target")).unwrap();
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("CACHEDIR.TAG"), "").unwrap();

        let project = layout(&root, &root.join("target"), &cache)
            .project()
            .unwrap();
        assert_eq!(project.artifact_dir, cache);
        assert_eq!(project.extra_dirs, [root.join("target")]);
        assert!(project.has_external_artifacts());
    }

    #[test]
    fn same_dirs_or_missing_dirs_collapse() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let target = root.join("target");
        assert_eq!(layout(&root, &target, &target).project(), None);

        fs::create_dir_all(&target).unwrap();
        let project = layout(&root, &target, &target).project().unwrap();
        assert_eq!(project.artifact_dir, target);
        assert!(project.extra_dirs.is_empty());
        assert!(!project.has_external_artifacts());
    }

    #[test]
    fn untagged_outside_dir_is_not_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let root = base.join("app");
        let elsewhere = base.join("elsewhere");
        fs::create_dir_all(root.join("target")).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        assert_eq!(
            layout(&root, &root.join("target"), &elsewhere).project(),
            None
        );
    }

    #[test]
    fn reads_build_directory_when_reported() {
        let json = r#"{"workspace_root":"/w","target_directory":"/w/target",
            "build_directory":"/cache/w","packages":[{"manifest_path":"/w/a/Cargo.toml"}]}"#;
        let layout = Layout::from(serde_json::from_str::<Metadata>(json).unwrap());
        assert_eq!(layout.build, Path::new("/cache/w"));
        assert_eq!(layout.members, [PathBuf::from("/w/a")]);

        let old = r#"{"workspace_root":"/w","target_directory":"/w/target","packages":[]}"#;
        let layout = Layout::from(serde_json::from_str::<Metadata>(old).unwrap());
        assert_eq!(layout.build, Path::new("/w/target"));
    }
}
