//! The policy itself, as a pure function of what is on disk, what purge
//! remembers, and the current time.

use std::{
    collections::HashSet,
    fmt,
    time::{Duration, SystemTime},
};

use purge_config::Policy;
use purge_ecosystem::{ArtifactGroup, Artifacts, Part, Snapshot};
use purge_state::{GroupState, ProjectState};

use crate::report::{format_age, format_bytes};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Remove all of the project's build output.
    CleanAll { reason: Reason },
    /// Remove one artifact group, and build it again if `rebuild`.
    CleanGroup {
        group: String,
        reason: Reason,
        rebuild: bool,
    },
    /// Remove some parts of one artifact group, keeping the rest usable.
    Trim {
        group: String,
        parts: Vec<String>,
        reason: Reason,
    },
}

impl Action {
    /// Short name used in the history file.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::CleanAll { .. } => "clean-all",
            Self::CleanGroup { rebuild: false, .. } => "clean",
            Self::CleanGroup { rebuild: true, .. } => "refresh",
            Self::Trim { .. } => "trim",
        }
    }

    pub fn group(&self) -> Option<&str> {
        match self {
            Self::CleanAll { .. } => None,
            Self::CleanGroup { group, .. } | Self::Trim { group, .. } => Some(group),
        }
    }

    pub fn reason(&self) -> &Reason {
        match self {
            Self::CleanAll { reason }
            | Self::CleanGroup { reason, .. }
            | Self::Trim { reason, .. } => reason,
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CleanAll { .. } => f.write_str("clean everything"),
            Self::CleanGroup {
                group,
                rebuild: false,
                ..
            } => write!(f, "clean {group}"),
            Self::CleanGroup {
                group,
                rebuild: true,
                ..
            } => write!(f, "clean + rebuild {group}"),
            Self::Trim { group, .. } => write!(f, "trim {group}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Neither sources nor builds changed for `idle`.
    Dormant { idle: Duration },
    /// The group was not built for `idle` (`None`: never).
    Unused { idle: Option<Duration> },
    /// The group is `bytes` against a fresh size of `baseline`.
    Bloated { bytes: u64, baseline: Option<u64> },
    /// `parts` builds or caches in the group, `bytes` in all, went unused.
    Stale { parts: usize, bytes: u64 },
    /// Requested explicitly by the user.
    Manual,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dormant { idle } => write!(f, "no activity for {}", format_age(*idle)),
            Self::Unused { idle: Some(idle) } => write!(f, "not built for {}", format_age(*idle)),
            Self::Unused { idle: None } => f.write_str("never built"),
            Self::Bloated {
                bytes,
                baseline: Some(baseline),
            } => write!(
                f,
                "grew to {} (fresh build: {})",
                format_bytes(*bytes),
                format_bytes(*baseline)
            ),
            Self::Bloated {
                bytes,
                baseline: None,
            } => {
                write!(
                    f,
                    "{} with no fresh-build size recorded yet",
                    format_bytes(*bytes)
                )
            }
            Self::Stale { parts, bytes } => {
                let plural = if *parts == 1 { "" } else { "s" };
                write!(f, "{parts} stale build{plural} ({})", format_bytes(*bytes))
            }
            Self::Manual => f.write_str("requested manually"),
        }
    }
}

/// What should happen to a project right now. Empty means nothing.
pub fn decide(
    snapshot: &Snapshot,
    policy: &Policy,
    state: Option<&ProjectState>,
    now: SystemTime,
) -> Vec<Action> {
    if policy.skip || snapshot.artifacts.total_bytes == 0 {
        return Vec::new();
    }
    let Some(activity) = last_activity(snapshot, state) else {
        return Vec::new();
    };
    let idle = age(now, activity);
    if idle >= policy.dormant_after {
        return vec![Action::CleanAll {
            reason: Reason::Dormant { idle },
        }];
    }
    // Rebuilding while someone is mid-task would compete with their work.
    let quiet = idle < policy.quiet_period;
    snapshot
        .artifacts
        .groups
        .iter()
        .filter_map(|group| {
            let group_state = state.and_then(|s| s.groups.get(&group.id));
            decide_group(group, policy, group_state, now, !quiet)
        })
        .collect()
}

