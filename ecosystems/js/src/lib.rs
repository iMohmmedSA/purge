//! JavaScript and TypeScript support: npm, pnpm, Yarn and Bun projects,
//! including workspaces.
//!
//! Purge removes only output it can identify (see the `outputs` module):
//! installed dependencies that a lockfile can bring back, and the build
//! caches of known tools that are dependencies of the project. It never
//! installs or builds anything itself, and never touches what git tracks.
//!
//! Dependencies shared through a package manager's store (pnpm, Bun) are
//! hard links; removing them frees nothing until the store is pruned, so
//! they measure as nearly empty and are left alone.

mod git;
mod outputs;
mod project;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
    time::SystemTime,
};

use purge_ecosystem::{ArtifactGroup, Artifacts, Ecosystem, Error, Project, Result, fsutil};
use purge_process::ProcessTable;

pub(crate) const NAME: &str = "js";

/// Programs that mean someone is running, building or installing the
/// project: dev servers, watchers and language servers all run on Node,
/// Bun or Deno.
const TOOLS: &[&str] = &[
    "node", "npm", "npx", "pnpm", "pnpx", "yarn", "bun", "bunx", "deno",
];

/// Git files whose modification marks commits, checkouts and staging.
const GIT_MARKERS: &[&str] = &[".git/HEAD", ".git/index", ".git/logs/HEAD"];

/// JavaScript support. Create one per scan.
#[derive(Debug, Default)]
pub struct JavaScript {
    /// Directory -> whether its filesystem records access times.
    access_times: Mutex<HashMap<PathBuf, bool>>,
}

impl JavaScript {
    pub fn new() -> Self {
        Self::default()
    }

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

impl Ecosystem for JavaScript {
    fn name(&self) -> &'static str {
        NAME
    }

