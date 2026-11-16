//! Finding a project's root: the package itself, or the workspace
//! (monorepo) it belongs to.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// Lockfiles that pin every installed package, by package manager. With
/// one, `node_modules` can be installed again exactly as it was.
pub(crate) const LOCKFILES: &[&str] = &[
    "package-lock.json",   // npm
    "npm-shrinkwrap.json", // npm
    "pnpm-lock.yaml",      // pnpm
    "yarn.lock",           // Yarn
    "bun.lock",            // Bun 1.2+
    "bun.lockb",           // older Bun
];

/// The root of the project `dir` belongs to, if `dir` is a package.
///
/// A package with its own lockfile is its own project. Otherwise it belongs
/// to the nearest enclosing workspace root (a `package.json` with
/// `workspaces`, or a `pnpm-workspace.yaml`), if any. The search stops at a
/// repository root or at another project's lockfile.
pub(crate) fn root(dir: &Path) -> Option<PathBuf> {
    if !dir.join("package.json").is_file() {
        return None;
    }
    if has_lockfile(dir) {
        return Some(dir.to_path_buf());
    }
    for ancestor in dir.ancestors().skip(1) {
        if is_workspace_root(ancestor) {
            return Some(ancestor.to_path_buf());
        }
        if has_lockfile(ancestor) || ancestor.join(".git").exists() {
            break;
        }
    }
    Some(dir.to_path_buf())
}

pub(crate) fn has_lockfile(dir: &Path) -> bool {
    LOCKFILES.iter().any(|name| dir.join(name).is_file())
}

fn is_workspace_root(dir: &Path) -> bool {
    dir.join("pnpm-workspace.yaml").is_file()
        || manifest(dir).is_some_and(|manifest| manifest.get("workspaces").is_some())
}

/// The parsed `package.json` in `dir`.
pub(crate) fn manifest(dir: &Path) -> Option<serde_json::Value> {
    let text = fs::read(dir.join("package.json")).ok()?;
    serde_json::from_slice(&text).ok()
}

/// Whether the package in `dir` depends on `name` in any way.
pub(crate) fn depends_on(manifest: &serde_json::Value, name: &str) -> bool {
    [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ]
    .iter()
    .any(|field| {
        manifest
            .get(field)
            .and_then(|deps| deps.get(name))
            .is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn packages_find_their_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        // A workspace with a member, and an unrelated project inside it.
        write(&repo.join("package.json"), r#"{"workspaces": ["apps/*"]}"#);
        write(&repo.join("package-lock.json"), "{}");
        write(&repo.join("apps/web/package.json"), "{}");
        write(&repo.join("tools/own/package.json"), "{}");
        write(&repo.join("tools/own/pnpm-lock.yaml"), "");

        assert_eq!(root(&repo), Some(repo.clone()));
        assert_eq!(root(&repo.join("apps/web")), Some(repo.clone()));
        let own = repo.join("tools/own");
        assert_eq!(root(&own), Some(own));
        assert_eq!(root(&repo.join("apps")), None, "not a package");
    }

    #[test]
    fn pnpm_workspaces_and_lone_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let mono = tmp.path().join("mono");
        write(&mono.join("pnpm-workspace.yaml"), "packages: ['pkgs/*']");
        write(&mono.join("package.json"), "{}");
        write(&mono.join("pkgs/a/package.json"), "{}");
        assert_eq!(root(&mono.join("pkgs/a")), Some(mono));

        // No workspace above: the package is its own project.
        let lone = tmp.path().join("lone");
        write(&lone.join("package.json"), "{}");
        assert_eq!(root(&lone), Some(lone));
    }

    #[test]
    fn reads_dependencies_of_every_kind() {
        let manifest: serde_json::Value = serde_json::from_str(
            r#"{"dependencies": {"next": "16"}, "devDependencies": {"turbo": "2"}}"#,
        )
        .unwrap();
        assert!(depends_on(&manifest, "next"));
        assert!(depends_on(&manifest, "turbo"));
        assert!(!depends_on(&manifest, "vite"));
    }
}
