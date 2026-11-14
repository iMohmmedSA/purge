//! Finds projects below the configured root folders by asking every
//! ecosystem about every directory.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use purge_ecosystem::{Ecosystem, Project, fsutil::is_ignored_dir};
use tracing::{debug, warn};

pub struct Scanner<'a> {
    ecosystems: &'a [Box<dyn Ecosystem>],
    max_depth: usize,
    exclude: &'a dyn Fn(&Path) -> bool,
}

impl std::fmt::Debug for Scanner<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scanner")
            .field("ecosystems", &self.ecosystems)
            .field("max_depth", &self.max_depth)
            .finish_non_exhaustive()
    }
}

impl<'a> Scanner<'a> {
    pub fn new(
        ecosystems: &'a [Box<dyn Ecosystem>],
        max_depth: usize,
        exclude: &'a dyn Fn(&Path) -> bool,
    ) -> Self {
        Self {
            ecosystems,
            max_depth,
            exclude,
        }
    }

    /// Projects under `roots`, each reported once, sorted by root.
    pub fn scan(&self, roots: &[PathBuf]) -> Vec<Project> {
        let mut walk = Walk {
            scanner: self,
            seen: HashSet::new(),
            artifact_dirs: HashSet::new(),
            found: Vec::new(),
        };
        for root in roots {
            match dunce::canonicalize(root) {
                Ok(root) => walk.visit(&root, 0),
                Err(err) => warn!(root = %root.display(), "cannot scan root: {err}"),
            }
        }
        let mut found = walk.found;
        found.sort_by(|a, b| a.root.cmp(&b.root));
        found
    }
}

struct Walk<'s, 'a> {
    scanner: &'s Scanner<'a>,
    seen: HashSet<(&'static str, PathBuf)>,
    artifact_dirs: HashSet<PathBuf>,
    found: Vec<Project>,
}

impl Walk<'_, '_> {
    fn visit(&mut self, dir: &Path, depth: usize) {
        if (self.scanner.exclude)(dir) {
            debug!(dir = %dir.display(), "excluded");
            return;
        }
        for ecosystem in self.scanner.ecosystems {
            match ecosystem.detect(dir) {
                Ok(Some(project)) => {
                    self.artifact_dirs
                        .extend(project.artifact_dirs().map(Path::to_path_buf));
                    if self.seen.insert((project.ecosystem, project.root.clone())) {
                        debug!(root = %project.root.display(), ecosystem = project.ecosystem, "found project");
                        self.found.push(project);
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    warn!(dir = %dir.display(), ecosystem = ecosystem.name(), "detection failed: {err}");
                }
            }
        }
        if depth >= self.scanner.max_depth {
            return;
        }
        for child in self.subdirectories(dir) {
            self.visit(&child, depth + 1);
        }
    }

    fn subdirectories(&self, dir: &Path) -> Vec<PathBuf> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                debug!(dir = %dir.display(), "cannot read directory: {err}");
                return Vec::new();
            }
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .filter(|path| !is_ignored_dir(path) && !self.artifact_dirs.contains(path))
            .collect();
        dirs.sort();
        dirs
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use purge_ecosystem::{ArtifactGroup, Artifacts, Result};
    use purge_process::ProcessTable;

    use super::*;

    /// Treats any directory containing a `marker` file as a project whose
    /// root is named in that file (relative to the scan root's parent), so
    /// tests can model workspaces.
    #[derive(Debug)]
    struct Fake;

    impl Ecosystem for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn detect(&self, dir: &Path) -> Result<Option<Project>> {
            let Ok(owner) = fs::read_to_string(dir.join("marker")) else {
                return Ok(None);
            };
            let root = if owner.is_empty() {
                dir.to_path_buf()
            } else {
                dir.join(owner.trim())
            };
            let root = dunce::canonicalize(root)?;
            Ok(Some(Project {
                ecosystem: "fake",
                name: String::new(),
                artifact_dir: root.join("out"),
                extra_dirs: Vec::new(),
                root,
            }))
        }

        fn source_modified(&self, _: &Project) -> Option<SystemTime> {
            unreachable!()
        }

        fn inspect(&self, _: &Project) -> Result<Artifacts> {
            unreachable!()
        }

        fn busy(&self, _: &Project, _: &ProcessTable) -> Option<String> {
            None
        }

        fn clean_all(&self, _: &Project) -> Result<()> {
            unreachable!()
        }

        fn clean_group(&self, _: &Project, _: &ArtifactGroup) -> Result<()> {
            unreachable!()
        }

        fn trim(&self, _: &Project, _: &ArtifactGroup, _: &[String]) -> Result<()> {
            unreachable!()
        }

        fn rebuild_group(&self, _: &Project, _: &ArtifactGroup, _: &[String]) -> Result<()> {
            unreachable!()
        }
    }

    fn touch(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn finds_dedupes_and_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(&root.join("ws/marker"), "");
        touch(&root.join("ws/member/marker"), "..");
        touch(&root.join("ws/out/nested/marker"), ""); // inside artifacts
        touch(&root.join("solo/marker"), "");
        touch(&root.join(".hidden/marker"), "");
        touch(&root.join("cache/CACHEDIR.TAG"), "");
        touch(&root.join("cache/p/marker"), "");
        touch(&root.join("skipme/marker"), "");
        touch(&root.join("a/b/c/deep/marker"), "");

        let ecosystems: Vec<Box<dyn Ecosystem>> = vec![Box::new(Fake)];
        let exclude = |p: &Path| p.ends_with("skipme");
        let found = Scanner::new(&ecosystems, 3, &exclude).scan(&[root.to_path_buf()]);
        let names: Vec<_> = found
            .iter()
            .map(|p| {
                p.root
                    .strip_prefix(dunce::canonicalize(root).unwrap())
                    .unwrap()
                    .to_path_buf()
            })
            .collect();
        assert_eq!(names, [PathBuf::from("solo"), PathBuf::from("ws")]);
    }
}
