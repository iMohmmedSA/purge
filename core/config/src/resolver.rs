use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};

use crate::{Config, Error, Policy, PolicyPatch, expand_tilde};

/// Answers "which policy applies to this project?" and "is this path
/// excluded?" for a loaded [`Config`].
#[derive(Debug)]
pub struct Resolver {
    base: Policy,
    rules: Vec<(PathMatcher, PolicyPatch)>,
    exclude: Vec<PathMatcher>,
}

impl Resolver {
    pub fn new(config: &Config) -> Result<Self, Error> {
        let mut base = Policy::default();
        base.apply(&config.policy);
        validate(&config.policy)?;
        let rules = config
            .projects
            .iter()
            .map(|rule| {
                validate(&rule.policy)?;
                Ok((PathMatcher::new(&rule.path)?, rule.policy.clone()))
            })
            .collect::<Result<_, Error>>()?;
        let exclude = config
            .exclude
            .iter()
            .map(|pattern| PathMatcher::new(pattern))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            base,
            rules,
            exclude,
        })
    }

    pub fn policy_for(&self, root: &Path) -> Policy {
        let mut policy = self.base.clone();
        for (matcher, patch) in &self.rules {
            if matcher.matches(root) {
                policy.apply(patch);
            }
        }
        policy
    }

    pub fn is_excluded(&self, path: &Path) -> bool {
        self.exclude.iter().any(|matcher| matcher.matches(path))
    }
}

fn validate(patch: &PolicyPatch) -> Result<(), Error> {
    match patch.bloat_factor {
        Some(factor) if factor.is_nan() || factor < 1.0 => Err(Error::Invalid(format!(
            "bloat_factor must be at least 1.0, got {factor}"
        ))),
        _ => Ok(()),
    }
}

/// A path prefix, or a glob if the pattern contains glob syntax.
#[derive(Debug)]
enum PathMatcher {
    Prefix(PathBuf),
    Glob(GlobMatcher),
}

impl PathMatcher {
    fn new(pattern: &str) -> Result<Self, Error> {
        let expanded = expand_tilde(Path::new(pattern));
        // Purge matches full paths, so a relative pattern would silently
        // never match. `**/name` is the way to match a name anywhere.
        if !expanded.is_absolute() && !pattern.starts_with("**") {
            return Err(Error::Invalid(format!(
                "path pattern `{pattern}` must be an absolute path, or start with `~` or `**` (e.g. `**/{pattern}`)"
            )));
        }
        if !pattern.contains(['*', '?', '[', '{']) {
            return Ok(Self::Prefix(expanded));
        }
        GlobBuilder::new(&expanded.to_string_lossy())
            .literal_separator(true)
            .build()
            .map(|glob| Self::Glob(glob.compile_matcher()))
            .map_err(|source| Error::Pattern {
                pattern: pattern.to_owned(),
                source,
            })
    }

    fn matches(&self, path: &Path) -> bool {
        match self {
            Self::Prefix(prefix) => path.starts_with(prefix),
            Self::Glob(glob) => glob.is_match(path),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::*;

    /// An absolute path on this system: Windows needs a drive letter.
    fn abs(path: &str) -> String {
        if cfg!(windows) {
            format!("C:{path}")
        } else {
            path.to_owned()
        }
    }

    fn at(path: &str) -> PathBuf {
        PathBuf::from(abs(path))
    }

    fn resolver(toml: &str) -> Resolver {
        Resolver::new(&Config::parse(toml).unwrap()).unwrap()
    }

    #[test]
    fn rules_apply_in_order_on_top_of_defaults() {
        let resolver = resolver(&format!(
            r#"
            [policy]
            max_age = "5d"
            [[project]]
            path = "{}"
            max_age = "20d"
            [[project]]
            path = "{}"
            skip = true
            "#,
            abs("/dev/work"),
            abs("/dev/work/legacy"),
        ));
        let day = Duration::from_hours(24);
        assert_eq!(resolver.policy_for(&at("/dev/play")).max_age, 5 * day);
        assert_eq!(resolver.policy_for(&at("/dev/work/api")).max_age, 20 * day);
        let legacy = resolver.policy_for(&at("/dev/work/legacy"));
        assert!(legacy.skip);
        assert_eq!(legacy.max_age, 20 * day);
        // Prefix matching is by component, not by string.
        assert_eq!(resolver.policy_for(&at("/dev/workshop")).max_age, 5 * day);
    }

    #[test]
    fn excludes_by_prefix_and_glob() {
        let resolver = resolver(&format!(
            r#"exclude = ["{}", "{}"]"#,
            abs("/dev/vendor"),
            abs("/dev/*/third_party"),
        ));
        assert!(resolver.is_excluded(&at("/dev/vendor/x")));
        assert!(resolver.is_excluded(&at("/dev/app/third_party")));
        assert!(!resolver.is_excluded(&at("/dev/a/b/third_party")));
        assert!(!resolver.is_excluded(&at("/dev/app")));
    }

    #[test]
    fn rejects_relative_patterns_that_could_never_match() {
        let config = Config::parse(r#"exclude = ["vendor"]"#).unwrap();
        let err = Resolver::new(&config).unwrap_err();
        assert!(err.to_string().contains("**/vendor"), "{err}");
        assert!(
            Config::parse("[[project]]\npath = \"work\"\nskip = true")
                .is_ok_and(|c| Resolver::new(&c).is_err())
        );

        let anywhere = resolver(r#"exclude = ["**/vendor"]"#);
        assert!(anywhere.is_excluded(&at("/dev/app/vendor")));
    }

    #[test]
    fn rejects_bloat_factor_below_one() {
        let config = Config::parse("[policy]\nbloat_factor = 0.5").unwrap();
        assert!(Resolver::new(&config).is_err());
    }
}
