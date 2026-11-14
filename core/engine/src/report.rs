use std::time::{Duration, SystemTime};

use bytesize::ByteSize;
use purge_ecosystem::Snapshot;
use purge_state::{Footprint, HistoryEntry};

use crate::{Action, Unit};

/// Everything one run found and did.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub started: SystemTime,
    pub finished: SystemTime,
    pub dry_run: bool,
    pub units: Vec<UnitReport>,
}

#[derive(Debug, Clone)]
pub struct UnitReport {
    pub unit: Unit,
    pub snapshot: Option<Snapshot>,
    /// Last source change or real build.
    pub activity: Option<SystemTime>,
    pub status: UnitStatus,
    pub actions: Vec<ActionOutcome>,
}

impl UnitReport {
    /// A report for a unit that has not been looked at yet.
    pub fn new(unit: Unit) -> Self {
        Self {
            unit,
            snapshot: None,
            activity: None,
            status: UnitStatus::Checked,
            actions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitStatus {
    /// Inspected; `actions` says what happened.
    Checked,
    /// Excluded by configuration.
    Skipped,
    /// Something is working in a member project; planned actions were not taken.
    Busy(String),
    /// Could not be inspected.
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct ActionOutcome {
    pub action: Action,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Dry run: would free about `estimate` bytes, if known.
    Planned {
        estimate: Option<u64>,
    },
    Done {
        freed: u64,
    },
    /// Not done this time, for the given reason.
    Deferred(String),
    Failed(String),
}

impl RunReport {
    pub fn outcomes(&self) -> impl Iterator<Item = (&UnitReport, &ActionOutcome)> {
        self.units
            .iter()
            .flat_map(|unit| unit.actions.iter().map(move |action| (unit, action)))
    }

    /// Bytes freed, or for a dry run, the known part of what would be freed.
    pub fn freed(&self) -> u64 {
        self.outcomes()
            .map(|(_, action)| match action.outcome {
                Outcome::Done { freed }
                | Outcome::Planned {
                    estimate: Some(freed),
                } => freed,
                _ => 0,
            })
            .sum()
    }

    pub fn failures(&self) -> usize {
        let actions = self
            .outcomes()
            .filter(|(_, action)| matches!(action.outcome, Outcome::Failed(_)))
            .count();
        let units = self
            .units
            .iter()
            .filter(|u| matches!(u.status, UnitStatus::Failed(_)))
            .count();
        actions + units
    }

    /// Units in which something was (or would be) done.
    pub fn touched(&self) -> usize {
        self.units
            .iter()
            .filter(|p| {
                p.actions
                    .iter()
                    .any(|a| matches!(a.outcome, Outcome::Done { .. } | Outcome::Planned { .. }))
            })
            .count()
    }

    /// Build output left once the run's actions are done.
    pub fn footprint(&self) -> Footprint {
        let measured: u64 = self
            .units
            .iter()
            .filter_map(|u| u.snapshot.as_ref())
            .map(|s| s.artifacts.total_bytes)
            .sum();
        Footprint {
            projects: self.units.iter().map(|u| u.unit.members().len()).sum(),
            bytes: measured.saturating_sub(self.freed()),
        }
    }

    /// Entries for actions that were actually attempted.
    pub fn history(&self) -> Vec<HistoryEntry> {
        self.outcomes()
            .filter_map(|(unit, action)| {
                let (freed_bytes, error) = match &action.outcome {
                    Outcome::Done { freed } => (*freed, None),
                    Outcome::Failed(err) => (0, Some(err.clone())),
                    Outcome::Planned { .. } | Outcome::Deferred(_) => return None,
                };
                Some(HistoryEntry {
                    at: self.finished,
                    project: unit.unit.key().to_path_buf(),
                    ecosystem: unit.unit.ecosystem().to_owned(),
                    action: action.action.kind().to_owned(),
                    group: action.action.group().map(str::to_owned),
                    reason: action.action.reason().to_string(),
                    freed_bytes,
                    error,
                })
            })
            .collect()
    }
}

/// `1.2 GB`-style sizes.
pub fn format_bytes(bytes: u64) -> String {
    ByteSize(bytes).display().si().to_string()
}

/// Compact ages in the largest whole unit: `3d`, `5h`, `12m`, `<1m`.
pub fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    match secs {
        0..60 => "<1m".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_ages() {
        assert_eq!(format_age(Duration::from_secs(5)), "<1m");
        assert_eq!(format_age(Duration::from_secs(600)), "10m");
        assert_eq!(format_age(Duration::from_secs(7200)), "2h");
        assert_eq!(format_age(Duration::from_secs(15 * 86_400 + 5)), "15d");
    }

    #[test]
    fn formats_bytes() {
        assert_eq!(format_bytes(1_500_000_000), "1.5 GB");
    }
}
