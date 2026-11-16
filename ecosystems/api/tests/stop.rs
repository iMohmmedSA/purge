//! A command running when purge is asked to stop is stopped too. In a test
//! binary of its own, since the stop request is process-wide.

#![cfg(unix)]

use std::{
    process::Command,
    thread,
    time::{Duration, Instant},
};

use purge_ecosystem::{Error, command};

#[test]
fn running_commands_stop_with_purge() {
    let started = Instant::now();
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(300));
        purge_process::request_stop();
    });
    let err = command::run(Command::new("sleep").arg("60"), Duration::from_secs(120)).unwrap_err();
    assert!(matches!(err, Error::Stopped { .. }), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
}
