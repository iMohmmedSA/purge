//! Process-level helpers: spawning commands that stay out of the user's way,
//! finding processes working inside a directory, and sampling system load.

use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

pub use sysinfo::Error;
use sysinfo::{
    CpuRefreshKind, MINIMUM_CPU_UPDATE_INTERVAL, ProcessRefreshKind, ProcessesToUpdate,
    RefreshKind, System, UpdateKind,
};

/// Puts the current process into background mode.
///
/// On Unix this drops the CPU priority to the lowest level (nice 19). Child
/// processes inherit it, so every `cargo` purge starts runs at the same
/// priority, and on Linux their default I/O priority follows it down. On
/// Windows, [`configure_background`] lowers each child instead.
pub fn enter_background_mode() -> io::Result<()> {
    #[cfg(unix)]
    {
        /// Lowest scheduling priority on every Unix.
        const LOWEST_PRIORITY: i32 = 19;
        rustix::process::setpriority_process(None, LOWEST_PRIORITY).map_err(io::Error::from)
    }
    #[cfg(not(unix))]
    {
        Ok(())
    }
}

/// Prepares a command purge is about to start. On Windows this gives it idle
/// priority and no console window. On Unix there is nothing to add, because
/// the child inherits the priority set by [`enter_background_mode`].
pub fn configure_background(cmd: &mut Command) -> &mut Command {
    imp::configure_background(cmd)
}

#[cfg(not(windows))]
mod imp {
    use std::process::Command;

    pub(crate) fn configure_background(cmd: &mut Command) -> &mut Command {
        cmd
    }
}

#[cfg(windows)]
mod imp {
    use std::{os::windows::process::CommandExt, process::Command};

    const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub(crate) fn configure_background(cmd: &mut Command) -> &mut Command {
        cmd.creation_flags(IDLE_PRIORITY_CLASS | CREATE_NO_WINDOW)
    }
}

/// A process seen by [`ProcessTable::capture`].
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    /// Executable name without any `.exe` suffix.
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub args: Vec<PathBuf>,
}

impl ProcessInfo {
    fn works_in(&self, root: &Path) -> bool {
        self.cwd.as_deref().is_some_and(|cwd| cwd.starts_with(root))
            || self.args.iter().any(|arg| arg.starts_with(root))
    }
}

/// A snapshot of the running processes, captured once per run.
#[derive(Debug, Default)]
pub struct ProcessTable {
    processes: Vec<ProcessInfo>,
}

impl ProcessTable {
    pub fn capture() -> Result<Self, Error> {
        let mut system = System::new()?;
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cwd(UpdateKind::Always)
                .with_cmd(UpdateKind::Always),
        );
        let processes = system
            .processes()
            .values()
            .map(|process| ProcessInfo {
                name: executable_name(process.name()),
                cwd: process.cwd().map(Path::to_path_buf),
                args: process.cmd().iter().map(PathBuf::from).collect(),
            })
            .collect();
        Ok(Self { processes })
    }

    /// Returns a process named one of `names` that is working inside `root`,
    /// either through its working directory or a path argument.
    pub fn find(&self, names: &[&str], root: &Path) -> Option<&ProcessInfo> {
        self.processes.iter().find(|process| {
            names
                .iter()
                .any(|name| process.name.eq_ignore_ascii_case(name))
                && process.works_in(root)
        })
    }
}

fn executable_name(name: &OsStr) -> String {
    let name = name.to_string_lossy();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    name.to_owned()
}

/// Samples overall CPU usage in percent. Blocks for about a second.
pub fn cpu_usage() -> Result<f32, Error> {
    let mut system = System::new_with_specifics(
        RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_usage()),
    )?;
    thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL.max(std::time::Duration::from_secs(1)));
    system.refresh_cpu_usage();
    Ok(system.global_cpu_usage())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_by_cwd_or_argument() {
        let table = ProcessTable {
            processes: vec![
                ProcessInfo {
                    name: "cargo".into(),
                    cwd: Some("/work/app/src".into()),
                    args: vec![],
                },
                ProcessInfo {
                    name: "rustc".into(),
                    cwd: Some("/".into()),
                    args: vec!["/work/lib/src/lib.rs".into()],
                },
            ],
        };
        assert!(table.find(&["cargo"], Path::new("/work/app")).is_some());
        assert!(table.find(&["rustc"], Path::new("/work/lib")).is_some());
        assert!(table.find(&["cargo"], Path::new("/work/lib")).is_none());
    }

    #[test]
    fn strips_exe_suffix() {
        assert_eq!(executable_name(OsStr::new("cargo.exe")), "cargo");
    }
}
