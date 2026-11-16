use std::{
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

use purge_config::{Config, Policy, Resolver};
use purge_ecosystem::{ArtifactGroup, Ecosystem, Project, Snapshot, fsutil};
use purge_process::ProcessTable;
use purge_scan::Scanner;
use purge_state::{GroupState, ProjectState, State};
use tracing::{info, warn};

use crate::{
    Action, ActionOutcome, Outcome, RunReport, Unit, UnitReport, UnitStatus, decide,
    decide::{effective_last_use, last_activity, learn_baselines},
    report::{format_age, format_bytes},
};

#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Decide but change nothing.
    pub dry_run: bool,
    /// Allow clean-and-rebuild actions; when false they are deferred.
    pub rebuild: bool,
    /// At most this many rebuilds in this run.
    pub max_rebuilds: usize,
}

/// Ties ecosystems, configuration and state together for one run.
#[derive(Debug)]
pub struct Engine<'a> {
    ecosystems: &'a [Box<dyn Ecosystem>],
    resolver: Resolver,
    roots: Vec<PathBuf>,
    max_depth: usize,
}

impl<'a> Engine<'a> {
    pub fn new(
        ecosystems: &'a [Box<dyn Ecosystem>],
        config: &Config,
    ) -> Result<Self, purge_config::Error> {
        Ok(Self {
            ecosystems,
            resolver: Resolver::new(config)?,
            roots: config.roots.clone(),
            max_depth: config.max_depth,
        })
    }

    pub fn ecosystem(&self, name: &str) -> Option<&dyn Ecosystem> {
        self.ecosystems
            .iter()
            .find(|e| e.name() == name)
            .map(AsRef::as_ref)
    }

    /// The policy for a unit. Shared output follows [`Policy::shared`].
    pub fn policy_for(&self, unit: &Unit) -> Policy {
        let policies: Vec<Policy> = unit
            .members()
            .iter()
            .map(|project| self.resolver.policy_for(&project.root))
            .collect();
        if unit.is_shared() {
            Policy::shared(&policies).unwrap_or_default()
        } else {
            policies.into_iter().next().unwrap_or_default()
        }
    }

    /// Projects under the configured roots, or under `roots` if given.
    pub fn discover(&self, roots: Option<&[PathBuf]>) -> Vec<Project> {
        let exclude = |path: &Path| self.resolver.is_excluded(path);
        Scanner::new(self.ecosystems, self.max_depth, &exclude).scan(roots.unwrap_or(&self.roots))
    }

    /// Measures a unit: its build output once, and the sources of every member.
    pub fn snapshot(
        &self,
        ecosystem: &dyn Ecosystem,
        unit: &Unit,
    ) -> purge_ecosystem::Result<Snapshot> {
        let artifacts = ecosystem.inspect(unit.primary())?;
        let source_modified = unit
            .members()
            .iter()
            .filter_map(|project| ecosystem.source_modified(project))
            .max();
        Ok(Snapshot {
            source_modified,
            artifacts,
        })
    }

    /// Decides and, unless `dry_run`, acts on every project. Asked to stop
    /// (see [`purge_process::request_stop`]), it finishes the step under way
    /// and leaves the rest for the next run.
    pub fn run(
        &self,
        projects: Vec<Project>,
        state: &mut State,
        options: &RunOptions,
    ) -> RunReport {
        let started = SystemTime::now();
        let processes = ProcessTable::capture().unwrap_or_else(|err| {
            warn!("cannot list processes, relying on build locks only: {err}");
            ProcessTable::default()
        });
        let mut rebuilds_left = options.max_rebuilds;
        let units = Unit::group(projects)
            .into_iter()
            .map_while(|unit| {
                (!purge_process::stop_requested())
                    .then(|| self.process(unit, state, &processes, options, &mut rebuilds_left))
            })
            .collect();
        let report = RunReport {
            started,
            finished: SystemTime::now(),
            dry_run: options.dry_run,
            units,
        };
        // A run cut short is not a run: the next one is due as soon as purge
        // starts again, and its partial measurements say nothing overall.
        if !options.dry_run && !purge_process::stop_requested() {
            state.prune();
            state.last_run = Some(report.finished);
            state.footprint = Some(report.footprint());
        }
        report
    }

