//! Freeing space in package caches that tools share between projects.

use std::time::SystemTime;

use purge_config::Caches;
use purge_ecosystem::SharedCache;

use crate::{Outcome, format_age, format_bytes};

/// What happened to one shared cache in a run.
#[derive(Debug, Clone)]
pub struct CacheReport {
    /// The ecosystem whose tool manages the cache.
    pub ecosystem: &'static str,
    pub cache: SharedCache,
    /// `prune` (only what no project uses) or `clear` (everything).
    pub action: &'static str,
    /// Why it was or was not freed, for people.
    pub note: String,
    /// `None` when there was nothing to do.
    pub outcome: Option<Outcome>,
}

/// Decides whether to free space in `cache` now: the action to take, or why
/// not. Only caches that free just what no project uses are touched
/// without being asked; the others need listing in `caches.clear`.
pub fn decide_cache(
    cache: &SharedCache,
    config: &Caches,
    last: Option<SystemTime>,
    now: SystemTime,
) -> Result<&'static str, String> {
    if !config.enabled {
        return Err("shared caches are switched off".into());
    }
    let action = if cache.unused_only {
        "prune"
    } else if config.clear.iter().any(|id| id == cache.id) {
        "clear"
    } else {
        return Err(format!(
            "{} cannot tell what is unused; add \"{}\" to caches.clear to clear it anyway",
            cache.name, cache.id
        ));
    };
    if cache.unused_bytes < config.min_savings.as_u64() {
        return Err(format!("{} unused", format_bytes(cache.unused_bytes)));
    }
    if let Some(last) = last {
        let since = now.duration_since(last).unwrap_or_default();
        if since < config.every {
            return Err(format!("freed {} ago", format_age(since)));
        }
    }
    Ok(action)
}

/// The note for a cache about to be freed.
pub(crate) fn reason(cache: &SharedCache, action: &str) -> String {
    let unused = format_bytes(cache.unused_bytes);
    if action == "prune" {
        format!("{unused} no project uses")
    } else {
        format!("{unused}; cleared entirely as caches.clear asks")
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::*;

    const MB: u64 = 1_000_000;
    const DAY: Duration = Duration::from_hours(24);

    fn cache(id: &'static str, unused_mb: u64, unused_only: bool) -> SharedCache {
        SharedCache {
            id,
            name: format!("{id} cache"),
            path: PathBuf::from("/caches").join(id),
            total_bytes: 2 * unused_mb * MB,
            unused_bytes: unused_mb * MB,
            unused_only,
            tools: &[],
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + 1000 * DAY
    }

    #[test]
    fn prunes_what_no_project_uses_without_asking() {
        let config = Caches::default();
        assert_eq!(
            decide_cache(&cache("pnpm", 300, true), &config, None, now()),
            Ok("prune")
        );
        // Too little to bother, or done recently.
        assert!(decide_cache(&cache("pnpm", 20, true), &config, None, now()).is_err());
        let recent = Some(now() - 2 * DAY);
        let err = decide_cache(&cache("pnpm", 300, true), &config, recent, now()).unwrap_err();
        assert_eq!(err, "freed 2d ago");
        let long_ago = Some(now() - 8 * DAY);
        assert!(decide_cache(&cache("pnpm", 300, true), &config, long_ago, now()).is_ok());
    }

    #[test]
    fn clears_everything_only_when_listed() {
        let mut config = Caches::default();
        let npm = cache("npm", 300, false);
        let err = decide_cache(&npm, &config, None, now()).unwrap_err();
        assert!(err.contains("add \"npm\" to caches.clear"), "{err}");
        config.clear = vec!["npm".into()];
        assert_eq!(decide_cache(&npm, &config, None, now()), Ok("clear"));
        // Listing one cache does not list another.
        assert!(decide_cache(&cache("bun", 300, false), &config, None, now()).is_err());
    }

    #[test]
    fn switched_off_touches_nothing() {
        let config = Caches {
            enabled: false,
            ..Caches::default()
        };
        assert!(decide_cache(&cache("pnpm", 900, true), &config, None, now()).is_err());
    }
}