fn decide_group(
    group: &ArtifactGroup,
    policy: &Policy,
    state: Option<&GroupState>,
    now: SystemTime,
    may_rebuild: bool,
) -> Option<Action> {
    let bytes = group.usage.bytes;
    if bytes == 0 {
        return None;
    }
    let idle = effective_last_use(group, state).map(|used| age(now, used));
    if idle.is_none_or(|idle| idle >= policy.max_age_for(&group.profile)) {
        return Some(Action::CleanGroup {
            group: group.id.clone(),
            reason: Reason::Unused { idle },
            rebuild: false,
        });
    }

    if policy.trim {
        let unused = stale_parts(group, policy.max_age_for(&group.profile), now);
        let stale_bytes: u64 = unused.iter().map(|part| part.bytes).sum();
        if !unused.is_empty() && stale_bytes >= policy.min_savings {
            return Some(Action::Trim {
                group: group.id.clone(),
                reason: Reason::Stale {
                    parts: unused.len(),
                    bytes: stale_bytes,
                },
                parts: unused.into_iter().map(|part| part.id.clone()).collect(),
            });
        }
    }

    if !may_rebuild || !group.rebuildable || !policy.rebuild_for(&group.profile) {
        return None;
    }
    let cooling_down = state
        .and_then(|s| s.refreshed_at)
        .is_some_and(|refreshed| age(now, refreshed) < policy.refresh_cooldown);
    if cooling_down {
        return None;
    }
    let baseline = state.and_then(|s| s.baseline_bytes);
    let bloated = match baseline {
        Some(baseline) => {
            bytes as f64 >= baseline as f64 * policy.bloat_factor
                && bytes.saturating_sub(baseline) >= policy.min_savings
        }
        // Nothing to compare against. Usually the baseline is learned for
        // free (see `learn_baselines`); only a profile so big that it is
        // almost certainly bloated is rebuilt just to measure it.
        None => bytes >= policy.learn_rebuild_above.max(policy.min_savings),
    };
    bloated.then(|| Action::CleanGroup {
        group: group.id.clone(),
        reason: Reason::Bloated { bytes, baseline },
        rebuild: true,
    })
}

/// The parts of a group still in use that went unused for `max_age`: those
/// last used before then, and those built by a compiler that built or used
/// nothing since. A part with no known last use is only judged by its
/// compiler, so a filesystem without access times never loses live parts.
pub fn stale_parts(group: &ArtifactGroup, max_age: Duration, now: SystemTime) -> Vec<&Part> {
    let recent = |time: Option<SystemTime>| time.is_some_and(|time| age(now, time) < max_age);
    let live_toolchains: HashSet<&str> = group
        .parts
        .iter()
        .filter(|part| recent(part.built) || recent(part.used))
        .filter_map(|part| part.toolchain.as_deref())
        .collect();
    group
        .parts
        .iter()
        .filter(|part| {
            if recent(part.built) || recent(part.used) {
                return false;
            }
            let unused = part.used.is_some();
            // With no compiler seen recently, there is nothing to compare
            // against: keep everything rather than guess.
            let old_compiler = !live_toolchains.is_empty()
                && part
                    .toolchain
                    .as_deref()
                    .is_some_and(|toolchain| !live_toolchains.contains(toolchain));
            unused || old_compiler
        })
        .collect()
}

