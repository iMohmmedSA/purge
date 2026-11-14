//! Command-line arguments.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "purge",
    version,
    about = "Reclaim disk space from stale build artifacts"
)]
pub struct Cli {
    /// Use this configuration file instead of the default.
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// More log output (-v, -vv).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show projects and what the policy would do now. Changes nothing.
    Scan {
        /// Scan these folders instead of the configured roots.
        folders: Vec<PathBuf>,
    },
    /// Apply the policy now.
    Run {
        /// Only show what would happen.
        #[arg(long)]
        dry_run: bool,
        /// Defer clean-and-rebuild actions.
        #[arg(long)]
        no_rebuild: bool,
    },
    /// Clean one project now, regardless of the policy.
    Clean {
        /// Project folder.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Only clean this profile (e.g. debug, release) instead of everything.
        #[arg(long)]
        profile: Option<String>,
        /// Build the profile again after cleaning it.
        #[arg(long, requires = "profile")]
        rebuild: bool,
        /// Only remove stale builds (unused for `max_age`), keeping
        /// everything in use, so nothing needs rebuilding.
        #[arg(long, conflicts_with = "rebuild")]
        stale: bool,
        /// Clean even if the project is in use or its build output is shared.
        #[arg(long)]
        force: bool,
    },
    /// Show configuration, service and recent activity.
    Status,
    /// Show how much space purge has freed.
    Stats {
        /// Number of projects to list.
        #[arg(long, default_value_t = 10)]
        top: usize,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show what purge has done.
    History {
        /// Number of entries to show.
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Manage the configuration file.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Run the background loop in the foreground.
    Daemon,
    /// Start purge automatically at login.
    #[command(subcommand)]
    Service(ServiceCommand),
}

impl Command {
    /// Whether the command scans or cleans, and so should run as
    /// background work.
    pub fn is_heavy(&self) -> bool {
        matches!(
            self,
            Self::Scan { .. } | Self::Run { .. } | Self::Clean { .. }
        )
    }
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Write a commented starter configuration.
    Init {
        /// Folders to watch.
        roots: Vec<PathBuf>,
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Print the configuration file's location.
    Path,
    /// Print the configuration file.
    Show,
}

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum ServiceCommand {
    /// Install and start the background service.
    Install,
    /// Stop and remove the background service.
    Uninstall,
    /// Show whether the service is installed and running.
    Status,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn arguments_are_consistent() {
        Cli::command().debug_assert();
    }
}
