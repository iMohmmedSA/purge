//! The output purge recognises in a JavaScript project, and how it is
//! measured.
//!
//! Only folders at fixed, tool-owned locations count, and only when the
//! tool that owns them is a dependency of the package they sit in (or of
//! the workspace root). Nothing else in a project is ever touched: build
//! folders such as `dist/` have no owner purge can be sure of.

use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

use purge_ecosystem::{ArtifactGroup, Artifacts, Part, fsutil};

use crate::{git, project};

/// One kind of output folder.
#[derive(Debug)]
pub(crate) struct Kind {
    /// Artifact group the folder joins, also its policy profile.
    pub(crate) group: &'static str,
    /// Location relative to the package that owns it.
    pub(crate) dir: &'static str,
    /// Packages that create the folder; one must be a dependency. Empty
    /// for `node_modules`, which instead needs a lockfile to be installed
    /// again exactly.
    owners: &'static [&'static str],
    /// Pieces that can be trimmed on their own, relative to the folder:
    /// `name` for one subfolder, `name/*` for each entry in it.
    parts: &'static [&'static str],
}

pub(crate) const NODE_MODULES: &str = "node_modules";

pub(crate) const KINDS: &[Kind] = &[
    Kind {
        group: NODE_MODULES,
        dir: "node_modules",
        owners: &[],
        // Tool caches (babel-loader, eslint-loader, Storybook, ...) and
        // Vite's pre-bundled dependencies.
        parts: &[".cache/*", ".vite"],
    },
    Kind {
        group: "next",
        dir: ".next",
        owners: &["next"],
        parts: &["cache/*", "dev"],
    },
    Kind {
        group: "nuxt",
        dir: ".nuxt",
        owners: &["nuxt"],
        parts: &[],
    },
    Kind {
        group: "nuxt",
        dir: ".output",
        owners: &["nuxt"],
        parts: &[],
    },
    Kind {
        group: "sveltekit",
        dir: ".svelte-kit",
        owners: &["@sveltejs/kit"],
        parts: &[],
    },
    Kind {
        group: "astro",
        dir: ".astro",
        owners: &["astro"],
        parts: &[],
    },
    Kind {
        group: "angular",
        dir: ".angular/cache",
        owners: &["@angular/cli"],
        // One folder per Angular version.
        parts: &["*"],
    },
    Kind {
        group: "turbo",
        dir: ".turbo",
        owners: &["turbo"],
        parts: &[],
    },
    Kind {
        group: "nx",
        dir: ".nx/cache",
        owners: &["nx"],
        // One entry per cached task run.
        parts: &["*"],
    },
    Kind {
        group: "parcel",
        dir: ".parcel-cache",
        owners: &["parcel"],
        parts: &[],
    },
    Kind {
        group: "docusaurus",
        dir: ".docusaurus",
        owners: &["@docusaurus/core"],
        parts: &[],
    },
];

/// How far below the project root packages are looked for.
const MAX_DEPTH: usize = 6;

/// An output folder found in a project.
#[derive(Debug, Clone)]
pub(crate) struct Output {
    pub(crate) kind: &'static Kind,
    pub(crate) path: PathBuf,
}

/// Every recognised output folder in the project at `root`, except those
/// git tracks.
pub(crate) fn locate(root: &Path) -> Vec<Output> {
    let root_manifest = project::manifest(root);
    let installable = project::has_lockfile(root);
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if let Some(manifest) = project::manifest(&dir) {
            for kind in KINDS {
                let path = dir.join(kind.dir);
                let owned = if kind.owners.is_empty() {
                    installable
                } else {
                    kind.owners.iter().any(|owner| {
                        project::depends_on(&manifest, owner)
                            || root_manifest
                                .as_ref()
                                .is_some_and(|root| project::depends_on(root, owner))
                    })
                };
                if owned && is_real_dir(&path) {
                    found.push(Output { kind, path });
                }
            }
        }
        if depth < MAX_DEPTH {
            stack.extend(
                fsutil::children(&dir)
                    .filter(|child| is_real_dir(child) && !skip(child))
                    .map(|child| (child, depth + 1)),
            );
        }
    }
    let paths: Vec<PathBuf> = found.iter().map(|output| output.path.clone()).collect();
    let tracked = git::tracked(root, &paths);
    found.retain(|output| !tracked.contains(&output.path));
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// Folders whose insides are never part of this project's packages:
/// installed dependencies, hidden folders, tagged caches, and nested
/// projects or repositories.
fn skip(dir: &Path) -> bool {
    fsutil::is_ignored_dir(dir) || project::has_lockfile(dir) || dir.join(".git").exists()
}

