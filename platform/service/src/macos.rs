use std::{fmt::Write as _, fs, path::PathBuf, process::Command};

use directories::BaseDirs;

use crate::{Error, Spec, command::run};

const LABEL: &str = "dev.purge.daemon";

fn plist_path() -> Result<PathBuf, Error> {
    let dirs = BaseDirs::new().ok_or(Error::NoHome)?;
    Ok(dirs
        .home_dir()
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn domain() -> Result<String, Error> {
    let output = run(Command::new("id").arg("-u"))?;
    Ok(format!(
        "gui/{}",
        String::from_utf8_lossy(&output.stdout).trim()
    ))
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn plist(spec: &Spec) -> String {
    let mut args = format!(
        "<string>{}</string>",
        escape(&spec.program.to_string_lossy())
    );
    for arg in &spec.args {
        let _ = write!(args, "<string>{}</string>", escape(arg));
    }
    let env = spec.path_env.as_ref().map_or_else(String::new, |path| {
        format!(
            "<key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict>",
            escape(&path.to_string_lossy())
        )
    });
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array>{args}</array>
  {env}
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Background</string>
  <key>ExitTimeOut</key><integer>300</integer>
  <key>LowPriorityIO</key><true/>
  <key>Nice</key><integer>10</integer>
</dict>
</plist>
"#
    )
}

pub(crate) fn install(spec: &Spec) -> Result<String, Error> {
    let path = plist_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let domain = domain()?;
    // Unload any previous version first; failure just means it was not loaded.
    let _ = run(Command::new("launchctl").args(["bootout", &format!("{domain}/{LABEL}")]));
    fs::write(&path, plist(spec))?;
    run(Command::new("launchctl")
        .arg("bootstrap")
        .arg(&domain)
        .arg(&path))?;
    Ok(format!(
        "installed and started launchd agent {}",
        path.display()
    ))
}

pub(crate) fn uninstall() -> Result<String, Error> {
    let path = plist_path()?;
    if !path.exists() {
        return Ok("service is not installed".into());
    }
    let _ = run(Command::new("launchctl").args(["bootout", &format!("{}/{LABEL}", domain()?)]));
    fs::remove_file(&path)?;
    Ok(format!("stopped and removed {}", path.display()))
}

pub(crate) fn stop() -> Result<String, Error> {
    if !plist_path()?.exists() {
        return Ok("service is not installed".into());
    }
    // Failure just means it was not loaded.
    let _ = run(Command::new("launchctl").args(["bootout", &format!("{}/{LABEL}", domain()?)]));
    Ok("stopped the launchd agent".into())
}

pub(crate) fn status() -> Result<String, Error> {
    let path = plist_path()?;
    if !path.exists() {
        return Ok("not installed".into());
    }
    let loaded = Command::new("launchctl")
        .args(["print", &format!("{}/{LABEL}", domain()?)])
        .output()?
        .status
        .success();
    let state = if loaded { "loaded" } else { "not loaded" };
    Ok(format!("launchd agent {state} ({})", path.display()))
}
