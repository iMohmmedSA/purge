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
