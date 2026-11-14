//! `purge`: reclaim disk space from stale build artifacts.

use std::process::ExitCode;

fn main() -> ExitCode {
    purge_cli::main()
}