/// A directory, not a symlink to one.
fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Measures the project's output, one group per kind. `used_times` says
/// whether access times show when dependencies were last loaded;
/// `source_modified` is the last change to the project's sources.
pub(crate) fn inspect(
    root: &Path,
    used_times: bool,
    source_modified: Option<SystemTime>,
) -> Artifacts {
    let mut kinds: BTreeMap<&'static str, Vec<Output>> = BTreeMap::new();
    for output in locate(root) {
        kinds.entry(output.kind.group).or_default().push(output);
    }
    let groups: Vec<ArtifactGroup> = kinds
        .into_iter()
        .map(|(name, outputs)| group(root, name, &outputs, used_times, source_modified))
        .filter(|group| !mostly_shared(group))
        .collect();
    Artifacts {
        total_bytes: groups.iter().map(|group| group.usage.bytes).sum(),
        groups,
    }
}

fn group(
    root: &Path,
    name: &'static str,
    outputs: &[Output],
    used_times: bool,
    source_modified: Option<SystemTime>,
) -> ArtifactGroup {
    let paths: Vec<PathBuf> = outputs.iter().map(|output| output.path.clone()).collect();
    // Dependencies are written once at install and then only read. Count
    // the project as using them whenever Node loaded one or the sources
    // changed, so they never look unused while the project is worked on.
    let last_used = (name == NODE_MODULES).then(|| {
        let loaded = if used_times {
            paths.iter().filter_map(|dir| last_loaded(dir)).max()
        } else {
            None
        };
        loaded.max(source_modified)
    });
    ArtifactGroup {
        id: name.to_owned(),
        profile: name.to_owned(),
        usage: fsutil::disk_usage_all(&paths),
        last_used: last_used.flatten(),
        // Purge never installs or builds JavaScript itself.
        rebuildable: false,
        parts: outputs
            .iter()
            .flat_map(|output| parts(root, output))
            .collect(),
        paths,
    }
}

/// Dependencies that are mostly hard links into a package manager's store
/// (pnpm, Bun): removing them would free little and cost a reinstall, so
/// they are left alone. The store itself is where that space lives.
fn mostly_shared(group: &ArtifactGroup) -> bool {
    group.id == NODE_MODULES && group.usage.bytes * 2 < group.usage.total
}

/// When Node last loaded any package installed in `node_modules`: every
/// load reads the package's `package.json`. Links back into the project
/// (npm, Yarn and pnpm workspaces link each member there) do not count:
/// those are the project's own files, not installed packages.
fn last_loaded(node_modules: &Path) -> Option<SystemTime> {
    let installed = dunce::canonicalize(node_modules).ok()?;
    let packages = fsutil::children(node_modules).flat_map(|entry| {
        let scoped = entry
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('@'));
        if scoped {
            fsutil::children(&entry).collect()
        } else {
            vec![entry]
        }
    });
    packages
        // Follows pnpm's links to the package itself, inside `node_modules`.
        .filter_map(|package| dunce::canonicalize(package).ok())
        .filter(|package| package.starts_with(&installed))
        .filter_map(|package| fs::metadata(package.join("package.json")).ok())
        .filter_map(|meta| meta.accessed().ok())
        .max()
}

fn parts(root: &Path, output: &Output) -> Vec<Part> {
    output
        .kind
        .parts
        .iter()
        .flat_map(|pattern| match pattern.strip_suffix("/*") {
            Some(parent) => fsutil::children(&output.path.join(parent)).collect(),
            None if *pattern == "*" => fsutil::children(&output.path).collect(),
            None => vec![output.path.join(pattern)],
        })
        .filter(|path| path.exists())
        .filter_map(|path| {
            let usage = fsutil::disk_usage(&path);
            Some(Part {
                id: part_id(root, &path)?,
                bytes: usage.bytes,
                built: usage.newest,
                // Caches are rewritten whenever they are used.
                used: usage.newest,
                toolchain: None,
            })
        })
        .collect()
}

