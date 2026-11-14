//! `purge`: reclaim disk space from stale build artifacts.
//!
//! The command-line interface, shared by the `purge` binary and its
//! `purge-rs` alias. The `purged` background daemon ships alongside.

mod args;
mod commands;
mod output;

use std::{io::IsTerminal as _, process::ExitCode};

use anyhow::Result;
use args::{Cli, Command};
use clap::Parser;
use purge_config::Paths;
use purge_session::Session;
use tracing_subscriber::EnvFilter;

/// Runs the command line, as `purge` or `purge-rs`.
pub fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

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
