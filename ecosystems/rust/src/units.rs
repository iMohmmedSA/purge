//! The units Cargo builds inside a profile, and removing stale ones.
//!
//! Cargo compiles each crate once per set of options (features, profile
//! settings, compiler, ...) into a *unit* named `<name>-<hash>`, where the
//! 16-hex-digit hash identifies the unit. Its files sit in `.fingerprint/`,
//! `deps/`, `build/` and `examples/` under that hash, and its fingerprint
//! JSON records which compiler built it (`rustc`, a hash of `rustc -vV`).
//! Every check whether a unit is fresh reads its fingerprint, so where the
//! filesystem records access times, they show when a unit was last used.
//!
//! Units are never shared or updated in place: a changed crate, option or
//! compiler makes a new unit beside the old one, which is left behind for
//! good. Incremental compilation caches (`incremental/<name>`) are rewritten
//! whenever their crate is compiled, so their modification time shows when
//! they were last used.

use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use purge_ecosystem::{Part, fsutil};
use serde::Deserialize;

/// Folders of a profile holding units' files. Fingerprints come first, so a
/// removal cut short leaves units Cargo rebuilds rather than trusts.
const UNIT_DIRS: &[&str] = &[".fingerprint", "deps", "build", "examples", ""];

const INCREMENTAL: &str = "incremental";
const INCREMENTAL_PREFIX: &str = "incremental/";

/// The removable parts of a profile kept in `dirs`.
pub(crate) fn parts(dirs: &[PathBuf], used_times: bool) -> Vec<Part> {
    let mut units: BTreeMap<String, Part> = BTreeMap::new();
    for dir in dirs {
        for fingerprint in fsutil::children(&dir.join(".fingerprint")) {
            let Some(hash) = unit_hash(&file_name(&fingerprint)).map(str::to_owned) else {
                continue;
            };
            let found = read_fingerprint(&fingerprint, used_times);
            let unit = units.entry(hash.clone()).or_insert_with(|| Part {
                id: hash,
                bytes: 0,
                built: None,
                used: None,
                toolchain: None,
            });
            unit.built = unit.built.max(found.built);
            unit.used = unit.used.max(found.used);
            unit.toolchain = unit.toolchain.take().or(found.toolchain);
        }
    }
    for dir in dirs {
        for sub in UNIT_DIRS {
            for entry in fsutil::children(&dir.join(sub)) {
                if let Some(unit) = unit_hash(&file_name(&entry)).and_then(|h| units.get_mut(h)) {
                    unit.bytes += fsutil::disk_usage(&entry).bytes;
                }
            }
        }
    }
    let mut parts: Vec<Part> = units.into_values().collect();
    for dir in dirs {
        for cache in fsutil::children(&dir.join(INCREMENTAL)) {
            let usage = fsutil::disk_usage(&cache);
            parts.push(Part {
                id: format!("{INCREMENTAL_PREFIX}{}", file_name(&cache)),
                bytes: usage.bytes,
                built: usage.newest,
                used: usage.newest,
                toolchain: None,
            });
        }
    }
    parts
}

/// Removes the parts named by `ids` from a profile kept in `dirs`.
pub(crate) fn trim(dirs: &[PathBuf], ids: &[String]) -> io::Result<()> {
    let mut hashes = HashSet::new();
    let mut caches = Vec::new();
    for id in ids {
        match id.strip_prefix(INCREMENTAL_PREFIX) {
            Some(name) if is_plain_name(name) => caches.push(name),
            None if is_hash(id) => {
                hashes.insert(id.as_str());
            }
            _ => return Err(invalid(id)),
        }
    }
    for dir in dirs {
        for sub in UNIT_DIRS {
            for entry in fsutil::children(&dir.join(sub)) {
                if unit_hash(&file_name(&entry)).is_some_and(|h| hashes.contains(h)) {
                    remove(&entry)?;
                }
            }
        }
        for name in &caches {
            remove(&dir.join(INCREMENTAL).join(name))?;
        }
    }
    Ok(())
}

/// The unit hash in a file name such as `libserde-0123456789abcdef.rlib`:
/// the text after the last `-` before the first `.`.
fn unit_hash(name: &str) -> Option<&str> {
    let stem = name.split('.').next()?;
    let (_, hash) = stem.rsplit_once('-')?;
    is_hash(hash).then_some(hash)
}

fn is_hash(text: &str) -> bool {
    text.len() == 16 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A single path component, so an id can never reach outside its folder.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\'])
}

fn invalid(id: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("not a cargo unit: `{id}`"),
    )
}

