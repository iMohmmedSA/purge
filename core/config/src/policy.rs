use std::{collections::BTreeMap, time::Duration};

use bytesize::ByteSize;
use serde::Deserialize;

const HOUR: Duration = Duration::from_hours(1);
const DAY: Duration = Duration::from_hours(24);

/// The fully resolved rules for one project.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    /// Leave the project alone entirely.
    pub skip: bool,
    /// With no source change or build for this long, remove all build output.
    pub dormant_after: Duration,
    /// Remove a profile that has not been built for this long, and within
    /// a profile still in use, the builds not used for this long.
    pub max_age: Duration,
    /// Remove stale builds inside profiles still in use: those not used
    /// for `max_age`, and those made by a compiler not used for that long.
    pub trim: bool,
    /// Allow clean-and-rebuild of profiles that are in use but bloated.
    pub rebuild: bool,
    /// A profile counts as bloated once it is this many times its fresh size...
    pub bloat_factor: f64,
    /// ...and cleaning would free at least this many bytes.
    pub min_savings: u64,
    /// With no fresh size recorded yet, rebuild once to measure it only if
    /// the profile is at least this big. Smaller profiles wait until purge
    /// has cleaned them and sees the user's next build.
    pub learn_rebuild_above: u64,
    /// Minimum time between two rebuilds of the same profile.
    pub refresh_cooldown: Duration,
    /// No rebuild while the project was edited or built more recently than
    /// this; the user is probably still working on it.
    pub quiet_period: Duration,
    /// Extra arguments for the rebuild command.
    pub build_args: Vec<String>,
    /// Per-profile overrides, keyed by profile name (`debug`, `release`, ...).
    pub profiles: BTreeMap<String, ProfilePatch>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            skip: false,
            dormant_after: 14 * DAY,
            max_age: 14 * DAY,
            trim: true,
            rebuild: true,
            bloat_factor: 1.5,
            min_savings: ByteSize::mb(500).as_u64(),
            learn_rebuild_above: ByteSize::gb(5).as_u64(),
            refresh_cooldown: 7 * DAY,
            quiet_period: HOUR,
            build_args: vec!["--workspace".into()],
            profiles: BTreeMap::new(),
        }
    }
}

impl Policy {
    pub fn max_age_for(&self, profile: &str) -> Duration {
        self.profiles
            .get(profile)
            .and_then(|p| p.max_age)
            .unwrap_or(self.max_age)
    }

    pub fn rebuild_for(&self, profile: &str) -> bool {
        self.profiles
            .get(profile)
            .and_then(|p| p.rebuild)
            .unwrap_or(self.rebuild)
    }

    /// The policy for build output shared by projects with `policies`: it
    /// is only cleaned once every one of them would allow it, so each
    /// setting takes its most cautious value. Shared output is never
    /// rebuilt, since that would mean building every project using it.
    /// Returns `None` for no policies.
    pub fn shared(policies: &[Self]) -> Option<Self> {
        let (first, rest) = policies.split_first()?;
        let mut shared = first.clone();
        let profile_names: Vec<String> = policies
            .iter()
            .flat_map(|policy| policy.profiles.keys().cloned())
            .collect();
        for other in rest {
            shared.skip |= other.skip;
            shared.dormant_after = shared.dormant_after.max(other.dormant_after);
            shared.max_age = shared.max_age.max(other.max_age);
            shared.trim &= other.trim;
        }
        shared.rebuild = false;
        shared.profiles = profile_names
            .into_iter()
            .map(|name| {
                let max_age = policies.iter().map(|p| p.max_age_for(&name)).max();
                let patch = ProfilePatch {
                    max_age,
                    rebuild: Some(false),
                };
                (name, patch)
            })
            .collect();
        Some(shared)
    }

