//! `purged`: the purge background daemon, as started by the login service.
//! On Windows it is a GUI-subsystem program so no console window appears.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

use purge_config::Paths;
use tracing::error;

fn main() -> ExitCode {
    let paths = match Paths::discover() {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("purged: {err}");
            return ExitCode::FAILURE;
        }
    };
    let _guard = match purge_daemon::init_logging(&paths, cfg!(not(windows))) {
        Ok(guard) => guard,
        Err(err) => {
            eprintln!("purged: {err:#}");
            return ExitCode::FAILURE;
        }
    };
    match purge_daemon::run(&paths) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}
