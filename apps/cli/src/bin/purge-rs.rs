//! `purge-rs`: the same as `purge`, under a name no other tool uses. On
//! macOS a built-in `purge` command may come first on the `PATH`.

use std::process::ExitCode;

fn main() -> ExitCode {
    purge_cli::main()
}