    fn detect(&self, dir: &Path) -> Result<Option<Project>> {
        let Some(root) = project::root(dir) else {
            return Ok(None);
        };
        let root = dunce::canonicalize(&root)?;
        let mut dirs = outputs::locate(&root).into_iter().map(|output| output.path);
        let Some(artifact_dir) = dirs.next() else {
            return Ok(None); // nothing to reclaim
        };
        let name = root.file_name().map_or_else(
            || root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        Ok(Some(Project {
            ecosystem: NAME,
            name,
            artifact_dir,
            extra_dirs: dirs.collect(),
            root,
        }))
    }

    fn source_modified(&self, project: &Project) -> Option<SystemTime> {
        let tree = fsutil::newest_mtime(&project.root, &|dir| {
            fsutil::is_ignored_dir(dir) || project.artifact_dirs().any(|output| output == dir)
        });
        let git = GIT_MARKERS
            .iter()
            .filter_map(|marker| {
                fs::metadata(project.root.join(marker))
                    .and_then(|meta| meta.modified())
                    .ok()
            })
            .max();
        tree.max(git)
    }

    fn inspect(&self, project: &Project) -> Result<Artifacts> {
        // Probe inside the output, never in the project itself: the probe
        // file would make the sources look freshly changed.
        let used_times = self.records_access_time(&project.artifact_dir);
        Ok(outputs::inspect(
            &project.root,
            used_times,
            self.source_modified(project),
        ))
    }

    fn busy(&self, project: &Project, processes: &ProcessTable) -> Option<String> {
        processes.find(TOOLS, &project.root).map(|process| {
            // Node names its threads (`node-MainThread`, `rolldown-worker`);
            // the program says more.
            let program = process.program.as_deref().unwrap_or(&process.name);
            format!("`{program}` is running in the project")
        })
    }

    /// Removes every group, as measured now: never dependencies that are
    /// shared with a package manager's store.
    fn clean_all(&self, project: &Project) -> Result<()> {
        for group in &self.inspect(project)?.groups {
            self.clean_group(project, group)?;
        }
        Ok(())
    }

    fn clean_group(&self, project: &Project, group: &ArtifactGroup) -> Result<()> {
        for path in recognised(project, group)? {
            if path.exists() {
                fs::remove_dir_all(path)?;
            }
        }
        Ok(())
    }

    fn trim(&self, project: &Project, group: &ArtifactGroup, parts: &[String]) -> Result<()> {
        let dirs = recognised(project, group)?;
        for id in parts {
            let path = outputs::part_path(&project.root, id, &dirs)
                .ok_or_else(|| Error::Invalid(format!("not part of `{}`: `{id}`", group.id)))?;
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() => fs::remove_dir_all(&path)?,
                Ok(_) => fs::remove_file(&path)?,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(())
    }

    fn rebuild_group(&self, _: &Project, group: &ArtifactGroup, _: &[String]) -> Result<()> {
        Err(Error::Invalid(format!(
            "purge does not rebuild `{}`; your own install or build does",
            group.id
        )))
    }
}

/// The group's directories, after checking that each is output purge
/// recognises in this project right now: nothing else is ever removed.
fn recognised(project: &Project, group: &ArtifactGroup) -> Result<Vec<PathBuf>> {
    let known: Vec<PathBuf> = outputs::locate(&project.root)
        .into_iter()
        .map(|output| output.path)
        .collect();
    for path in &group.paths {
        if !known.contains(path) {
            return Err(Error::Invalid(format!(
                "{} is not output purge recognises",
                path.display()
            )));
        }
    }
    Ok(group.paths.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn app(root: &Path) {
        write(
            &root.join("package.json"),
            r#"{"dependencies": {"next": "16"}}"#,
        );
        write(&root.join("package-lock.json"), "{}");
        write(&root.join("node_modules/ms/package.json"), "{}");
        write(&root.join("node_modules/.vite/deps/ms.js"), "x");
        write(&root.join(".next/cache/images/a"), "x");
        write(&root.join("src/page.tsx"), "x");
        write(&root.join(".env.local"), "SECRET=1");
    }

    #[test]
    fn detects_projects_with_output_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let js = JavaScript::new();
        write(&root.join("package.json"), "{}");
        assert_eq!(js.detect(&root).unwrap(), None);

        app(&root);
        let project = js.detect(&root).unwrap().unwrap();
        assert_eq!(project.ecosystem, NAME);
        assert_eq!(project.root, root);
        let dirs: Vec<&Path> = project.artifact_dirs().collect();
        assert_eq!(dirs, [root.join(".next"), root.join("node_modules")]);
        assert!(!project.has_external_artifacts());
        assert!(js.source_modified(&project).is_some());
    }

    // Setting a directory's time needs Unix: Windows cannot open one as a file.
    #[cfg(unix)]
    #[test]
    fn measuring_does_not_look_like_activity() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        app(&root);
        let js = JavaScript::new();
        let project = js.detect(&root).unwrap().unwrap();
        let past = SystemTime::now() - std::time::Duration::from_hours(240);
        for dir in [root.clone(), root.join("src")] {
            fs::File::open(&dir).unwrap().set_modified(past).unwrap();
        }
        for file in [
            "package.json",
            "package-lock.json",
            "src/page.tsx",
            ".env.local",
        ] {
            fs::File::options()
                .write(true)
                .open(root.join(file))
                .unwrap()
                .set_modified(past)
                .unwrap();
        }
        let before = js.source_modified(&project);
        js.inspect(&project).unwrap();
        assert_eq!(js.source_modified(&project), before);
    }

    #[test]
    fn cleans_trims_and_never_rebuilds() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        app(&root);
        let js = JavaScript::new();
        let project = js.detect(&root).unwrap().unwrap();
        let artifacts = js.inspect(&project).unwrap();
        let deps = artifacts.group("node_modules").unwrap();
        assert!(js.rebuild_group(&project, deps, &[]).is_err());

        js.trim(&project, deps, &["node_modules/.vite".to_owned()])
            .unwrap();
        assert!(!root.join("node_modules/.vite").exists());
        assert!(root.join("node_modules/ms").exists());
        assert!(
            js.trim(&project, deps, &["src".to_owned()]).is_err(),
            "outside the group"
        );

        let next = artifacts.group("next").unwrap();
        js.clean_group(&project, next).unwrap();
        assert!(!root.join(".next").exists());

        js.clean_all(&project).unwrap();
        assert!(!root.join("node_modules").exists());
        // Sources and secrets are untouched.
        assert!(root.join("src/page.tsx").exists());
        assert!(root.join(".env.local").exists());
    }

    #[test]
    fn refuses_paths_it_does_not_recognise() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        app(&root);
        let js = JavaScript::new();
        let project = js.detect(&root).unwrap().unwrap();
        let mut forged = js.inspect(&project).unwrap().group("next").unwrap().clone();
        forged.paths = vec![root.join("src")];
        assert!(js.clean_group(&project, &forged).is_err());
        assert!(root.join("src").exists());
    }
}
