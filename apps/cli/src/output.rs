//! Terminal rendering of reports and history.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::SystemTime,
};

use jiff::{Timestamp, tz::TimeZone};
use purge_engine::{
    CacheReport, Outcome, RunReport, UnitReport, UnitStatus, format_age, format_bytes,
};
use purge_state::{Footprint, HistoryEntry, Stats, Totals};

/// Width of the bars in `purge stats`.
const BAR_WIDTH: usize = 24;

pub fn print_report(report: &RunReport) {
    if report.units.is_empty() {
        println!("no projects with build output found");
    }
    let width = report
        .units
        .iter()
        .map(|u| display_path(u.unit.key()).chars().count())
        .max()
        .unwrap_or(0);
    for unit in &report.units {
        print_unit(unit, width);
    }
    print_caches(&report.caches);
    if report.units.is_empty() && report.caches.is_empty() {
        return;
    }

    println!();
    let projects = count(
        report.units.iter().map(|u| u.unit.members().len()).sum(),
        "project",
    );
    let touched = report.touched();
    let freed = format_bytes(report.freed());
    if report.dry_run {
        let unknown = report
            .outcomes()
            .filter(|(_, a)| a.outcome == Outcome::Planned { estimate: None })
            .count();
        let unknown = match unknown {
            0 => String::new(),
            n => format!(" plus {n} rebuild(s) of unknown benefit"),
        };
        println!(
            "{projects}, {touched} with work to do, about {freed} to reclaim{unknown} (dry run)"
        );
    } else {
        println!("{projects}, {touched} cleaned, {freed} freed");
    }
    let failures = report.failures();
    if failures > 0 {
        println!("{failures} failed; see above");
    }
}

fn print_caches(caches: &[CacheReport]) {
    if caches.is_empty() {
        return;
    }
    println!("\nShared caches");
    let width = caches
        .iter()
        .map(|c| display_path(&c.cache.path).chars().count())
        .max()
        .unwrap_or(0);
    for report in caches {
        let cache = &report.cache;
        let what = match &report.outcome {
            None => report.note.clone(),
            Some(outcome) => {
                let result = match outcome {
                    Outcome::Planned {
                        estimate: Some(bytes),
                    } => format!("would free {}", format_bytes(*bytes)),
                    Outcome::Planned { estimate: None } => "would free an unknown amount".into(),
                    Outcome::Done { freed } => format!("freed {}", format_bytes(*freed)),
                    Outcome::Deferred(why) => format!("deferred: {why}"),
                    Outcome::Failed(err) => format!("FAILED: {err}"),
                };
                format!("{}: {} -> {result}", report.action, report.note)
            }
        };
        println!(
            "  {:<width$}  {:<10}  {:>9}  {what}",
            display_path(&cache.path),
            cache.name,
            format_bytes(cache.total_bytes),
        );
    }
}

fn print_unit(report: &UnitReport, width: usize) {
    let path = display_path(report.unit.key());
    let size = report
        .snapshot
        .as_ref()
        .map_or_else(String::new, |s| format_bytes(s.artifacts.total_bytes));
    let state = match &report.status {
        UnitStatus::Checked => report.activity.map_or_else(
            || "no activity recorded".to_owned(),
            |at| format!("active {} ago", format_age(age(at))),
        ),
        UnitStatus::Skipped => "skipped by config".to_owned(),
        UnitStatus::Busy(reason) => format!("busy: {reason}"),
        UnitStatus::Failed(err) => format!("error: {err}"),
    };
    println!(
        "{path:<width$}  {:<5}  {size:>9}  {state}",
        report.unit.ecosystem()
    );
    if report.unit.is_shared() {
        let users: Vec<String> = report
            .unit
            .members()
            .iter()
            .map(|project| display_path(&project.root))
            .collect();
        println!("    shared target, used by {}", users.join(", "));
    }
    for action in &report.actions {
        let result = match &action.outcome {
            Outcome::Planned {
                estimate: Some(bytes),
            } => format!("would free {}", format_bytes(*bytes)),
            Outcome::Planned { estimate: None } => "would free an unknown amount".to_owned(),
            Outcome::Done { freed } => format!("freed {}", format_bytes(*freed)),
            Outcome::Deferred(why) => format!("deferred: {why}"),
            Outcome::Failed(err) => format!("FAILED: {err}"),
        };
        println!(
            "    {}: {} -> {result}",
            action.action,
            action.action.reason()
        );
    }
}

