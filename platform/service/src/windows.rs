use std::{
    os::windows::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};

use crate::{Error, SERVICE_NAME, Spec, command::run};

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn command_line(spec: &Spec) -> String {
    std::iter::once(spec.program.display().to_string())
        .chain(spec.args.iter().cloned())
        .map(|part| format!("\"{part}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

fn image_name(program: &Path) -> String {
    program.file_name().map_or_else(
        || "purged.exe".into(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The command line registered to run at login, if any.
fn registered_command() -> Result<Option<String>, Error> {
    let output = Command::new("reg")
        .args(["query", RUN_KEY, "/v", SERVICE_NAME])
        .output()?;
    if !output.status.success() {
        // `reg query` fails when the value does not exist.
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().find_map(|line| {
        line.split_once("REG_SZ")
            .map(|(_, value)| value.trim().to_owned())
    }))
}

pub(crate) fn install(spec: &Spec) -> Result<String, Error> {
    let line = command_line(spec);
    run(Command::new("reg").args([
        "add",
        RUN_KEY,
        "/v",
        SERVICE_NAME,
        "/t",
        "REG_SZ",
        "/d",
        &line,
        "/f",
    ]))?;
    // Replace a running instance so the new binary takes effect.
    let _ = run(Command::new("taskkill").args(["/IM", &image_name(&spec.program), "/F"]));
    Command::new(&spec.program)
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()?;
    Ok(format!(
        "registered {line} to start at login and started it"
    ))
}

pub(crate) fn uninstall() -> Result<String, Error> {
    let Some(line) = registered_command()? else {
        return Ok("service is not installed".into());
    };
    run(Command::new("reg").args(["delete", RUN_KEY, "/v", SERVICE_NAME, "/f"]))?;
    let program = line.split('"').nth(1).unwrap_or(&line);
    let _ = run(Command::new("taskkill").args(["/IM", &image_name(Path::new(program)), "/F"]));
    Ok("stopped and removed the login entry".into())
}

pub(crate) fn status() -> Result<String, Error> {
    Ok(registered_command()?.map_or_else(
        || "not installed".into(),
        |line| format!("starts at login: {line}"),
    ))
}