/// Records the fresh size of groups the user has built since purge last
/// cleaned or rebuilt them. Right after a clean, the next build is a fresh
/// one, so its size is the baseline the bloat check needs, without purge
/// rebuilding anything. After a refresh, the user's next build adds what
/// purge's own build left out (tests, examples, other features), so the
/// baseline grows to include it. Returns the ids of the groups learned.
pub fn learn_baselines(artifacts: &Artifacts, state: &mut ProjectState) -> Vec<String> {
    let mut learned = Vec::new();
    for group in &artifacts.groups {
        let since = match state.groups.get(&group.id) {
            Some(known) if known.cleaned_at.is_some() => known.cleaned_at,
            Some(known) if known.baseline_bytes.is_some() => continue,
            _ => state.fully_cleaned_at,
        };
        let built = group
            .usage
            .newest
            .is_some_and(|newest| since.is_some_and(|since| newest > since));
        if group.usage.bytes == 0 || !built {
            continue;
        }
        let entry = state.groups.entry(group.id.clone()).or_default();
        entry.baseline_bytes = Some(entry.baseline_bytes.unwrap_or(0).max(group.usage.bytes));
        entry.cleaned_at = None;
        learned.push(group.id.clone());
    }
    learned
}

/// When the user last built `group`. A rebuild by purge itself does not
/// count, so refreshing a profile never keeps it alive forever.
pub fn effective_last_use(group: &ArtifactGroup, state: Option<&GroupState>) -> Option<SystemTime> {
    let newest = group.usage.newest.max(group.last_used);
    match state {
        Some(GroupState {
            refreshed_at: Some(refreshed),
            used_before_refresh,
            ..
        }) if newest.is_some_and(|newest| newest <= *refreshed) => *used_before_refresh,
        _ => newest,
    }
}

/// The later of the last source change and the last real build.
pub fn last_activity(snapshot: &Snapshot, state: Option<&ProjectState>) -> Option<SystemTime> {
    snapshot
        .artifacts
        .groups
        .iter()
        .filter_map(|group| effective_last_use(group, state.and_then(|s| s.groups.get(&group.id))))
        .chain(snapshot.source_modified)
        .max()
}

