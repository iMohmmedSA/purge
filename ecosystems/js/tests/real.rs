//! Drives real package managers against throwaway projects. Each test is
//! skipped when its package manager is not installed.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use purge_ecosystem::Ecosystem;
use purge_js::JavaScript;

/// Runs `program` in `dir`, or returns `None` if it is not installed.
fn run(dir: &Path, program: &str, args: &[&str]) -> Option<Output> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("npm_config_audit", "false")
        .env("npm_config_fund", "false")
        .env("npm_config_update_notifier", "false")
        .output()
        .ok()?;
    assert!(output.status.success(), "{program} {args:?}: {output:?}");
    Some(output)
}

fn package(dir: &Path) {
    fs::write(
        dir.join("package.json"),
        r#"{"name": "t", "version": "1.0.0", "private": true}"#,
    )
    .unwrap();
}

#[test]
fn npm_dependencies_are_measured_cleaned_and_reinstalled() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    package(&root);
    if run(&root, "npm", &["install", "ms@2.1.3"]).is_none() {
        eprintln!("skipped: npm is not installed");
        return;
    }

    let js = JavaScript::new();
    let project = js.detect(&root).unwrap().expect("a project");
    let artifacts = js.inspect(&project).unwrap();
    let deps = artifacts.group("node_modules").unwrap();
    // npm copies packages: removing them frees all of it.
    assert!(deps.usage.bytes > 0);
    assert_eq!(deps.usage.bytes, deps.usage.total);
    assert!(deps.last_used.is_some());

    js.clean_group(&project, deps).unwrap();
    assert!(!root.join("node_modules").exists());
    assert!(root.join("package-lock.json").exists(), "lockfile kept");
    // The lockfile brings back exactly what was there.
    run(&root, "npm", &["ci"]);
    assert!(root.join("node_modules/ms/index.js").exists());
}

#[test]
fn pnpm_dependencies_are_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    package(&root);
    if run(&root, "pnpm", &["add", "ms@2.1.3"]).is_none() {
        eprintln!("skipped: pnpm is not installed");
        return;
    }

    // Every package file is a hard link into pnpm's store: removing
    // `node_modules` would free next to nothing and cost a reinstall, so
    // purge leaves it alone.
    let js = JavaScript::new();
    let project = js.detect(&root).unwrap().expect("a project");
    let artifacts = js.inspect(&project).unwrap();
    assert_eq!(artifacts.group("node_modules"), None);
    js.clean_all(&project).unwrap();
    assert!(root.join("node_modules/ms/index.js").exists());
}

#[test]
fn pnpm_store_counts_only_what_no_project_uses() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    package(&root);
    if run(&root, "pnpm", &["add", "is-odd@3.0.1"]).is_none() {
        eprintln!("skipped: pnpm is not installed");
        return;
    }
    let js = JavaScript::new();
    let store = |js: &JavaScript| {
        js.shared_caches(std::slice::from_ref(&root))
            .into_iter()
            .find(|cache| cache.id == "pnpm")
            .expect("the pnpm store serving the project")
    };
    // A package no other test installs: tests run in parallel, and a
    // package another project links to is rightly not unused.
    let used = store(&js);
    assert!(used.unused_only);
    let index = fs::metadata(root.join("node_modules/is-odd/index.js")).unwrap();

    // Once no project links to the package, its files count as unused.
    fs::remove_dir_all(root.join("node_modules")).unwrap();
    let unused = store(&js);
    assert!(
        unused.unused_bytes >= used.unused_bytes + index.len(),
        "{used:?} -> {unused:?}"
    );
}
