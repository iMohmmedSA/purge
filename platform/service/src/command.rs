use std::process::{Command, Output};

use crate::Error;

/// Runs `cmd`, failing with its stderr if it exits unsuccessfully.
pub(crate) fn run(cmd: &mut Command) -> Result<Output, Error> {
    let output = cmd.output()?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(Error::Command {
            command: describe(cmd),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn describe(cmd: &Command) -> String {
    std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}
