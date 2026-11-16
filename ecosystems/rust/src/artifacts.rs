//! Measuring a workspace's build output, one group per profile.
//!
//! Profiles are recognised in the build directory by the lock files Cargo
//! keeps in each (and, up to Cargo 1.99, a `.fingerprint/` directory). When `build.build-dir` keeps intermediate files
//! apart, `target/` holds only final artifacts, in profile folders of the
//! same names; those join the matching group.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use purge_ecosystem::{ArtifactGroup, Artifacts, Project, fsutil};

use crate::units;

/// Measures every artifact directory of `project`. `used_times` says
/// whether access times on its filesystem show when Cargo last used a unit.
pub(crate) fn inspect(project: &Project, used_times: bool) -> io::Result<Artifacts> {
    let mut groups = BTreeMap::new();
    let mut loose = 0;
    for dir in project.artifact_dirs() {
        loose += collect(dir, &mut groups)?;
    }
    let groups: Vec<ArtifactGroup> = groups
        .into_iter()
        .map(|(id, found)| found.into_group(id, used_times))
        .collect();
    let total_bytes = loose + groups.iter().map(|g| g.usage.bytes).sum::<u64>();
    Ok(Artifacts {
        groups,
        total_bytes,
    })
}

/// A profile's directories, gathered across artifact directories.
#[derive(Debug)]
struct Found {
    profile: String,
    paths: Vec<PathBuf>,
    rebuildable: bool,
}

impl Found {
    fn into_group(self, id: String, used_times: bool) -> ArtifactGroup {
        ArtifactGroup {
            id,
            usage: fsutil::disk_usage_all(&self.paths),
            last_used: None,
            parts: units::parts(&self.paths, used_times),
            profile: self.profile,
            paths: self.paths,
            rebuildable: self.rebuildable,
        }
    }
}

/// Adds the profiles in `dir` to `groups`, and returns the size of
/// everything else in it.
fn collect(dir: &Path, groups: &mut BTreeMap<String, Found>) -> io::Result<u64> {
    let mut loose = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type()?.is_dir() {
            loose += fsutil::disk_usage(&path).bytes;
        } else if is_profile_dir(&path) || groups.contains_key(&name) {
            add(groups, name.clone(), name, path, true);
        } else {
            // A cross-compilation directory such as `x86_64-pc-windows-gnu/`
            // holds profiles cargo can rebuild. A nested target directory,
            // tagged like `target/` itself (rust-analyzer's private one), holds
            // profiles cargo cannot address, so they are only ever removed.
            let rebuildable = !fsutil::is_cache_dir(&path);
            for child in fsutil::children(&path) {
                let profile = file_name(&child);
                let id = format!("{name}/{profile}");
                if child.is_dir() && (is_profile_dir(&child) || groups.contains_key(&id)) {
                    add(groups, id, profile, child, rebuildable);
                } else {
                    // doc/, package/, tmp/ and friends: only removed by a full clean.
                    loose += fsutil::disk_usage(&child).bytes;
                }
            }
        }
    }
    Ok(loose)
}

fn add(
    groups: &mut BTreeMap<String, Found>,
    id: String,
    profile: String,
    path: PathBuf,
    rebuildable: bool,
) {
    groups
        .entry(id)
        .or_insert_with(|| Found {
            profile,
            paths: Vec::new(),
            rebuildable,
        })
        .paths
        .push(path);
}

/// Lock files Cargo keeps in every profile directory it builds into.
/// Cargo 1.99+ uses all of them; older releases only `.cargo-lock`.
pub(crate) const BUILD_LOCKS: &[&str] =
    &[".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];

/// Whether `dir` is a profile directory Cargo built into.
pub(crate) fn is_profile_dir(dir: &Path) -> bool {
    dir.join(".fingerprint").is_dir() || BUILD_LOCKS.iter().any(|lock| dir.join(lock).is_file())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_default(|name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn project(root: &Path, dirs: &[&Path]) -> Project {
        Project {
            ecosystem: crate::NAME,
            name: "p".into(),
            root: root.to_path_buf(),
            artifact_dir: dirs[0].to_path_buf(),
            extra_dirs: dirs[1..].iter().map(|d| d.to_path_buf()).collect(),
        }
    }

    fn file(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn groups_profiles_cross_targets_and_editor_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        for profile in [
            "debug",
            "x86_64-pc-windows-gnu/release",
            "rust-analyzer/debug",
        ] {
            fs::create_dir_all(target.join(profile).join(".fingerprint")).unwrap();
            file(&target.join(profile).join("out"), 8192);
        }
        file(&target.join("CACHEDIR.TAG"), 10);
        file(&target.join("rust-analyzer/CACHEDIR.TAG"), 10);
        file(&target.join("rust-analyzer/flycheck0/stdout"), 8192);

        let artifacts = inspect(&project(dir.path(), &[&target]), false).unwrap();
        let groups: Vec<(&str, &str, bool)> = artifacts
            .groups
            .iter()
            .map(|g| (g.id.as_str(), g.profile.as_str(), g.rebuildable))
            .collect();
        assert_eq!(
            groups,
            [
                ("debug", "debug", true),
                ("rust-analyzer/debug", "debug", false),
                ("x86_64-pc-windows-gnu/release", "release", true),
            ]
        );
        let in_groups: u64 = artifacts.groups.iter().map(|g| g.usage.bytes).sum();
        assert!(
            artifacts.total_bytes > in_groups,
            "flycheck output is counted"
        );
    }

    #[test]
    fn recognises_profiles_of_either_layout() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        // Up to Cargo 1.99: a `.fingerprint/` directory.
        fs::create_dir_all(target.join("debug/.fingerprint")).unwrap();
        // Cargo 1.100: lock files, units under `build/<crate>/<hash>/`.
        file(&target.join("release/.cargo-lock"), 0);
        file(
            &target.join("release/build/x/0123456789abcdef/out/libx.rlib"),
            8192,
        );
        file(&target.join("doc/index.html"), 10);

        let artifacts = inspect(&project(dir.path(), &[&target]), false).unwrap();
        let ids: Vec<&str> = artifacts.groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["debug", "release"]);
        assert_eq!(artifacts.group("release").unwrap().parts.len(), 1);
    }

    #[test]
    fn final_artifacts_join_their_profile_from_the_build_dir() {
        let dir = tempfile::tempdir().unwrap();
        let build = dir.path().join("cache");
        let target = dir.path().join("target");
        fs::create_dir_all(build.join("debug/.fingerprint")).unwrap();
        file(&build.join("debug/deps/libx-0123456789abcdef.rlib"), 8192);
        file(&target.join("debug/app"), 8192);
        file(&target.join("doc/index.html"), 8192);

        let artifacts = inspect(&project(dir.path(), &[&build, &target]), false).unwrap();
        assert_eq!(artifacts.groups.len(), 1);
        let debug = &artifacts.groups[0];
        assert_eq!(debug.paths, [build.join("debug"), target.join("debug")]);
        assert!(debug.usage.bytes >= 2 * 8192);
        assert!(artifacts.total_bytes >= debug.usage.bytes + 8192);
    }
}
