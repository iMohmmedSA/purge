//! Filesystem measurements shared by ecosystems.

use std::{
    collections::HashMap,
    fs::{self, File, FileTimes, Metadata},
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use crate::DiskUsage;

/// Measures a tree without following symlinks. Unreadable entries are skipped.
pub fn disk_usage(path: &Path) -> DiskUsage {
    disk_usage_all([path])
}

/// Measures several trees together, counting a file linked into more than
/// one of them once.
pub fn disk_usage_all<P: AsRef<Path>>(paths: impl IntoIterator<Item = P>) -> DiskUsage {
    let mut usage = DiskUsage::default();
    let mut linked: HashMap<Inode, Links> = HashMap::new();
    let mut stack: Vec<PathBuf> = paths
        .into_iter()
        .map(|p| p.as_ref().to_path_buf())
        .collect();
    while let Some(path) = stack.pop() {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            stack.extend(children(&path));
        } else {
            usage.newest = usage.newest.max(meta.modified().ok());
            let size = allocated(&meta);
            match hard_links(&meta) {
                None => {
                    usage.bytes += size;
                    usage.total += size;
                }
                Some((inode, count)) => {
                    let links = linked.entry(inode).or_insert(Links {
                        seen: 0,
                        count,
                        size,
                    });
                    if links.seen == 0 {
                        usage.total += size;
                    }
                    links.seen += 1;
                }
            }
        }
    }
    // A hard-linked file is only freed once every one of its links goes.
    usage.bytes += linked
        .values()
        .filter(|links| links.seen >= links.count)
        .map(|links| links.size)
        .sum::<u64>();
    usage
}

/// A file's identity: device and inode number.
type Inode = (u64, u64);

/// A hard-linked file met while measuring.
struct Links {
    /// Links found inside the measured trees.
    seen: u64,
    /// Links the file has in all.
    count: u64,
    size: u64,
}

/// Whether the filesystem holding `dir` updates access times when files are
/// read, at least daily (Linux's default `relatime` does; `noatime` mounts
/// and Windows by default do not). Finds out by reading a scratch file in
/// `dir` whose times were set two days back.
pub fn records_access_time(dir: &Path) -> bool {
    let probe = dir.join(format!(".purge-atime-probe-{}", std::process::id()));
    let result = probe_access_time(&probe);
    // Best effort: a leftover probe is harmless and replaced next time.
    let _ = fs::remove_file(&probe);
    result.unwrap_or(false)
}

fn probe_access_time(probe: &Path) -> io::Result<bool> {
    let past = SystemTime::now() - Duration::from_hours(48);
    let file = File::create(probe)?;
    file.set_times(FileTimes::new().set_accessed(past).set_modified(past))?;
    drop(file);
    fs::read(probe)?;
    let accessed = fs::metadata(probe)?.accessed()?;
    Ok(accessed > past + Duration::from_hours(24))
}

/// Reads a file without counting as a use of it: its access time is put
/// back afterwards, so purge's own reads never make old files look used.
pub fn read_unnoticed(path: &Path) -> io::Result<Vec<u8>> {
    let accessed = fs::metadata(path)?.accessed().ok();
    let bytes = fs::read(path)?;
    if let Some(accessed) = accessed {
        // Best effort: failing only risks the file looking recently used.
        let _ = File::options()
            .write(true)
            .open(path)
            .and_then(|file| file.set_times(FileTimes::new().set_accessed(accessed)));
    }
    Ok(bytes)
}

/// Most recent modification time in a tree, skipping (below `root`) every
/// directory for which `skip` returns true. Symlinks are not followed.
pub fn newest_mtime(root: &Path, skip: &dyn Fn(&Path) -> bool) -> Option<SystemTime> {
    let mut newest = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if path != root && skip(&path) {
                continue;
            }
            stack.extend(children(&path));
        }
        newest = newest.max(meta.modified().ok());
    }
    newest
}

/// Whether `dir` is marked as a cache by a `CACHEDIR.TAG` file, as Cargo
/// does for `target/`. See <https://bford.info/cachedir/>.
pub fn is_cache_dir(dir: &Path) -> bool {
    dir.join("CACHEDIR.TAG").is_file()
}

/// Dependency folders that are huge and never hold the user's own work.
const DEPENDENCY_DIRS: &[&str] = &["node_modules"];

/// Whether walks over a project tree should skip `dir`: hidden folders
/// (version control, editor state), tagged caches such as `target/`, and
/// vendored dependency folders.
pub fn is_ignored_dir(dir: &Path) -> bool {
    is_hidden(dir)
        || is_cache_dir(dir)
        || dir
            .file_name()
            .is_some_and(|name| DEPENDENCY_DIRS.iter().any(|skip| name == *skip))
}

/// Whether the final path component starts with a dot.
pub fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
}

