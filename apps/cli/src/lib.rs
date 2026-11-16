//! `purge`: reclaim disk space from stale build artifacts.
//!
//! The command-line interface, shared by the `purge` binary and its
//! `purge-rs` alias. The `purged` background daemon ships alongside.

mod args;
mod commands;
mod output;

use std::{
    io::IsTerminal as _,
    process::ExitCode,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::Result;
use args::{Cli, Command};
use clap::Parser;
use purge_config::Paths;
use purge_session::Session;
use tracing_subscriber::EnvFilter;

/// Runs the command line, as `purge` or `purge-rs`.
pub fn main() -> ExitCode {
    let code = match run(Cli::parse()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    };
    // Cut short by Ctrl-C: say so, as shells do for an interrupted command.
    if purge_process::stop_requested() {
        ExitCode::from(INTERRUPTED)
    } else {
        code
    }
}

/// Exit code of a command interrupted by Ctrl-C (128 + SIGINT).
const INTERRUPTED: u8 = 130;

fn run(cli: Cli) -> Result<ExitCode> {
    let mut paths = Paths::discover()?;
    if let Some(config) = cli.config {
        paths.config_file = config;
    }
    if let Command::Daemon = cli.command {
        let _guard = purge_daemon::init_logging(&paths, true)?;
        purge_daemon::run(&paths)?;
        return Ok(ExitCode::SUCCESS);
    }
    init_logging(cli.verbose);
    if cli.command.is_heavy()
        && let Err(err) = purge_process::enter_background_mode()
    {
        tracing::warn!("cannot lower process priority: {err}");
    }
    if cli.command.changes_files() {
        stop_on_ctrl_c();
    }

    let done = |()| ExitCode::SUCCESS;
    // Commands that must work without a valid configuration.
    match cli.command {
        Command::Status => return commands::status(&paths).map(done),
        Command::Config(command) => return commands::config(&paths, command).map(done),
        Command::Service(command) => return commands::service(&paths, command).map(done),
        _ => {}
    }
    let session = Session::open(paths)?;
    match cli.command {
        Command::Scan { folders } => commands::scan(&session, &folders),
        Command::Run { dry_run: true, .. } => commands::scan(&session, &[]),
        Command::Run { no_rebuild, .. } => commands::run(&session, !no_rebuild),
        Command::Clean {
            path,
            profile,
            rebuild,
            stale,
            force,
        } => commands::clean(
            &session,
            &path,
            &commands::CleanRequest {
                profile: profile.as_deref(),
                rebuild,
                stale,
                force,
            },
        ),
        Command::Stats { top, json } => commands::stats(&session, top, json).map(done),
        Command::History { limit } => commands::history(&session, limit).map(done),
        Command::Status | Command::Config(_) | Command::Service(_) | Command::Daemon => {
            unreachable!("handled above")
        }
    }
}

/// The first Ctrl-C lets the step under way finish (a half-deleted
/// `node_modules` is worse than either); the second quits at once.
fn stop_on_ctrl_c() {
    static PRESSED: AtomicBool = AtomicBool::new(false);
    let installed = ctrlc::set_handler(|| {
        if PRESSED.swap(true, Ordering::SeqCst) {
            std::process::exit(INTERRUPTED.into());
        }
        purge_process::request_stop();
        eprintln!("stopping after the current step; press Ctrl-C again to quit now");
    });
    if let Err(err) = installed {
        tracing::warn!("cannot handle Ctrl-C: {err}");
    }
}

fn init_logging(verbose: u8) {
    let level = match verbose {
        // Failed actions are already in the printed report; logging them
        // too would print every compiler error twice.
        0 => "warn,purge_engine::run=error",
        1 => "info",
        _ => "debug",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .without_time()
        .init();
}
