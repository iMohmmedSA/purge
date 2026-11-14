//! Drives the real `cargo` against a throwaway workspace.

use std::{
    collections::HashSet,
    fs,
    path::Path,
    process::{Command, Output},
    time::{Duration, SystemTime},
};

use purge_ecosystem::Ecosystem;
use purge_process::ProcessTable;
use purge_rust::Rust;

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn cargo_build(dir: &Path, args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO"))
        .arg("build")
        .args(args)
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("CARGO_BUILD_BUILD_DIR")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    output
}

fn compiled_anything(output: &Output) -> bool {
    String::from_utf8_lossy(&output.stderr).contains("Compiling")
}

/// A single-package workspace with an optional feature.
fn package(root: &Path) {
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         [features]\nextra = []\n[workspace]\n",
    );
    write(&root.join("src/lib.rs"), "pub fn f() {}\n");
}

fn redirected() -> bool {
    let redirected = std::env::var_os("CARGO_TARGET_DIR").is_some()
        || std::env::var_os("CARGO_BUILD_BUILD_DIR").is_some();
    if redirected {
        eprintln!("skipped: the environment redirects build output");
    }
    redirected
}

#[test]
fn detects_inspects_cleans_and_rebuilds_a_workspace() {
    if redirected() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\"]\nresolver = \"3\"\n",
    );
    write(
        &root.join("member/Cargo.toml"),
        "[package]\nname = \"member\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    );
    write(&root.join("member/src/lib.rs"), "pub fn f() {}\n");

    // Nothing built yet: nothing to reclaim.
    assert_eq!(Rust::new().detect(&root).unwrap(), None);

    cargo_build(&root, &[]);
    cargo_build(&root, &["--release"]);

    // A scan sees the root first; its members then resolve to the workspace
    // from the cache.
    let rust = Rust::new();
    let project = rust.detect(&root).unwrap().unwrap();
    assert_eq!(project.root, root);
    assert_eq!(project.artifact_dir, root.join("target"));
    assert_eq!(
        rust.detect(&root.join("member")).unwrap(),
        Some(project.clone())
    );
    // Without the cache, a member still finds its workspace.
    assert_eq!(
        Rust::new().detect(&root.join("member")).unwrap(),
        Some(project.clone())
    );

    assert!(!project.has_external_artifacts());
    assert!(rust.source_modified(&project).is_some());
    let snapshot = rust.inspect(&project).unwrap();
    let ids: Vec<&str> = snapshot.groups.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["debug", "release"]);
    assert!(
        snapshot
            .groups
            .iter()
            .all(|g| g.usage.bytes > 0 && g.usage.newest.is_some())
    );
    assert!(snapshot.total_bytes >= snapshot.groups.iter().map(|g| g.usage.bytes).sum());
    assert_eq!(rust.busy(&project, &ProcessTable::default()), None);

    let debug = snapshot.group("debug").unwrap();
    rust.clean_group(&project, debug).unwrap();
    assert!(!debug.paths[0].exists());
    assert!(snapshot.group("release").unwrap().paths[0].exists());

    rust.rebuild_group(&project, debug, &["--workspace".into()])
        .unwrap();
    assert!(debug.paths[0].join(".fingerprint").is_dir());

    rust.clean_all(&project).unwrap();
    assert!(!project.artifact_dir.exists());
}

#[test]
fn projects_sharing_a_target_dir_report_the_same_artifact_dir() {
    if redirected() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let shared = base.join("shared-target");
    for name in ["a", "b"] {
        let root = base.join(name);
        write(
            &root.join("Cargo.toml"),
            &format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n"
            ),
        );
        write(&root.join("src/lib.rs"), "");
        write(
            &root.join(".cargo/config.toml"),
            &format!("[build]\ntarget-dir = {:?}\n", shared.to_string_lossy()),
        );
        cargo_build(&root, &[]);
    }

    let rust = Rust::new();
    let a = rust.detect(&base.join("a")).unwrap().unwrap();
    let b = rust.detect(&base.join("b")).unwrap().unwrap();
    assert_eq!(a.artifact_dir, shared);
    assert_eq!(a.artifact_dir, b.artifact_dir);
    assert!(a.has_external_artifacts());

    let artifacts = rust.inspect(&a).unwrap();
    assert_eq!(artifacts.groups.len(), 1);
    rust.clean_all(&a).unwrap();
    assert!(!shared.exists());
}

#[test]
fn separate_build_dir_is_measured_and_cleaned_with_target() {
    if redirected() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let root = base.join("app");
    let build_dir = base.join("build-cache");
    package(&root);
    write(
        &root.join(".cargo/config.toml"),
        &format!("[build]\nbuild-dir = {:?}\n", build_dir.to_string_lossy()),
    );
    cargo_build(&root, &[]);

    let rust = Rust::new();
    let project = rust.detect(&root).unwrap().unwrap();
    assert_eq!(project.artifact_dir, build_dir);
    assert_eq!(project.extra_dirs, [root.join("target")]);
    assert!(project.has_external_artifacts());

    let artifacts = rust.inspect(&project).unwrap();
    let debug = artifacts.group("debug").unwrap();
    assert_eq!(
        debug.paths,
        [build_dir.join("debug"), root.join("target/debug")]
    );
    assert!(!debug.parts.is_empty());

    rust.clean_group(&project, debug).unwrap();
    assert!(debug.paths.iter().all(|path| !path.exists()));
    cargo_build(&root, &[]);
    rust.clean_all(&project).unwrap();
    assert!(project.artifact_dirs().all(|dir| !dir.exists()));
}

#[test]
fn trimming_a_stale_build_keeps_the_live_one_fresh() {
    if redirected() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    package(&root);
    let fingerprints = root.join("target/debug/.fingerprint");
    let units = || -> HashSet<String> {
        fs::read_dir(&fingerprints)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    };

    // The same crate built with two feature sets: two units.
    cargo_build(&root, &[]);
    let stale = units();
    cargo_build(&root, &["--features", "extra"]);
    assert!(units().len() > stale.len());

    // Make the first build look a month old, as if never used since.
    let month_ago = SystemTime::now() - Duration::from_hours(24 * 30);
    for unit in &stale {
        for file in fs::read_dir(fingerprints.join(unit)).unwrap() {
            fs::File::options()
                .write(true)
                .open(file.unwrap().path())
                .unwrap()
                .set_times(
                    fs::FileTimes::new()
                        .set_accessed(month_ago)
                        .set_modified(month_ago),
                )
                .unwrap();
        }
    }

    let rust = Rust::new();
    let project = rust.detect(&root).unwrap().unwrap();
    let artifacts = rust.inspect(&project).unwrap();
    let debug = artifacts.group("debug").unwrap();
    let hashes: Vec<String> = stale
        .iter()
        .filter_map(|unit| unit.rsplit_once('-').map(|(_, hash)| hash.to_owned()))
        .collect();
    let old: Vec<String> = debug
        .parts
        .iter()
        .filter(|part| hashes.contains(&part.id))
        .map(|part| {
            assert_eq!(part.built, Some(month_ago));
            assert!(part.toolchain.is_some());
            part.id.clone()
        })
        .collect();
    assert_eq!(old.len(), hashes.len());

    rust.trim(&project, debug, &old).unwrap();
    assert_eq!(units().intersection(&stale).count(), 0);
    // The live build is untouched: nothing to compile.
    assert!(!compiled_anything(&cargo_build(
        &root,
        &["--features", "extra"]
    )));
    // The trimmed one is simply built again when asked for.
    assert!(compiled_anything(&cargo_build(&root, &[])));
}
