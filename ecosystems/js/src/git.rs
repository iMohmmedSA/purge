//! Asking git which output is committed. Some projects commit build output
//! or even `node_modules` (GitHub Actions written in JavaScript must), and
//! purge never removes anything committed.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use purge_ecosystem::command;
use purge_process::configure_background;
use tracing::warn;

const TIMEOUT: Duration = Duration::from_mins(1);

/// Which of `candidates` (directories under `root`) contain files tracked by
/// git. If git cannot answer for a repository, every candidate counts as
/// tracked, so nothing in it is removed.
pub(crate) fn tracked(root: &Path, candidates: &[PathBuf]) -> Vec<PathBuf> {
    if candidates.is_empty() || !in_repository(root) {
        return Vec::new();
    }
    let relative: Vec<&Path> = candidates
        .iter()
        .filter_map(|dir| dir.strip_prefix(root).ok())
        .collect();
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--"])
        .args(&relative);
    configure_background(&mut cmd);
    match command::run(&mut cmd, TIMEOUT) {
        Ok(output) => {
            let files: Vec<PathBuf> = output
                .stdout
                .split(|&byte| byte == 0)
                .filter(|path| !path.is_empty())
                .map(|path| root.join(String::from_utf8_lossy(path).as_ref()))
                .collect();
            candidates
                .iter()
                .filter(|dir| files.iter().any(|file| file.starts_with(dir)))
                .cloned()
                .collect()
        }
        Err(err) => {
            warn!(root = %root.display(), "cannot ask git which files are committed, leaving the project alone: {err}");
            candidates.to_vec()
        }
    }
}

/// Whether `dir` lies in a git repository.
fn in_repository(dir: &Path) -> bool {
    dir.ancestors().any(|dir| dir.join(".git").exists())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn finds_committed_output() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        for dir in ["node_modules/dep", "dist", ".next/cache"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("node_modules/dep/index.js"), "").unwrap();
        fs::write(root.join("dist/app.js"), "").unwrap();
        fs::write(root.join(".next/cache/x"), "").unwrap();
        let candidates = [
            root.join("node_modules"),
            root.join("dist"),
            root.join(".next"),
        ];

        // Outside a repository nothing is tracked.
        assert!(tracked(&root, &candidates).is_empty());

        git(&root, &["init", "-q"]);
        git(&root, &["add", "node_modules"]);
        git(&root, &["commit", "-q", "-m", "vendor"]);
        assert_eq!(tracked(&root, &candidates), [root.join("node_modules")]);
    }
}