pub fn print_history(entries: &[HistoryEntry]) {
    for entry in entries {
        let what = match &entry.group {
            Some(group) => format!("{} {group}", entry.action),
            None => entry.action.clone(),
        };
        let result = match &entry.error {
            Some(err) => format!("FAILED: {}", err.lines().next().unwrap_or_default()),
            None => format_bytes(entry.freed_bytes),
        };
        println!(
            "{}  {:<16}  {:>9}  {}  ({})",
            local_time(entry.at),
            what,
            result,
            display_path(&entry.project),
            entry.reason
        );
    }
}

pub fn print_stats(stats: &Stats, footprint: Option<Footprint>, last_run: Option<SystemTime>) {
    if stats.all_time.actions == 0 {
        println!("nothing freed yet");
    } else {
        println!("Space freed");
        for period in &stats.periods {
            print_totals(period.label, stats_width(), period.totals);
        }
        print_totals("all time", stats_width(), stats.all_time);
        if let Some(since) = stats.since {
            println!("  (history since {})", local_time(since));
        }

        println!("\nBy month");
        let largest = stats.by_month.values().map(|t| t.freed_bytes).max();
        for (month, totals) in &stats.by_month {
            println!(
                "  {month}  {:>9}  {}",
                format_bytes(totals.freed_bytes),
                bar(totals.freed_bytes, largest.unwrap_or_default())
            );
        }

        println!("\nBy action");
        for (action, totals) in &stats.by_action {
            print_totals(action_label(action), stats_width(), *totals);
        }

        println!("\nTop projects");
        let width = stats
            .top_projects
            .iter()
            .map(|p| display_path(&p.project).chars().count())
            .max()
            .unwrap_or(0);
        for project in &stats.top_projects {
            print_totals(&display_path(&project.project), width, project.totals);
        }
    }

    println!();
    match (footprint, last_run) {
        (Some(footprint), Some(at)) => println!(
            "Build output now: {} in {} (measured {} ago)",
            format_bytes(footprint.bytes),
            count(footprint.projects, "project"),
            format_age(age(at))
        ),
        _ => println!("Build output now: unknown until the first run"),
    }
    if let Some(failure) = &stats.last_failure {
        println!(
            "Failures: {} (last {} in {}: {})",
            stats.failures,
            local_time(failure.at),
            display_path(&failure.project),
            failure.error.lines().next().unwrap_or_default()
        );
    }
}

/// Label column width for the period and action rows of `purge stats`.
fn stats_width() -> usize {
    "last 24 hours".len().max("unused profiles".len())
}

fn print_totals(label: &str, width: usize, totals: Totals) {
    println!(
        "  {label:<width$}  {:>9}  {}",
        format_bytes(totals.freed_bytes),
        count(totals.actions, "action")
    );
}

/// `1 project`, `2 projects`.
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// What each history action kind means, for people.
fn action_label(kind: &str) -> &str {
    match kind {
        "clean-all" => "whole projects",
        "clean" => "unused profiles",
        "refresh" => "rebuilt profiles",
        "trim" => "trimmed profiles",
        "prune" => "pruned caches",
        "clear" => "cleared caches",
        other => other,
    }
}

/// A bar proportional to `value / largest`; never empty for a non-zero value.
fn bar(value: u64, largest: u64) -> String {
    if largest == 0 || value == 0 {
        return String::new();
    }
    let cells = (value as f64 / largest as f64 * BAR_WIDTH as f64).round() as usize;
    "█".repeat(cells.clamp(1, BAR_WIDTH))
}

fn local_time(at: SystemTime) -> String {
    Timestamp::try_from(at).map_or_else(
        |_| "?".to_owned(),
        |ts| {
            ts.to_zoned(TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        },
    )
}

fn age(at: SystemTime) -> std::time::Duration {
    SystemTime::now().duration_since(at).unwrap_or_default()
}

/// Shows paths under the home directory as `~/...`.
pub fn display_path(path: &Path) -> String {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    let home = HOME.get_or_init(std::env::home_dir);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => path.display().to_string(),
    }
}
