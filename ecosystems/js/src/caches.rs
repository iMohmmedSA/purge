//! The package caches JavaScript tools share between projects.
//!
//! | Cache | Freed by | Only what is unused? |
//! |---|---|---|
//! | pnpm store | `pnpm store prune` | yes: packages no project's `node_modules` links to |
//! | npm cache | `npm cache clean --force` | no: npm does not track use; projects keep their own copies |
//! | Bun cache | `bun pm cache rm` | no; but projects keep the files they link to |
//!
//! Yarn's cache is never touched: with Plug'n'Play, projects load their
//! packages straight from it.
//!
//! Every file a project uses from pnpm's store or Bun's cache is a hard
//! link to it, so files with a single link are exactly what freeing the
//! cache frees, whichever project might use the rest.

use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use purge_ecosystem::{Result, SharedCache, command, fsutil};
use purge_process::configure_background;

/// Time limits for the package managers' own commands.
const QUERY_TIMEOUT: Duration = Duration::from_mins(1);
const PRUNE_TIMEOUT: Duration = Duration::from_mins(30);

/// The caches serving projects under `roots`, measured. Caches whose tool
/// is not installed are skipped.
pub(crate) fn find(roots: &[PathBuf]) -> Vec<SharedCache> {
    let mut caches = Vec::new();
    if let Some(pnpm) = Tool::locate("pnpm") {
        // One store per filesystem: ask from each root.
        let mut stores: Vec<PathBuf> = roots
            .iter()
            .filter(|root| root.is_dir())
            .filter_map(|root| pnpm.query(root, &["store", "path"]))
            .collect();
        stores.sort();
        stores.dedup();
        caches.extend(stores.into_iter().map(|store| SharedCache {
            id: "pnpm",
            name: "pnpm store".into(),
            total_bytes: fsutil::disk_usage(&store).total,
            unused_bytes: fsutil::unlinked_bytes(&store.join("files")),
            path: store,
            unused_only: true,
            tools: &["pnpm", "pnpx"],
        }));
    }
    if let Some(npm) = Tool::locate("npm") {
        // npm refuses to print the setting when it looks like a secret.
        let dir = npm
            .query(&home(), &["config", "get", "cache"])
            .unwrap_or_else(npm_default_cache);
        let cache = dir.join("_cacache");
        if cache.is_dir() {
            let total = fsutil::disk_usage(&cache).total;
            caches.push(SharedCache {
                id: "npm",
                name: "npm cache".into(),
                path: cache,
                total_bytes: total,
                // npm copies packages into projects: it is all unused.
                unused_bytes: total,
                unused_only: false,
                tools: &["npm", "npx"],
            });
        }
    }
    if let Some(bun) = Tool::locate("bun")
        && let Some(cache) = bun.query(&home(), &["pm", "cache"])
        && cache.is_dir()
    {
        caches.push(SharedCache {
            id: "bun",
            name: "Bun cache".into(),
            total_bytes: fsutil::disk_usage(&cache).total,
            unused_bytes: fsutil::unlinked_bytes(&cache),
            path: cache,
            unused_only: false,
            tools: &["bun", "bunx"],
        });
    }
    caches
}

/// Frees space in `cache` with its tool's own command.
pub(crate) fn prune(cache: &SharedCache) -> Result<()> {
    let program = match cache.id {
        "pnpm" => "pnpm",
        "npm" => "npm",
        "bun" => "bun",
        other => {
            return Err(purge_ecosystem::Error::Invalid(format!(
                "unknown cache `{other}`"
            )));
        }
    };
    let tool = Tool::locate(program).ok_or_else(|| {
        purge_ecosystem::Error::Invalid(format!("`{program}` is no longer installed"))
    })?;
    let path = cache.path.as_os_str();
    let mut cmd = match cache.id {
        "pnpm" => tool.command(&home(), &["store", "prune", "--store-dir"], Some(path)),
        // The cache folder is the parent of `_cacache`.
        "npm" => tool.command(
            &home(),
            &["cache", "clean", "--force", "--cache"],
            cache.path.parent().map(Path::as_os_str),
        ),
        _ => tool.command(&home(), &["pm", "cache", "rm"], None),
    };
    command::run(&mut cmd, PRUNE_TIMEOUT).map(drop)
}

/// A package manager found on this machine.
#[derive(Debug)]
struct Tool {
    program: PathBuf,
}

impl Tool {
    /// Finds `name` on the `PATH`, then where installers usually put it:
    /// a background service may not get the `PATH` of a terminal (version
    /// managers such as fnm change it per terminal).
    fn locate(name: &str) -> Option<Self> {
        let home = home();
        let fallbacks = [
            home.join(".local/share/fnm/aliases/default/bin"),
            home.join(".local/share/pnpm"),
            home.join(".bun/bin"),
            home.join(".volta/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ];
        env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
            .chain(fallbacks)
            .flat_map(|dir| file_names(name).map(move |file| dir.join(file)))
            .find(|candidate| candidate.is_file())
            .map(|program| Self { program })
    }

    /// Runs the tool and returns the path it prints.
    fn query(&self, dir: &Path, args: &[&str]) -> Option<PathBuf> {
        let output = command::run(&mut self.command(dir, args, None), QUERY_TIMEOUT).ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text.lines().map(str::trim).rfind(|line| !line.is_empty())?;
        Some(PathBuf::from(line))
    }

    fn command(&self, dir: &Path, args: &[&str], last: Option<&std::ffi::OsStr>) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.current_dir(dir).args(args).args(last);
        // npm and pnpm are Node programs: make sure the Node installed
        // beside them is the one found.
        if let Some(dir) = self.program.parent() {
            let mut path = OsString::from(dir);
            if let Some(rest) = env::var_os("PATH") {
                path.push(if cfg!(windows) { ";" } else { ":" });
                path.push(rest);
            }
            cmd.env("PATH", path);
        }
        cmd.env("NO_COLOR", "1")
            .env("npm_config_update_notifier", "false");
        configure_background(&mut cmd);
        cmd
    }
}

/// Where npm keeps its cache unless configured otherwise.
fn npm_default_cache() -> PathBuf {
    if let Some(dir) = env::var_os("npm_config_cache").or_else(|| env::var_os("NPM_CONFIG_CACHE")) {
        return PathBuf::from(dir);
    }
    if cfg!(windows)
        && let Some(local) = env::var_os("LOCALAPPDATA")
    {
        return PathBuf::from(local).join("npm-cache");
    }
    home().join(".npm")
}

/// File names a program may have: Windows installs npm and pnpm as `.cmd`
/// scripts.
fn file_names(name: &str) -> impl Iterator<Item = String> + use<> {
    let names = if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.cmd")]
    } else {
        vec![name.to_owned()]
    };
    names.into_iter()
}

fn home() -> PathBuf {
    env::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_scripts_are_found_too() {
        let names: Vec<String> = file_names("npm").collect();
        if cfg!(windows) {
            assert_eq!(names, ["npm.exe", "npm.cmd"]);
        } else {
            assert_eq!(names, ["npm"]);
        }
    }
}