    /// Overlays the fields `patch` sets.
    pub fn apply(&mut self, patch: &PolicyPatch) {
        let PolicyPatch {
            skip,
            dormant_after,
            max_age,
            trim,
            rebuild,
            bloat_factor,
            min_savings,
            learn_rebuild_above,
            refresh_cooldown,
            quiet_period,
            build_args,
            profiles,
        } = patch;
        set(&mut self.skip, *skip);
        set(&mut self.dormant_after, *dormant_after);
        set(&mut self.max_age, *max_age);
        set(&mut self.trim, *trim);
        set(&mut self.rebuild, *rebuild);
        set(&mut self.bloat_factor, *bloat_factor);
        set(&mut self.min_savings, min_savings.map(|b| b.as_u64()));
        set(
            &mut self.learn_rebuild_above,
            learn_rebuild_above.map(|b| b.as_u64()),
        );
        set(&mut self.refresh_cooldown, *refresh_cooldown);
        set(&mut self.quiet_period, *quiet_period);
        set(&mut self.build_args, build_args.clone());
        for (name, profile) in profiles {
            self.profiles
                .entry(name.clone())
                .or_default()
                .apply(profile);
        }
    }
}

fn set<T>(field: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *field = value;
    }
}

/// A partial [`Policy`] as written in the config file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPatch {
    pub skip: Option<bool>,
    #[serde(default, with = "humantime_serde")]
    pub dormant_after: Option<Duration>,
    #[serde(default, with = "humantime_serde")]
    pub max_age: Option<Duration>,
    pub trim: Option<bool>,
    pub rebuild: Option<bool>,
    pub bloat_factor: Option<f64>,
    pub min_savings: Option<ByteSize>,
    pub learn_rebuild_above: Option<ByteSize>,
    #[serde(default, with = "humantime_serde")]
    pub refresh_cooldown: Option<Duration>,
    #[serde(default, with = "humantime_serde")]
    pub quiet_period: Option<Duration>,
    pub build_args: Option<Vec<String>>,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfilePatch>,
}

/// Overrides for a single profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilePatch {
    #[serde(default, with = "humantime_serde")]
    pub max_age: Option<Duration>,
    pub rebuild: Option<bool>,
}

impl ProfilePatch {
    fn apply(&mut self, other: &Self) {
        self.max_age = other.max_age.or(self.max_age);
        self.rebuild = other.rebuild.or(self.rebuild);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_overrides_fall_back_to_project_values() {
        let mut policy = Policy::default();
        policy.apply(&PolicyPatch {
            max_age: Some(3 * DAY),
            profiles: [(
                "release".to_owned(),
                ProfilePatch {
                    max_age: Some(30 * DAY),
                    rebuild: Some(false),
                },
            )]
            .into(),
            ..PolicyPatch::default()
        });
        assert_eq!(policy.max_age_for("debug"), 3 * DAY);
        assert_eq!(policy.max_age_for("release"), 30 * DAY);
        assert!(policy.rebuild_for("debug"));
        assert!(!policy.rebuild_for("release"));
    }

    #[test]
    fn shared_policy_takes_the_most_cautious_values() {
        let mut keen = Policy {
            max_age: 3 * DAY,
            dormant_after: 5 * DAY,
            ..Policy::default()
        };
        keen.profiles.insert(
            "release".into(),
            ProfilePatch {
                max_age: Some(30 * DAY),
                rebuild: Some(true),
            },
        );
        let careful = Policy {
            skip: false,
            max_age: 10 * DAY,
            ..Policy::default()
        };
        let shared = Policy::shared(&[keen, careful]).unwrap();
        assert_eq!(shared.max_age_for("debug"), 10 * DAY);
        assert_eq!(shared.max_age_for("release"), 30 * DAY);
        assert_eq!(shared.dormant_after, 14 * DAY);
        assert!(!shared.rebuild_for("debug"));
        assert!(!shared.rebuild_for("release"));

        let skipped = Policy {
            skip: true,
            ..Policy::default()
        };
        assert!(Policy::shared(&[Policy::default(), skipped]).unwrap().skip);
        assert_eq!(Policy::shared(&[]), None);
    }

    #[test]
    fn later_profile_patches_merge_field_by_field() {
        let mut policy = Policy::default();
        let release = |patch: ProfilePatch| PolicyPatch {
            profiles: [("release".to_owned(), patch)].into(),
            ..PolicyPatch::default()
        };
        policy.apply(&release(ProfilePatch {
            max_age: Some(30 * DAY),
            rebuild: None,
        }));
        policy.apply(&release(ProfilePatch {
            max_age: None,
            rebuild: Some(false),
        }));
        assert_eq!(policy.max_age_for("release"), 30 * DAY);
        assert!(!policy.rebuild_for("release"));
    }
}