/// A part's id: its path relative to the project root, with `/`.
fn part_id(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// The path a part id names, if it lies strictly inside one of `dirs`.
pub(crate) fn part_path(root: &Path, id: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let relative = Path::new(id);
    let plain = relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    if !plain {
        return None;
    }
    let path = root.join(relative);
    dirs.iter()
        .any(|dir| path.starts_with(dir) && &path != dir)
        .then_some(path)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn found(root: &Path) -> Vec<String> {
        locate(root)
            .iter()
            .map(|output| part_id(root, &output.path).unwrap())
            .collect()
    }

    #[test]
    fn recognises_only_owned_output() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            &root.join("package.json"),
            r#"{"workspaces": ["apps/*"], "devDependencies": {"turbo": "2"}}"#,
        );
        write(&root.join("package-lock.json"), "{}");
        write(&root.join("node_modules/ms/index.js"), "x");
        write(&root.join(".turbo/cache/a"), "x");
        // A Next.js app, and a folder named like Nuxt output without Nuxt.
        write(
            &root.join("apps/web/package.json"),
            r#"{"dependencies": {"next": "16"}}"#,
        );
        write(&root.join("apps/web/.next/cache/images/a"), "x");
        write(&root.join("apps/web/.output/keep"), "x");
        // Never touched: build folders, notes, secrets, Yarn's files.
        write(&root.join("apps/web/dist/app.js"), "x");
        write(&root.join(".env"), "SECRET=1");
        write(&root.join(".yarn/releases/yarn-4.cjs"), "x");

        assert_eq!(found(root), [".turbo", "apps/web/.next", "node_modules"]);
    }

    #[test]
    fn dependencies_need_a_lockfile() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(&root.join("package.json"), "{}");
        write(&root.join("node_modules/ms/index.js"), "x");
        assert!(found(root).is_empty(), "cannot be installed again exactly");
        write(&root.join("pnpm-lock.yaml"), "");
        assert_eq!(found(root), ["node_modules"]);
    }

    #[test]
    fn nested_projects_are_their_own() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(&root.join("package.json"), "{}");
        write(&root.join("bun.lock"), "{}");
        write(&root.join("node_modules/a/index.js"), "x");
        write(&root.join("other/package.json"), "{}");
        write(&root.join("other/yarn.lock"), "");
        write(&root.join("other/node_modules/b/index.js"), "x");
        assert_eq!(found(root), ["node_modules"]);
    }

    #[test]
    fn groups_parts_and_last_use() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            &root.join("package.json"),
            r#"{"dependencies": {"next": "16"}}"#,
        );
        write(&root.join("package-lock.json"), "{}");
        write(&root.join("node_modules/ms/package.json"), "{}");
        write(&root.join("node_modules/@scope/pkg/package.json"), "{}");
        write(&root.join("node_modules/.cache/babel-loader/x"), "x");
        write(&root.join("node_modules/.vite/deps/ms.js"), "x");
        write(&root.join(".next/cache/images/a"), "x");
        write(&root.join(".next/server/page.js"), "x");

        let edited = SystemTime::now() - Duration::from_hours(48);
        let artifacts = inspect(root, false, Some(edited));
        let ids: Vec<&str> = artifacts.groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["next", "node_modules"]);
        let deps = &artifacts.groups[1];
        assert!(!deps.rebuildable);
        assert_eq!(deps.last_used, Some(edited));
        let parts: Vec<&str> = deps.parts.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            parts,
            ["node_modules/.cache/babel-loader", "node_modules/.vite"]
        );
        let next = &artifacts.groups[0];
        assert_eq!(next.parts[0].id, ".next/cache/images");
        assert_eq!(next.last_used, None);
    }

    #[cfg(unix)]
    #[test]
    fn dependencies_linked_from_a_store_are_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        let store = tmp.path().join("store");
        write(&root.join("package.json"), "{}");
        write(&root.join("pnpm-lock.yaml"), "");
        write(&store.join("big.js"), &"x".repeat(64 * 1024));
        fs::create_dir_all(root.join("node_modules/dep")).unwrap();
        fs::hard_link(store.join("big.js"), root.join("node_modules/dep/index.js")).unwrap();
        assert!(inspect(&root, false, None).groups.is_empty());

        // Copied rather than linked (npm), the same files are worth removing.
        fs::remove_file(root.join("node_modules/dep/index.js")).unwrap();
        fs::copy(store.join("big.js"), root.join("node_modules/dep/index.js")).unwrap();
        assert_eq!(inspect(&root, false, None).groups[0].id, NODE_MODULES);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_links_are_not_installed_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let old = SystemTime::now() - Duration::from_hours(240);
        let age = |path: &Path| {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_accessed(old).set_modified(old))
                .unwrap();
        };
        write(&root.join("node_modules/ms/package.json"), "{}");
        age(&root.join("node_modules/ms/package.json"));
        // A workspace member, linked into node_modules and just read.
        write(&root.join("apps/web/package.json"), "{}");
        std::os::unix::fs::symlink("../apps/web", root.join("node_modules/web")).unwrap();
        assert_eq!(last_loaded(&root.join("node_modules")), Some(old));
    }

    #[test]
    fn part_ids_stay_inside_their_group() {
        let root = Path::new("/p");
        let dirs = [root.join("node_modules")];
        assert_eq!(
            part_path(root, "node_modules/.vite", &dirs),
            Some(root.join("node_modules/.vite"))
        );
        for bad in ["node_modules", "src", "node_modules/../src", "/etc", ""] {
            assert_eq!(part_path(root, bad, &dirs), None, "{bad}");
        }
    }
}
