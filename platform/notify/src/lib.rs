//! Desktop notifications on Linux (D-Bus), macOS and Windows.

const APP_NAME: &str = "purge";

#[derive(Debug, thiserror::Error)]
#[error("could not show notification: {0}")]
pub struct Error(#[from] notify_rust::error::Error);

/// Shows a desktop notification.
pub fn send(summary: &str, body: &str) -> Result<(), Error> {
    notify_rust::Notification::new()
        .appname(APP_NAME)
        .summary(summary)
        .body(body)
        .show()
        .map(drop)?;
    Ok(())
}
