//! Running external tools with useful errors and a time limit.

use std::{
    io::Read,
    process::{Child, Command, Output, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{Error, Result};

/// How many trailing lines of stderr an error keeps.
const STDERR_LINES: usize = 20;

/// Longest pause between two checks on a running command. Checks start
/// fast so quick commands return quickly.
const MAX_POLL: Duration = Duration::from_millis(250);

/// Runs `cmd` to completion, failing with its stderr if it exits
/// unsuccessfully. A command still running after `timeout` is killed, so a
/// hung tool can never stall purge.
pub fn run(cmd: &mut Command, timeout: Duration) -> Result<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Read both pipes while waiting, so a chatty tool never blocks on a full one.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let Some(status) = wait(&mut child, timeout)? else {
        // Leave the readers behind: the tool's own children may keep the
        // pipes open for a while after it is killed.
        return Err(Error::Timeout {
            command: describe(cmd),
            after: timeout,
        });
    };
    let output = Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    };
    if output.status.success() {
        return Ok(output);
    }
    Err(Error::Command {
        command: describe(cmd),
        status: output.status.to_string(),
        stderr: tail(&String::from_utf8_lossy(&output.stderr), STDERR_LINES),
    })
}

/// Waits for `child` to exit, killing it after `timeout` (then `None`).
fn wait(child: &mut Child, timeout: Duration) -> Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + timeout;
    let mut pause = Duration::from_millis(1);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = Instant::now();
        if now >= deadline {
            child.kill()?;
            child.wait()?;
            return Ok(None);
        }
        thread::sleep(pause.min(deadline - now));
        pause = (pause * 2).min(MAX_POLL);
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> Option<JoinHandle<Vec<u8>>> {
    pipe.map(|mut pipe| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            // A read error only loses output, never the result.
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    })
}

fn collect(reader: Option<JoinHandle<Vec<u8>>>) -> Vec<u8> {
    reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default()
}

/// Renders a command line for messages.
fn describe(cmd: &Command) -> String {
    let program = cmd.get_program().to_string_lossy();
    let program = std::path::Path::new(program.as_ref())
        .file_name()
        .map_or(program.clone(), |name| name.to_string_lossy());
    std::iter::once(program)
        .chain(cmd.get_args().map(|arg| arg.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.trim_end().lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_last_lines() {
        assert_eq!(tail("a\nb\nc\n", 2), "b\nc");
        assert_eq!(tail("", 2), "");
    }

    #[test]
    fn describes_with_program_file_name() {
        let mut cmd = Command::new("/usr/bin/cargo");
        cmd.args(["clean", "--release"]);
        assert_eq!(describe(&cmd), "cargo clean --release");
    }

    #[cfg(unix)]
    #[test]
    fn captures_output_and_failures() {
        let long = Duration::from_secs(30);
        let output = run(
            Command::new("sh").args(["-c", "echo out; echo err >&2"]),
            long,
        )
        .unwrap();
        assert_eq!(output.stdout, b"out\n");
        assert_eq!(output.stderr, b"err\n");

        let err = run(
            Command::new("sh").args(["-c", "echo boom >&2; exit 3"]),
            long,
        )
        .unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn kills_commands_that_overrun() {
        let started = Instant::now();
        let err = run(Command::new("sleep").arg("30"), Duration::from_millis(200)).unwrap_err();
        assert!(matches!(err, Error::Timeout { .. }), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
