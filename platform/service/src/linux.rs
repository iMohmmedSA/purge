use std::{fmt::Write as _, fs, path::PathBuf, process::Command};

use directories::BaseDirs;

use crate::{Error, SERVICE_NAME, Spec, command::run};

fn unit_name() -> String {
    format!("{SERVICE_NAME}.service")
}

fn unit_path() -> Result<PathBuf, Error> {
    let dirs = BaseDirs::new().ok_or(Error::NoHome)?;
    Ok(dirs.config_dir().join("systemd/user").join(unit_name()))
}

fn systemctl(args: &[&str]) -> Command {
    let mut cmd = Command::new("systemctl");
    cmd.arg("--user").args(args);
    cmd
}

/// Quotes a value for systemd and escapes its specifier character.
fn quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

fn unit(spec: &Spec) -> String {
    let mut exec = quote(&spec.program.to_string_lossy());
    for arg in &spec.args {
        exec.push(' ');
        exec.push_str(&quote(arg));
    }
    let mut unit = String::new();
    let _ = writeln!(unit, "[Unit]");
    let _ = writeln!(
        unit,
        "Description=purge: reclaim disk space from stale build artifacts"
    );
    let _ = writeln!(unit);
    let _ = writeln!(unit, "[Service]");
    let _ = writeln!(unit, "Type=simple");
    let _ = writeln!(unit, "ExecStart={exec}");
    if let Some(path) = &spec.path_env {
        let _ = writeln!(
            unit,
            "Environment={}",
            quote(&format!("PATH={}", path.to_string_lossy()))
        );
    }
    let _ = writeln!(unit, "Restart=on-failure");
    let _ = writeln!(unit, "RestartSec=60");
    let _ = writeln!(unit, "Nice=10");
    let _ = writeln!(unit);
    let _ = writeln!(unit, "[Install]");
    let _ = writeln!(unit, "WantedBy=default.target");
    unit
}

pub(crate) fn install(spec: &Spec) -> Result<String, Error> {
    let path = unit_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, unit(spec))?;
    run(&mut systemctl(&["daemon-reload"]))?;
    run(&mut systemctl(&["enable", "--now", &unit_name()]))?;
    // Pick up a changed binary or unit when reinstalling.
    run(&mut systemctl(&["restart", &unit_name()]))?;
    Ok(format!(
        "installed and started systemd user unit {}",
        path.display()
    ))
}

pub(crate) fn uninstall() -> Result<String, Error> {
    let path = unit_path()?;
    if !path.exists() {
        return Ok("service is not installed".into());
    }
    // Disabling fails if the unit was already stopped; removal matters more.
    let _ = run(&mut systemctl(&["disable", "--now", &unit_name()]));
    fs::remove_file(&path)?;
    run(&mut systemctl(&["daemon-reload"]))?;
    Ok(format!("stopped and removed {}", path.display()))
}

pub(crate) fn status() -> Result<String, Error> {
    let path = unit_path()?;
    if !path.exists() {
        return Ok("not installed".into());
    }
    let output = systemctl(&["is-active", &unit_name()]).output()?;
    let state = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(format!("systemd user unit {state} ({})", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_quotes_arguments_and_path() {
        let unit = unit(&Spec {
            program: "/home/me/.cargo/bin/purged".into(),
            args: vec!["--x".into()],
            path_env: Some("/usr/bin:/home/me/50%".into()),
        });
        assert!(unit.contains("ExecStart=\"/home/me/.cargo/bin/purged\" \"--x\""));
        assert!(unit.contains("Environment=\"PATH=/usr/bin:/home/me/50%%\""));
    }
}
