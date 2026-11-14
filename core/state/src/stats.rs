//! Summaries of the action history.

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    time::{Duration, SystemTime},
};

use jiff::{Timestamp, tz::TimeZone};
use serde::Serialize;

use crate::HistoryEntry;

const DAY: Duration = Duration::from_hours(24);

/// Recent periods reported by [`Stats::compute`].
const PERIODS: &[(&str, Duration)] = &[
    ("last 24 hours", DAY),
    ("last 7 days", Duration::from_hours(7 * 24)),
    ("last 30 days", Duration::from_hours(30 * 24)),
];

/// Space freed and actions taken.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    pub freed_bytes: u64,
    pub actions: usize,
}

impl Totals {
    fn add(&mut self, entry: &HistoryEntry) {
        self.freed_bytes += entry.freed_bytes;
        self.actions += 1;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Period {
    pub label: &'static str,
    pub totals: Totals,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectTotals {
    pub project: PathBuf,
    pub totals: Totals,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    #[serde(with = "humantime_serde")]
    pub at: SystemTime,
    pub project: PathBuf,
    pub error: String,
}

/// What purge has achieved, from its history. Failed actions are counted
/// separately and never in the totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub all_time: Totals,
    /// When the history starts.
    #[serde(with = "humantime_serde")]
    pub since: Option<SystemTime>,
    pub periods: Vec<Period>,
    /// Keyed by local `YYYY-MM`, oldest first.
    pub by_month: BTreeMap<String, Totals>,
    /// Keyed by action kind (`clean-all`, `clean`, `refresh`).
    pub by_action: BTreeMap<String, Totals>,
    /// Projects that gained the most, largest first.
    pub top_projects: Vec<ProjectTotals>,
    pub failures: usize,
    pub last_failure: Option<Failure>,
}

impl Stats {
    /// Summarizes `entries`, listing at most `top` projects.
    pub fn compute(entries: &[HistoryEntry], now: SystemTime, top: usize) -> Self {
        let mut all_time = Totals::default();
        let mut periods: Vec<Period> = PERIODS
            .iter()
            .map(|&(label, _)| Period {
                label,
                totals: Totals::default(),
            })
            .collect();
        let mut by_month: BTreeMap<String, Totals> = BTreeMap::new();
        let mut by_action: BTreeMap<String, Totals> = BTreeMap::new();
        let mut by_project: HashMap<&PathBuf, Totals> = HashMap::new();
        let mut failures = 0;
        let mut last_failure: Option<&HistoryEntry> = None;
        let tz = TimeZone::system();

        for entry in entries {
            if entry.error.is_some() {
                failures += 1;
                if last_failure.is_none_or(|last| entry.at >= last.at) {
                    last_failure = Some(entry);
                }
                continue;
            }
            all_time.add(entry);
            let age = now.duration_since(entry.at).unwrap_or_default();
            for (period, &(_, length)) in periods.iter_mut().zip(PERIODS) {
                if age < length {
                    period.totals.add(entry);
                }
            }
            if let Ok(at) = Timestamp::try_from(entry.at) {
                let month = at.to_zoned(tz.clone()).strftime("%Y-%m").to_string();
                by_month.entry(month).or_default().add(entry);
            }
            by_action
                .entry(entry.action.clone())
                .or_default()
                .add(entry);
            by_project.entry(&entry.project).or_default().add(entry);
        }

        let mut top_projects: Vec<ProjectTotals> = by_project
            .into_iter()
            .map(|(project, totals)| ProjectTotals {
                project: project.clone(),
                totals,
            })
            .collect();
        top_projects.sort_by(|a, b| {
            b.totals
                .freed_bytes
                .cmp(&a.totals.freed_bytes)
                .then_with(|| a.project.cmp(&b.project))
        });
        top_projects.truncate(top);

        Self {
            all_time,
            since: entries.iter().map(|e| e.at).min(),
            periods,
            by_month,
            by_action,
            top_projects,
            failures,
            last_failure: last_failure.map(|entry| Failure {
                at: entry.at,
                project: entry.project.clone(),
                error: entry.error.clone().unwrap_or_default(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + 1000 * DAY
    }

    fn entry(project: &str, action: &str, days_ago: u32, mb: u64) -> HistoryEntry {
        HistoryEntry {
            at: now() - days_ago * DAY,
            project: project.into(),
            ecosystem: "rust".into(),
            action: action.into(),
            group: None,
            reason: String::new(),
            freed_bytes: mb * 1_000_000,
            error: None,
        }
    }

    #[test]
    fn summarizes_by_period_action_and_project() {
        let mut failed = entry("/dev/c", "refresh", 0, 0);
        failed.error = Some("build failed".into());
        let entries = [
            entry("/dev/a", "clean-all", 40, 500),
            entry("/dev/b", "clean", 10, 300),
            entry("/dev/a", "clean", 3, 100),
            entry("/dev/b", "refresh", 0, 50),
            failed,
        ];
        let stats = Stats::compute(&entries, now(), 1);

        let mb = |n: u64| n * 1_000_000;
        assert_eq!(
            stats.all_time,
            Totals {
                freed_bytes: mb(950),
                actions: 4
            }
        );
        let periods: Vec<u64> = stats.periods.iter().map(|p| p.totals.freed_bytes).collect();
        assert_eq!(periods, [mb(50), mb(150), mb(450)]);
        assert_eq!(
            stats.by_action["clean"],
            Totals {
                freed_bytes: mb(400),
                actions: 2
            }
        );
        assert_eq!(stats.by_month.values().map(|t| t.actions).sum::<usize>(), 4);
        assert_eq!(stats.top_projects.len(), 1);
        assert_eq!(stats.top_projects[0].project, PathBuf::from("/dev/a"));
        assert_eq!(stats.failures, 1);
        assert_eq!(stats.last_failure.as_ref().unwrap().error, "build failed");
        assert_eq!(stats.since, Some(now() - 40 * DAY));
    }

    #[test]
    fn empty_history() {
        let stats = Stats::compute(&[], now(), 10);
        assert_eq!(stats.all_time, Totals::default());
        assert_eq!(stats.since, None);
        assert!(stats.top_projects.is_empty());
    }
}