/// Removes a file, symlink or directory; already gone is fine.
fn remove(path: &Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(err) => Err(err),
    };
    match result {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// What a unit's fingerprint folder tells.
#[derive(Debug, Default)]
struct Fingerprint {
    built: Option<SystemTime>,
    used: Option<SystemTime>,
    toolchain: Option<String>,
}

#[derive(Deserialize)]
struct FingerprintJson {
    rustc: u64,
}

fn read_fingerprint(dir: &Path, used_times: bool) -> Fingerprint {
    let mut found = Fingerprint::default();
    let mut json = Vec::new();
    // Times first: reading the JSON below must not count as a use.
    for file in fsutil::children(dir) {
        let Ok(meta) = fs::symlink_metadata(&file) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().ok();
        found.built = found.built.max(modified);
        if used_times {
            found.used = found.used.max(meta.accessed().ok()).max(modified);
        }
        if file.extension().is_some_and(|ext| ext == "json") {
            json.push(file);
        }
    }
    found.toolchain = json.iter().find_map(|file| {
        let bytes = fsutil::read_unnoticed(file).ok()?;
        let parsed: FingerprintJson = serde_json::from_slice(&bytes).ok()?;
        // Some build script fingerprints record 0 rather than a compiler.
        (parsed.rustc != 0).then(|| parsed.rustc.to_string())
    });
    found
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const OLD: &str = "0123456789abcdef";
    const NEW: &str = "fedcba9876543210";

    fn file(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// A profile with two builds of `foo` (by different compilers) and an
    /// incremental cache.
    fn profile() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let debug = tmp.path().join("debug");
        for (hash, rustc) in [(OLD, 111), (NEW, 222)] {
            let fp = debug.join(format!(".fingerprint/foo-{hash}"));
            file(&fp.join("lib-foo"), "x");
            file(
                &fp.join("lib-foo.json"),
                &format!(r#"{{"rustc":{rustc},"features":"[]"}}"#),
            );
            file(
                &debug.join(format!("deps/libfoo-{hash}.rlib")),
                &"x".repeat(5000),
            );
            file(&debug.join(format!("deps/foo-{hash}.d")), "deps");
            file(&debug.join(format!("build/foo-{hash}/out/gen.rs")), "gen");
        }
        file(
            &debug.join("incremental/foo-1abcdefghijk/s-1/dep-graph.bin"),
            "g",
        );
        file(&debug.join("libfoo.rlib"), "final");
        file(&debug.join("deps/unrelated-nothash.d"), "keep");
        (tmp, debug)
    }

    #[test]
    fn finds_units_and_caches() {
        let (_tmp, debug) = profile();
        let parts = parts(&[debug], false);
        let ids: Vec<&str> = parts.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, [OLD, NEW, "incremental/foo-1abcdefghijk"]);
        assert_eq!(parts[0].toolchain.as_deref(), Some("111"));
        assert_eq!(parts[1].toolchain.as_deref(), Some("222"));
        assert!(parts[0].bytes >= 5000);
        assert!(parts[0].built.is_some());
        assert_eq!(parts[0].used, None, "access times not trusted");
        assert_eq!(parts[2].toolchain, None);
        assert!(parts[2].used.is_some());
    }

    #[test]
    fn records_last_use_from_access_times_without_touching_them() {
        let (_tmp, debug) = profile();
        let json = debug.join(format!(".fingerprint/foo-{OLD}/lib-foo.json"));
        let past = SystemTime::now() - Duration::from_hours(24 * 30);
        for entry in fs::read_dir(json.parent().unwrap()).unwrap() {
            let path = entry.unwrap().path();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_accessed(past).set_modified(past))
                .unwrap();
        }
        let parts = parts(&[debug], true);
        assert_eq!(parts[0].used, Some(past));
        // Reading the compiler out of the JSON left its access time alone.
        assert_eq!(fs::metadata(&json).unwrap().accessed().unwrap(), past);
    }

    #[test]
    fn trims_only_the_named_parts() {
        let (_tmp, debug) = profile();
        trim(
            std::slice::from_ref(&debug),
            &[OLD.to_owned(), "incremental/foo-1abcdefghijk".to_owned()],
        )
        .unwrap();
        assert!(!debug.join(format!(".fingerprint/foo-{OLD}")).exists());
        assert!(!debug.join(format!("deps/libfoo-{OLD}.rlib")).exists());
        assert!(!debug.join(format!("build/foo-{OLD}")).exists());
        assert!(!debug.join("incremental/foo-1abcdefghijk").exists());
        for kept in [
            format!(".fingerprint/foo-{NEW}"),
            format!("deps/libfoo-{NEW}.rlib"),
            format!("build/foo-{NEW}"),
            "libfoo.rlib".to_owned(),
            "deps/unrelated-nothash.d".to_owned(),
        ] {
            assert!(debug.join(&kept).exists(), "{kept} was removed");
        }
    }

    #[test]
    fn refuses_ids_that_are_not_parts() {
        let (_tmp, debug) = profile();
        for bad in ["../../etc", "incremental/../deps", "incremental/", "xyz"] {
            assert!(
                trim(std::slice::from_ref(&debug), &[bad.to_owned()]).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn parses_unit_hashes() {
        assert_eq!(unit_hash("libserde-0123456789abcdef.rlib"), Some(OLD));
        assert_eq!(unit_hash("serde-0123456789abcdef"), Some(OLD));
        assert_eq!(unit_hash("build-script-build"), None);
        assert_eq!(unit_hash("foo-0123456789ABCDEF"), None);
        assert_eq!(unit_hash("foo-1abcdefghijk"), None);
    }
}