    fn process(
        &self,
        unit: Unit,
        state: &mut State,
        processes: &ProcessTable,
        options: &RunOptions,
        rebuilds_left: &mut usize,
    ) -> UnitReport {
        let mut report = UnitReport::new(unit);
        let unit = &report.unit;
        let Some(ecosystem) = self.ecosystem(unit.ecosystem()) else {
            report.status = UnitStatus::Failed(format!("unknown ecosystem `{}`", unit.ecosystem()));
            return report;
        };
        let policy = self.policy_for(unit);
        if policy.skip {
            report.status = UnitStatus::Skipped;
            return report;
        }
        let snapshot = match self.snapshot(ecosystem, unit) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                report.status = UnitStatus::Failed(err.to_string());
                return report;
            }
        };
        if let Some(entry) = state.projects.get_mut(unit.key()) {
            for group in learn_baselines(&snapshot.artifacts, entry) {
                info!(unit = %unit.key().display(), group, "recorded fresh build size");
            }
        }
        let remembered = state.projects.get(unit.key());
        report.activity = last_activity(&snapshot, remembered);
        let actions = decide(&snapshot, &policy, remembered, SystemTime::now());

        let busy = (!actions.is_empty())
            .then(|| {
                unit.members()
                    .iter()
                    .find_map(|project| ecosystem.busy(project, processes))
            })
            .flatten();
        if let Some(reason) = busy {
            report.actions = actions
                .into_iter()
                .map(|action| ActionOutcome {
                    action,
                    outcome: Outcome::Deferred(reason.clone()),
                })
                .collect();
            report.status = UnitStatus::Busy(reason);
        } else if !actions.is_empty() {
            let entry = state.projects.entry(unit.key().to_path_buf()).or_default();
            report.actions = Context {
                ecosystem,
                unit,
                snapshot: &snapshot,
                policy: &policy,
                options,
                rebuilds_left,
            }
            .apply_all(actions, entry);
        }
        report.snapshot = Some(snapshot);
        report
    }

    /// Carries out `actions` on one unit regardless of the policy, as for a
    /// manual `purge clean`.
    pub fn apply(
        &self,
        unit: Unit,
        snapshot: Snapshot,
        actions: Vec<Action>,
        state: &mut State,
        options: &RunOptions,
    ) -> RunReport {
        let started = SystemTime::now();
        let mut report = UnitReport::new(unit);
        report.activity = last_activity(&snapshot, state.projects.get(report.unit.key()));
        match self.ecosystem(report.unit.ecosystem()) {
            Some(ecosystem) => {
                let unit = &report.unit;
                let policy = self.policy_for(unit);
                let mut rebuilds_left = options.max_rebuilds;
                let entry = state.projects.entry(unit.key().to_path_buf()).or_default();
                report.actions = Context {
                    ecosystem,
                    unit,
                    snapshot: &snapshot,
                    policy: &policy,
                    options,
                    rebuilds_left: &mut rebuilds_left,
                }
                .apply_all(actions, entry);
            }
            None => {
                report.status =
                    UnitStatus::Failed(format!("unknown ecosystem `{}`", report.unit.ecosystem()));
            }
        }
        report.snapshot = Some(snapshot);
        RunReport {
            started,
            finished: SystemTime::now(),
            dry_run: options.dry_run,
            units: vec![report],
        }
    }
}

/// Everything needed to carry out actions on one unit.
struct Context<'r> {
    ecosystem: &'r dyn Ecosystem,
    unit: &'r Unit,
    snapshot: &'r Snapshot,
    policy: &'r Policy,
    options: &'r RunOptions,
    rebuilds_left: &'r mut usize,
}

