//! Registers the purge daemon to start at login:
//! a systemd user unit on Linux, a launchd agent on macOS, and a
//! `HKCU\...\Run` entry on Windows. None of these need administrator rights.

use std::{ffi::OsString, io, path::PathBuf};

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod command;

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "unsupported.rs"
)]
mod platform;

/// Identifier used for the unit, agent label and registry value.
pub const SERVICE_NAME: &str = "purge";

/// What the service should run.
#[derive(Debug, Clone)]
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// `PATH` captured at install time, so the daemon can find `cargo` and
    /// friends even when the service manager starts it with a minimal
    /// environment.
    pub path_env: Option<OsString>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("`{command}` failed: {stderr}")]
    Command { command: String, stderr: String },
    #[error("could not determine the home directory")]
    NoHome,
    #[error("background services are not supported on this platform")]
    Unsupported,
}

/// Installs and starts the service. Returns a human-readable description.
pub fn install(spec: &Spec) -> Result<String, Error> {
    platform::install(spec)
}

/// Stops and removes the service. Returns a human-readable description.
pub fn uninstall() -> Result<String, Error> {
    platform::uninstall()
}

/// Stops the service without removing it; it starts again at the next login
/// or install. Returns a human-readable description.
pub fn stop() -> Result<String, Error> {
    platform::stop()
}

/// Describes whether the service is installed and running.
pub fn status() -> Result<String, Error> {
    platform::status()
}