/// Entries directly inside `dir`; nothing if it cannot be read.
pub fn children(dir: &Path) -> impl Iterator<Item = PathBuf> + use<> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
}

#[cfg(unix)]
fn allocated(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks() * 512
}

#[cfg(not(unix))]
fn allocated(meta: &Metadata) -> u64 {
    meta.len()
}

/// The identity and link count of a file with more than one hard link, as
/// Cargo makes for final artifacts and pnpm and Bun for every installed
/// file. `None` for a file with a single link.
#[cfg(unix)]
fn hard_links(meta: &Metadata) -> Option<(Inode, u64)> {
    use std::os::unix::fs::MetadataExt;
    (meta.nlink() > 1).then(|| ((meta.dev(), meta.ino()), meta.nlink()))
}

/// Standard Rust cannot read link counts on Windows, so every file counts
/// as freed there.
#[cfg(not(unix))]
fn hard_links(_meta: &Metadata) -> Option<(Inode, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_tree_and_counts_hard_links_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        fs::write(&file, vec![1u8; 64 * 1024]).unwrap();
        let single = disk_usage(dir.path()).bytes;
        assert!(single >= 64 * 1024);

        fs::hard_link(&file, dir.path().join("b.bin")).unwrap();
        let linked = disk_usage(dir.path()).bytes;
        if cfg!(unix) {
            assert_eq!(single, linked);
        }
        assert!(disk_usage(dir.path()).newest.is_some());
    }

    #[test]
    fn files_linked_from_outside_are_not_freed() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        let project = dir.path().join("project");
        fs::create_dir_all(&store).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(store.join("shared.js"), vec![1u8; 64 * 1024]).unwrap();
        fs::hard_link(store.join("shared.js"), project.join("shared.js")).unwrap();
        fs::write(project.join("own.js"), vec![1u8; 8 * 1024]).unwrap();

        let usage = disk_usage(&project);
        assert!(usage.total >= 72 * 1024);
        if cfg!(unix) {
            // Removing the project frees only its own file.
            assert!(usage.bytes < 64 * 1024, "{usage:?}");
            assert!(usage.bytes >= 8 * 1024);
            // Measured together with the store, the shared file is freed too.
            assert_eq!(
                disk_usage_all([&project, &store]).bytes,
                disk_usage_all([&project, &store]).total
            );
        }
    }

    #[test]
    fn directory_times_do_not_count_as_use() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a");
        fs::write(&file, "x").unwrap();
        let past = SystemTime::now() - Duration::from_hours(240);
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(past)
            .unwrap();
        // Creating and removing a file bumps the directory's time only.
        fs::write(dir.path().join("b"), "x").unwrap();
        fs::remove_file(dir.path().join("b")).unwrap();
        assert_eq!(disk_usage(dir.path()).newest, Some(past));
    }

    #[test]
    fn reading_unnoticed_keeps_the_access_time() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.json");
        fs::write(&file, "{}").unwrap();
        let past = SystemTime::now() - Duration::from_hours(240);
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(FileTimes::new().set_accessed(past).set_modified(past))
            .unwrap();
        assert_eq!(read_unnoticed(&file).unwrap(), b"{}");
        assert_eq!(fs::metadata(&file).unwrap().accessed().unwrap(), past);
    }

    #[test]
    fn access_time_probe_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        // Either answer is fine here; it depends on how /tmp is mounted.
        let _ = records_access_time(dir.path());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn missing_path_is_empty() {
        assert_eq!(
            disk_usage(Path::new("/definitely/not/here")),
            DiskUsage::default()
        );
    }

    #[test]
    fn newest_mtime_honours_skip() {
        let dir = tempfile::tempdir().unwrap();
        let skipped = dir.path().join("target");
        fs::create_dir(&skipped).unwrap();
        let artifact = skipped.join("x");
        fs::write(&artifact, "x").unwrap();
        let future = SystemTime::now() + std::time::Duration::from_hours(8760);
        fs::File::options()
            .write(true)
            .open(&artifact)
            .unwrap()
            .set_modified(future)
            .unwrap();

        let skipping = newest_mtime(dir.path(), &|p| p.ends_with("target")).unwrap();
        assert!(skipping < future);
        assert_eq!(newest_mtime(dir.path(), &|_| false), Some(future));
    }

    #[test]
    fn detects_hidden_and_cache_dirs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_hidden(Path::new("/x/.git")));
        assert!(!is_hidden(Path::new("/x/src")));
        assert!(!is_cache_dir(dir.path()));
        fs::write(dir.path().join("CACHEDIR.TAG"), "Signature").unwrap();
        assert!(is_cache_dir(dir.path()));
        assert!(is_ignored_dir(dir.path()));
        assert!(is_ignored_dir(Path::new("/x/web/node_modules")));
        assert!(!is_ignored_dir(Path::new("/x/src")));
    }
}