impl Context<'_> {
    fn apply_all(&mut self, actions: Vec<Action>, entry: &mut ProjectState) -> Vec<ActionOutcome> {
        actions
            .into_iter()
            .map(|action| {
                let outcome = if purge_process::stop_requested() {
                    Outcome::Deferred("purge is stopping".into())
                } else {
                    self.apply(&action, entry)
                };
                ActionOutcome { action, outcome }
            })
            .collect()
    }

    fn apply(&mut self, action: &Action, entry: &mut ProjectState) -> Outcome {
        let outcome = match action {
            Action::CleanAll { .. } => self.clean_all(entry),
            Action::CleanGroup { group, rebuild, .. } => {
                match self.snapshot.artifacts.group(group) {
                    Some(group) => self.clean_group(group, *rebuild, entry),
                    None => Outcome::Failed(format!("no artifact group `{group}`")),
                }
            }
            Action::Trim { group, parts, .. } => match self.snapshot.artifacts.group(group) {
                Some(group) => self.trim(group, parts),
                None => Outcome::Failed(format!("no artifact group `{group}`")),
            },
        };
        let key = self.unit.key().display();
        match &outcome {
            Outcome::Done { freed } => {
                info!(unit = %key, "{action}: freed {} ({})", format_bytes(*freed), action.reason());
            }
            Outcome::Failed(err) => warn!(unit = %key, "{action} failed: {err}"),
            Outcome::Deferred(why) => info!(unit = %key, "{action} deferred: {why}"),
            Outcome::Planned { .. } => {}
        }
        outcome
    }

    fn clean_all(&self, entry: &mut ProjectState) -> Outcome {
        let before = self.snapshot.artifacts.total_bytes;
        if self.options.dry_run {
            return Outcome::Planned {
                estimate: Some(before),
            };
        }
        if let Err(err) = self.ecosystem.clean_all(self.unit.primary()) {
            return Outcome::Failed(err.to_string());
        }
        let left = fsutil::disk_usage_all(self.unit.primary().artifact_dirs()).bytes;
        entry.groups.clear();
        entry.fully_cleaned_at = Some(SystemTime::now());
        Outcome::Done {
            freed: before.saturating_sub(left),
        }
    }

    /// Removes stale parts of a group. Nothing about the group changes in
    /// the state: what is left is exactly what is in use.
    fn trim(&self, group: &ArtifactGroup, parts: &[String]) -> Outcome {
        if self.options.dry_run {
            let estimate = group
                .parts
                .iter()
                .filter(|part| parts.contains(&part.id))
                .map(|part| part.bytes)
                .sum();
            return Outcome::Planned {
                estimate: Some(estimate),
            };
        }
        let before = group.usage.bytes;
        if let Err(err) = self.ecosystem.trim(self.unit.primary(), group, parts) {
            return Outcome::Failed(err.to_string());
        }
        Outcome::Done {
            freed: before.saturating_sub(fsutil::disk_usage_all(&group.paths).bytes),
        }
    }

    fn clean_group(
        &mut self,
        group: &ArtifactGroup,
        rebuild: bool,
        entry: &mut ProjectState,
    ) -> Outcome {
        // A rebuild is only planned for a profile still in use, so if it
        // cannot happen now, leave the profile alone rather than strip it.
        if rebuild && !self.options.rebuild {
            return Outcome::Deferred("rebuilds are disabled for this run".into());
        }
        if rebuild && *self.rebuilds_left == 0 {
            return Outcome::Deferred("rebuild limit for this run reached".into());
        }
        let before = group.usage.bytes;
        let remembered = entry.groups.get(&group.id);
        if rebuild {
            *self.rebuilds_left -= 1;
        }
        if self.options.dry_run {
            let estimate = if rebuild {
                remembered
                    .and_then(|s| s.baseline_bytes)
                    .map(|b| before.saturating_sub(b))
            } else {
                Some(before)
            };
            return Outcome::Planned { estimate };
        }

        let project = self.unit.primary();
        let used = effective_last_use(group, remembered);
        if let Err(err) = self.ecosystem.clean_group(project, group) {
            return Outcome::Failed(err.to_string());
        }
        if !rebuild {
            entry.groups.insert(
                group.id.clone(),
                GroupState {
                    cleaned_at: Some(SystemTime::now()),
                    ..GroupState::default()
                },
            );
            return Outcome::Done {
                freed: before.saturating_sub(fsutil::disk_usage_all(&group.paths).bytes),
            };
        }

        info!(unit = %self.unit.key().display(), group = group.id, "rebuilding");
        let rebuild_started = Instant::now();
        let build = self
            .ecosystem
            .rebuild_group(project, group, &self.policy.build_args);
        let after = fsutil::disk_usage_all(&group.paths).bytes;
        let now = SystemTime::now();
        // Either way, what is in the group now was built by purge, not the
        // user: it must not count as use, and it is only a baseline if the
        // build finished. The user's next build refines the baseline.
        entry.groups.insert(
            group.id.clone(),
            GroupState {
                cleaned_at: Some(now),
                baseline_bytes: build.is_ok().then_some(after),
                refreshed_at: Some(now),
                used_before_refresh: used,
            },
        );
        if let Err(err) = build {
            if purge_process::stop_requested() {
                // Stopped mid-build: the space is freed, and the user's next
                // build of the profile simply finishes the work.
                info!(unit = %self.unit.key().display(), group = group.id, "rebuild stopped: {err}");
                return Outcome::Done {
                    freed: before.saturating_sub(after),
                };
            }
            return Outcome::Failed(format!("cleaned, but the rebuild failed: {err}"));
        }
        info!(
            unit = %self.unit.key().display(),
            group = group.id,
            "rebuilt in {}",
            format_age(rebuild_started.elapsed())
        );
        Outcome::Done {
            freed: before.saturating_sub(after),
        }
    }
}
