use std::fmt::Write as _;

use purge_engine::{Outcome, RunReport, format_bytes};
use tracing::warn;

/// Most actions listed in a notification body.
const MAX_LINES: usize = 5;

/// Shows a desktop notification for runs that freed space or failed.
pub(crate) fn report(report: &RunReport) {
    let Some((summary, body)) = message(report) else {
        return;
    };
    if let Err(err) = purge_notify::send(&summary, &body) {
        warn!("{err}");
    }
}

/// Summary and body for a notification, or `None` if nothing worth
/// mentioning happened.
fn message(report: &RunReport) -> Option<(String, String)> {
    let freed = report.freed();
    let failures = report.failures();
    if freed == 0 && failures == 0 {
        return None;
    }
    let mut summary = format!("purge freed {}", format_bytes(freed));
    if failures > 0 {
        let _ = write!(summary, ", {failures} failed");
    }
    let lines: Vec<String> = report
        .outcomes()
        .filter_map(|(unit, action)| match &action.outcome {
            Outcome::Done { freed } => Some(format!(
                "{}: {} ({})",
                unit.unit.name(),
                action.action,
                format_bytes(*freed)
            )),
            Outcome::Failed(_) => Some(format!("{}: {} failed", unit.unit.name(), action.action)),
            Outcome::Planned { .. } | Outcome::Deferred(_) => None,
        })
        .chain(
            report
                .caches
                .iter()
                .filter_map(|cache| match cache.outcome.as_ref()? {
                    Outcome::Done { freed } => Some(format!(
                        "{}: {} ({})",
                        cache.cache.name,
                        cache.action,
                        format_bytes(*freed)
                    )),
                    Outcome::Failed(_) => {
                        Some(format!("{}: {} failed", cache.cache.name, cache.action))
                    }
                    Outcome::Planned { .. } | Outcome::Deferred(_) => None,
                }),
        )
        .collect();
    let mut body = lines[..lines.len().min(MAX_LINES)].join("\n");
    if lines.len() > MAX_LINES {
        let _ = write!(body, "\n…and {} more", lines.len() - MAX_LINES);
    }
    Some((summary, body))
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use purge_ecosystem::Project;
    use purge_engine::{Action, ActionOutcome, Reason, Unit, UnitReport};

    use super::*;

    fn report(outcomes: Vec<Outcome>) -> RunReport {
        let unit = Unit::single(Project {
            ecosystem: "rust",
            name: "app".into(),
            root: "/dev/app".into(),
            artifact_dir: "/dev/app/target".into(),
            extra_dirs: Vec::new(),
        });
        let actions = outcomes
            .into_iter()
            .map(|outcome| ActionOutcome {
                action: Action::CleanAll {
                    reason: Reason::Manual,
                },
                outcome,
            })
            .collect();
        RunReport {
            started: SystemTime::UNIX_EPOCH,
            finished: SystemTime::UNIX_EPOCH,
            dry_run: false,
            units: vec![UnitReport {
                actions,
                ..UnitReport::new(unit)
            }],
            caches: Vec::new(),
        }
    }

    #[test]
    fn quiet_when_nothing_happened() {
        assert_eq!(message(&report(vec![])), None);
        assert_eq!(
            message(&report(vec![Outcome::Deferred("busy".into())])),
            None
        );
    }

    #[test]
    fn summarizes_and_truncates() {
        let mut outcomes = vec![Outcome::Failed("boom".into())];
        outcomes.extend((0..6).map(|_| Outcome::Done { freed: 1_000_000 }));
        let (summary, body) = message(&report(outcomes)).unwrap();
        assert_eq!(summary, "purge freed 6.0 MB, 1 failed");
        assert_eq!(body.lines().count(), MAX_LINES + 1);
        assert!(body.starts_with("app: clean everything failed"));
        assert!(body.ends_with("…and 2 more"));
    }
}
