use std::{fmt, time::Duration};

use jiff::civil::Time;
use serde::Deserialize;

/// When the daemon runs.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Schedule {
    /// Time between two runs.
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    /// Wait this long after the daemon starts, so login stays snappy.
    #[serde(with = "humantime_serde")]
    pub startup_delay: Duration,
    /// When a run is due but postponed (outside the window, machine busy),
    /// check again after this long.
    #[serde(with = "humantime_serde")]
    pub retry_after: Duration,
    /// Only run inside this local time window, e.g. `"22:00-06:00"`.
    pub window: Option<TimeWindow>,
    /// Postpone runs while overall CPU usage is above this percentage.
    /// `100` disables the check.
    pub max_cpu_percent: f32,
    /// At most this many rebuilds per run; rebuilds are the expensive part.
    pub max_rebuilds: usize,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            interval: Duration::from_hours(6),
            startup_delay: Duration::from_mins(2),
            retry_after: Duration::from_mins(30),
            window: None,
            max_cpu_percent: 30.0,
            max_rebuilds: 2,
        }
    }
}

impl Schedule {
    /// Rejects settings that would make the daemon spin or never run.
    pub(crate) fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("interval", self.interval),
            ("retry_after", self.retry_after),
        ] {
            if value < MIN_WAIT {
                return Err(format!("schedule.{name} must be at least 1m"));
            }
        }
        if !(self.max_cpu_percent > 0.0 && self.max_cpu_percent <= 100.0) {
            return Err(format!(
                "schedule.max_cpu_percent must be between 1 and 100, got {}",
                self.max_cpu_percent
            ));
        }
        Ok(())
    }
}

/// Shortest allowed wait between two checks.
const MIN_WAIT: Duration = Duration::from_mins(1);

/// A daily local time range; it may wrap past midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct TimeWindow {
    pub start: Time,
    pub end: Time,
}

impl TimeWindow {
    pub fn contains(&self, time: Time) -> bool {
        if self.start <= self.end {
            self.start <= time && time < self.end
        } else {
            time >= self.start || time < self.end
        }
    }
}

impl TryFrom<String> for TimeWindow {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let invalid = || format!("invalid time window `{text}`, expected e.g. \"22:00-06:00\"");
        let (start, end) = text.split_once('-').ok_or_else(invalid)?;
        let parse = |part: &str| part.trim().parse::<Time>().map_err(|_| invalid());
        Ok(Self {
            start: parse(start)?,
            end: parse(end)?,
        })
    }
}

impl fmt::Display for TimeWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}-{}",
            self.start.strftime("%H:%M"),
            self.end.strftime("%H:%M")
        )
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::time;

    use super::*;

    fn window(text: &str) -> TimeWindow {
        TimeWindow::try_from(text.to_owned()).unwrap()
    }

    #[test]
    fn same_day_window() {
        let w = window("09:00-17:30");
        assert!(w.contains(time(9, 0, 0, 0)));
        assert!(w.contains(time(17, 29, 0, 0)));
        assert!(!w.contains(time(17, 30, 0, 0)));
        assert!(!w.contains(time(8, 59, 0, 0)));
    }

    #[test]
    fn window_across_midnight() {
        let w = window("22:00 - 06:00");
        assert!(w.contains(time(23, 0, 0, 0)));
        assert!(w.contains(time(2, 0, 0, 0)));
        assert!(!w.contains(time(12, 0, 0, 0)));
        assert_eq!(w.to_string(), "22:00-06:00");
    }

    #[test]
    fn rejects_settings_that_spin_or_never_run() {
        assert!(Schedule::default().validate().is_ok());
        let zero = Schedule {
            interval: Duration::ZERO,
            ..Schedule::default()
        };
        assert!(zero.validate().is_err());
        let never = Schedule {
            max_cpu_percent: 0.0,
            ..Schedule::default()
        };
        assert!(never.validate().is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(TimeWindow::try_from("tonight".to_owned()).is_err());
        assert!(TimeWindow::try_from("25:00-01:00".to_owned()).is_err());
    }
}
