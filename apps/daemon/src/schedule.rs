use std::time::{Duration, SystemTime};

/// How long until a run is due, or `None` if one is due now.
pub(crate) fn wait_before_run(
    last_run: Option<SystemTime>,
    now: SystemTime,
    interval: Duration,
) -> Option<Duration> {
    // A last run in the future means the clock moved back: run now.
    let since = now.duration_since(last_run?).ok()?;
    interval.checked_sub(since).filter(|wait| !wait.is_zero())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_hours(1);

    fn at(hours: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_hours(hours)
    }

    #[test]
    fn due_when_never_run_or_interval_passed() {
        assert_eq!(wait_before_run(None, at(10), 6 * HOUR), None);
        assert_eq!(wait_before_run(Some(at(4)), at(10), 6 * HOUR), None);
        assert_eq!(wait_before_run(Some(at(1)), at(10), 6 * HOUR), None);
    }

    #[test]
    fn waits_for_the_rest_of_the_interval() {
        assert_eq!(
            wait_before_run(Some(at(8)), at(10), 6 * HOUR),
            Some(4 * HOUR)
        );
    }

    #[test]
    fn clock_moved_back_runs_now() {
        assert_eq!(wait_before_run(Some(at(12)), at(10), 6 * HOUR), None);
    }
}