fn age(now: SystemTime, then: SystemTime) -> Duration {
    now.duration_since(then).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use purge_ecosystem::DiskUsage;

    use super::*;

    const DAY: Duration = Duration::from_hours(24);
    const MB: u64 = 1_000_000;

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + 1000 * DAY
    }

    fn days_ago(days: u64) -> SystemTime {
        now() - days as u32 * DAY
    }

    fn group(id: &str, mb: u64, built_days_ago: u64) -> ArtifactGroup {
        ArtifactGroup {
            id: id.into(),
            profile: id.rsplit('/').next().unwrap().into(),
            paths: vec![PathBuf::from("target").join(id)],
            usage: DiskUsage {
                bytes: mb * MB,
                total: mb * MB,
                newest: Some(days_ago(built_days_ago)),
            },
            last_used: None,
            rebuildable: true,
            parts: Vec::new(),
        }
    }

    fn snapshot(source_days_ago: u64, groups: Vec<ArtifactGroup>) -> Snapshot {
        let total_bytes = groups.iter().map(|g| g.usage.bytes).sum::<u64>() + MB;
        Snapshot {
            source_modified: Some(days_ago(source_days_ago)),
            artifacts: Artifacts {
                groups,
                total_bytes,
            },
        }
    }

    fn with_baseline(id: &str, mb: u64, refreshed_days_ago: u64) -> ProjectState {
        let mut state = ProjectState::default();
        state.groups.insert(
            id.into(),
            GroupState {
                cleaned_at: None,
                baseline_bytes: Some(mb * MB),
                refreshed_at: Some(days_ago(refreshed_days_ago)),
                used_before_refresh: Some(days_ago(refreshed_days_ago)),
            },
        );
        state
    }

    #[test]
    fn dormant_project_is_cleaned_entirely() {
        let snap = snapshot(20, vec![group("debug", 900, 15)]);
        let actions = decide(&snap, &Policy::default(), None, now());
        assert_eq!(
            actions,
            [Action::CleanAll {
                reason: Reason::Dormant { idle: 15 * DAY }
            }]
        );
    }

    #[test]
    fn recent_builds_keep_a_project_out_of_dormancy() {
        // Sources untouched for a month, but debug was built yesterday.
        let snap = snapshot(30, vec![group("debug", 100, 1)]);
        assert!(decide(&snap, &Policy::default(), None, now()).is_empty());
    }

    #[test]
    fn unused_profile_is_cleaned_without_rebuild() {
        let snap = snapshot(1, vec![group("debug", 100, 1), group("release", 2000, 20)]);
        let actions = decide(&snap, &Policy::default(), None, now());
        assert_eq!(
            actions,
            [Action::CleanGroup {
                group: "release".into(),
                reason: Reason::Unused {
                    idle: Some(20 * DAY)
                },
                rebuild: false,
            }]
        );
    }

    #[test]
    fn per_profile_max_age_is_respected() {
        let mut policy = Policy::default();
        policy.profiles.entry("release".into()).or_default().max_age = Some(30 * DAY);
        let snap = snapshot(1, vec![group("release", 100, 20)]);
        assert!(decide(&snap, &policy, None, now()).is_empty());
    }

    #[test]
    fn huge_profile_without_baseline_is_refreshed_to_learn() {
        let snap = snapshot(1, vec![group("debug", 6000, 1)]);
        let actions = decide(&snap, &Policy::default(), None, now());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind(), "refresh");
    }

    #[test]
    fn ordinary_profile_without_baseline_is_left_alone() {
        let snap = snapshot(1, vec![group("debug", 3000, 1)]);
        assert!(decide(&snap, &Policy::default(), None, now()).is_empty());
    }

    #[test]
    fn refresh_only_when_bloated_enough_to_matter() {
        let snap = |mb| snapshot(1, vec![group("debug", mb, 1)]);
        let state = with_baseline("debug", 1000, 10);
        let policy = Policy::default();
        // 1.4x: not bloated.
        assert!(decide(&snap(1400), &policy, Some(&state), now()).is_empty());
        // 1.6x and 600 MB over baseline: refresh.
        let actions = decide(&snap(1600), &policy, Some(&state), now());
        assert_eq!(actions[0].kind(), "refresh");
        // Small project: 2x but only 200 MB to gain.
        let small = with_baseline("debug", 200, 10);
        assert!(decide(&snap(400), &policy, Some(&small), now()).is_empty());
    }

    #[test]
    fn refresh_respects_cooldown_and_rebuild_switch() {
        let snap = snapshot(1, vec![group("debug", 3000, 1)]);
        let recent = with_baseline("debug", 1000, 2);
        assert!(decide(&snap, &Policy::default(), Some(&recent), now()).is_empty());

        let policy = Policy {
            rebuild: false,
            ..Policy::default()
        };
        assert!(decide(&snap, &policy, None, now()).is_empty());
    }

    #[test]
    fn no_refresh_right_after_activity() {
        let mut snap = snapshot(1, vec![group("debug", 6000, 1)]);
        assert_eq!(
            decide(&snap, &Policy::default(), None, now())[0].kind(),
            "refresh"
        );

        snap.source_modified = Some(now() - Duration::from_mins(10));
        assert!(decide(&snap, &Policy::default(), None, now()).is_empty());

        // Cleaning unused profiles is still fine while the project is active.
        let snap = snapshot(0, vec![group("release", 3000, 20)]);
        assert_eq!(
            decide(&snap, &Policy::default(), None, now())[0].kind(),
            "clean"
        );
    }

    #[test]
    fn learns_baseline_from_the_first_build_after_a_clean() {
        let mut state = ProjectState::default();
        let artifacts = |built_days_ago| Artifacts {
            groups: vec![
                group("debug", 900, built_days_ago),
                group("release", 400, 9),
            ],
            total_bytes: 0,
        };
        // Never cleaned by purge: nothing to learn from.
        assert!(learn_baselines(&artifacts(1), &mut state).is_empty());

        // Purge cleaned debug 5 days ago; the user has not rebuilt it yet.
        state.groups.entry("debug".into()).or_default().cleaned_at = Some(days_ago(5));
        assert!(learn_baselines(&artifacts(6), &mut state).is_empty());

        // Rebuilt 2 days ago: that size is the fresh baseline.
        assert_eq!(learn_baselines(&artifacts(2), &mut state), ["debug"]);
        let debug = &state.groups["debug"];
        assert_eq!(debug.baseline_bytes, Some(900 * MB));
        assert_eq!(debug.cleaned_at, None);
        // Learned once; later growth is what the bloat check measures.
        assert!(learn_baselines(&artifacts(1), &mut state).is_empty());
    }

    #[test]
    fn full_clean_lets_every_later_profile_learn() {
        let mut state = ProjectState {
            fully_cleaned_at: Some(days_ago(10)),
            ..ProjectState::default()
        };
        let artifacts = Artifacts {
            groups: vec![group("debug", 900, 3), group("release", 400, 2)],
            total_bytes: 0,
        };
        assert_eq!(
            learn_baselines(&artifacts, &mut state),
            ["debug", "release"]
        );
    }

    #[test]
    fn users_first_build_after_a_refresh_raises_the_baseline() {
        // Purge rebuilt debug at 900 MB 3 days ago; the user's tests since
        // added 300 MB that every later build will need again.
        let mut state = ProjectState::default();
        state.groups.insert(
            "debug".into(),
            GroupState {
                cleaned_at: Some(days_ago(3)),
                baseline_bytes: Some(900 * MB),
                refreshed_at: Some(days_ago(3)),
                used_before_refresh: Some(days_ago(4)),
            },
        );
        let artifacts = |mb, built_days_ago| Artifacts {
            groups: vec![group("debug", mb, built_days_ago)],
            total_bytes: 0,
        };
        assert!(learn_baselines(&artifacts(900, 3), &mut state).is_empty());
        assert_eq!(learn_baselines(&artifacts(1200, 1), &mut state), ["debug"]);
        assert_eq!(state.groups["debug"].baseline_bytes, Some(1200 * MB));
        // Only the first build counts; growth after it is bloat.
        assert!(learn_baselines(&artifacts(2000, 0), &mut state).is_empty());
    }

    #[test]
    fn failed_rebuild_output_is_not_a_baseline() {
        let mut state = ProjectState::default();
        state.groups.insert(
            "debug".into(),
            GroupState {
                cleaned_at: Some(days_ago(2)),
                baseline_bytes: None,
                refreshed_at: Some(days_ago(2)),
                used_before_refresh: Some(days_ago(3)),
            },
        );
        // Partial output from the failed build predates the record.
        let partial = Artifacts {
            groups: vec![group("debug", 200, 2)],
            total_bytes: 0,
        };
        assert!(learn_baselines(&partial, &mut state).is_empty());
        // ...and does not count as the user building the profile.
        assert_eq!(
            effective_last_use(&partial.groups[0], state.groups.get("debug")),
            Some(days_ago(3))
        );
    }

    #[test]
    fn reading_a_group_counts_as_using_it() {
        // Installed a month ago, loaded yesterday: in use.
        let mut deps = group("node_modules", 500, 30);
        deps.rebuildable = false;
        deps.last_used = Some(days_ago(1));
        assert_eq!(effective_last_use(&deps, None), Some(days_ago(1)));
        let snap = snapshot(30, vec![deps.clone()]);
        assert!(decide(&snap, &Policy::default(), None, now()).is_empty());

        // Not loaded since: dormant like any other output.
        deps.last_used = Some(days_ago(20));
        let snap = snapshot(30, vec![deps]);
        assert_eq!(
            decide(&snap, &Policy::default(), None, now())[0].kind(),
            "clean-all"
        );
    }

    #[test]
    fn groups_other_tools_built_are_never_rebuilt() {
        let mut huge = group("rust-analyzer/debug", 6000, 1);
        huge.rebuildable = false;
        let snap = snapshot(1, vec![huge]);
        assert!(decide(&snap, &Policy::default(), None, now()).is_empty());
    }

    fn part(id: &str, mb: u64, built: u64, used: Option<u64>, toolchain: &str) -> Part {
        Part {
            id: id.into(),
            bytes: mb * MB,
            built: Some(days_ago(built)),
            used: used.map(days_ago),
            toolchain: (!toolchain.is_empty()).then(|| toolchain.into()),
        }
    }

    #[test]
    fn stale_builds_are_trimmed_without_rebuilding() {
        let mut debug = group("debug", 3000, 1);
        debug.parts = vec![
            part("live", 900, 30, Some(1), "new"),
            part("unused", 400, 30, Some(20), "new"),
            part("old-compiler", 300, 60, None, "old"),
            part("unknown-use", 200, 40, None, "new"),
            part("incremental/x", 50, 20, Some(20), ""),
        ];
        let ids: Vec<&str> = stale_parts(&debug, 14 * DAY, now())
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(ids, ["unused", "old-compiler", "incremental/x"]);

        let snap = snapshot(1, vec![debug.clone()]);
        let actions = decide(&snap, &Policy::default(), None, now());
        assert_eq!(
            actions,
            [Action::Trim {
                group: "debug".into(),
                parts: vec![
                    "unused".into(),
                    "old-compiler".into(),
                    "incremental/x".into()
                ],
                reason: Reason::Stale {
                    parts: 3,
                    bytes: 750 * MB
                },
            }]
        );
        assert_eq!(actions[0].reason().to_string(), "3 stale builds (750.0 MB)");

        // Below `min_savings`, or switched off: nothing to do.
        let policy = Policy {
            min_savings: 800 * MB,
            ..Policy::default()
        };
        assert!(decide(&snap, &policy, None, now()).is_empty());
        let policy = Policy {
            trim: false,
            ..Policy::default()
        };
        assert!(decide(&snap, &policy, None, now()).is_empty());
    }

    #[test]
    fn without_a_recent_compiler_nothing_is_judged_by_compiler() {
        let mut debug = group("debug", 3000, 1);
        // Access times unknown, nothing built recently: no basis to judge.
        debug.parts = vec![part("a", 900, 30, None, "x"), part("b", 900, 40, None, "y")];
        assert!(stale_parts(&debug, 14 * DAY, now()).is_empty());
    }

    #[test]
    fn purges_own_rebuild_does_not_count_as_use() {
        // Purge rebuilt debug 10 days ago; the user last built it 13 days ago.
        let mut state = with_baseline("debug", 500, 10);
        state.groups.get_mut("debug").unwrap().used_before_refresh = Some(days_ago(13));
        let mut built = group("debug", 500, 10);
        built.usage.newest = Some(days_ago(10));
        assert_eq!(
            effective_last_use(&built, state.groups.get("debug")),
            Some(days_ago(13))
        );
        // Sources edited yesterday, so not dormant, but debug idles out at 14d.
        let snap = snapshot(1, vec![built.clone()]);
        let later = now() + 2 * DAY;
        let actions = decide(&snap, &Policy::default(), Some(&state), later);
        assert_eq!(actions[0].kind(), "clean");

        // A user build after the refresh counts again.
        built.usage.newest = Some(days_ago(1));
        assert_eq!(
            effective_last_use(&built, state.groups.get("debug")),
            Some(days_ago(1))
        );
    }

    #[test]
    fn skip_and_empty_do_nothing() {
        let snap = snapshot(100, vec![group("debug", 900, 100)]);
        let policy = Policy {
            skip: true,
            ..Policy::default()
        };
        assert!(decide(&snap, &policy, None, now()).is_empty());
        assert!(decide(&Snapshot::default(), &Policy::default(), None, now()).is_empty());
    }

    #[test]
    fn cross_target_groups_use_their_profile_policy() {
        let mut policy = Policy::default();
        policy.profiles.entry("release".into()).or_default().max_age = Some(30 * DAY);
        let snap = snapshot(1, vec![group("aarch64-apple-darwin/release", 100, 20)]);
        assert!(decide(&snap, &policy, None, now()).is_empty());
    }
}
