//! The background loop: waits until a run is due and the machine is idle,
//! runs purge, and notifies.

mod logging;
mod notify;
mod schedule;

use std::{
    sync::mpsc::{self, RecvTimeoutError},
    time::{Duration, SystemTime},
};

use anyhow::Result;
use purge_config::{Paths, Schedule};
use purge_engine::{format_age, format_bytes};
use purge_session::Session;
use tracing::{debug, error, info, warn};

pub use logging::init_logging;

/// Runs until interrupted (Ctrl-C, SIGTERM, or the service stopping). A
/// run under way stops at its next safe point first: it finishes the
/// deletion in progress, stops a running build, and leaves the rest.
pub fn run(paths: &Paths) -> Result<()> {
    let (stop_tx, stop) = mpsc::channel();
    let handler_tx = stop_tx.clone();
    if let Err(err) = ctrlc::set_handler(move || {
        purge_process::request_stop();
        let _ = handler_tx.send(());
    }) {
        // Still worth running; it just cannot shut down gracefully.
        warn!("cannot install signal handler: {err}");
    }
    if let Err(err) = purge_process::enter_background_mode() {
        warn!("cannot lower process priority: {err}");
    }

    // A broken configuration must not stop the daemon: it starts on the
    // default schedule, and the loop below keeps retrying until it is fixed.
    let mut schedule =
        Session::open(paths.clone()).map_or_default(|session| session.config().schedule.clone());
    info!(config = %paths.config_file.display(), "purge daemon started");

    let mut delay = schedule.startup_delay;
    while !stopped(&stop, delay) {
        delay = match Session::open(paths.clone()) {
            Ok(session) => {
                schedule = session.config().schedule.clone();
                tick(&session).unwrap_or_else(|err| {
                    error!("run failed: {err:#}");
                    schedule.retry_after
                })
            }
            Err(err) => {
                warn!("cannot load configuration, retrying later: {err:#}");
                schedule.retry_after
            }
        };
        debug!("next check in {}", format_age(delay));
    }
    drop(stop_tx);
    info!("purge daemon stopped");
    Ok(())
}

/// Waits up to `delay`; true if a stop was requested meanwhile.
fn stopped(stop: &mpsc::Receiver<()>, delay: Duration) -> bool {
    !matches!(stop.recv_timeout(delay), Err(RecvTimeoutError::Timeout))
}

/// Runs if due and allowed; returns how long to wait before the next check.
/// Never waits longer than `retry_after`, so configuration changes are
/// picked up within that time.
fn tick(session: &Session) -> Result<Duration> {
    let config = session.config();
    let schedule = &config.schedule;
    if config.roots.is_empty() {
        warn!("no roots configured; run `purge config init <folder>` to choose what to watch");
        return Ok(schedule.retry_after);
    }
    let last_run = session.load_state()?.last_run;
    if let Some(wait) = schedule::wait_before_run(last_run, SystemTime::now(), schedule.interval) {
        return Ok(wait.min(schedule.retry_after));
    }
    if let Some(reason) = postpone_reason(schedule) {
        info!("{reason}, postponing");
        return Ok(schedule.retry_after);
    }

    let Some(report) = session.run(true)? else {
        info!("another purge run is in progress, postponing");
        return Ok(schedule.retry_after);
    };
    info!(
        units = report.units.len(),
        freed = %format_bytes(report.freed()),
        failures = report.failures(),
        "run finished"
    );
    if config.notify.enabled {
        notify::report(&report);
    }
    Ok(schedule.interval.min(schedule.retry_after))
}

/// Why a due run should wait: outside the time window, or the machine busy.
fn postpone_reason(schedule: &Schedule) -> Option<String> {
    if let Some(window) = schedule.window
        && !window.contains(jiff::Zoned::now().time())
    {
        return Some(format!("outside the run window {window}"));
    }
    if schedule.max_cpu_percent >= 100.0 {
        return None;
    }
    match purge_process::cpu_usage() {
        Ok(cpu) if cpu > schedule.max_cpu_percent => Some(format!("system busy ({cpu:.0}% CPU)")),
        Ok(_) => None,
        Err(err) => {
            warn!("cannot measure CPU usage, running anyway: {err}");
            None
        }
    }
}
