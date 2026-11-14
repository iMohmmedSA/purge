use std::{collections::BTreeMap, path::Path};

use purge_ecosystem::Project;

/// One artifact directory and every discovered project that builds into it.
/// Usually a single project, or several that share a target directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// Never empty. All members share an ecosystem and an artifact directory.
    members: Vec<Project>,
}

impl Unit {
    pub fn single(project: Project) -> Self {
        Self {
            members: vec![project],
        }
    }

    /// Groups projects by ecosystem and artifact directory, ordered by [`Unit::key`].
    pub fn group(projects: Vec<Project>) -> Vec<Self> {
        let mut by_dir: BTreeMap<_, Vec<Project>> = BTreeMap::new();
        for project in projects {
            by_dir
                .entry((project.ecosystem, project.artifact_dir.clone()))
                .or_default()
                .push(project);
        }
        let mut units: Vec<Self> = by_dir
            .into_values()
            .map(|members| Self { members })
            .collect();
        units.sort_by(|a, b| a.key().cmp(b.key()));
        units
    }

    /// The project used to run the ecosystem's tools.
    pub fn primary(&self) -> &Project {
        &self.members[0]
    }

    pub fn members(&self) -> &[Project] {
        &self.members
    }

    pub fn ecosystem(&self) -> &'static str {
        self.primary().ecosystem
    }

    pub fn artifact_dir(&self) -> &Path {
        &self.primary().artifact_dir
    }

    /// Whether other projects, discovered or not, may build into the same
    /// directory.
    pub fn is_shared(&self) -> bool {
        self.members.len() > 1 || self.primary().has_external_artifacts()
    }

    /// Identifies the unit in state and history: the project root, or for
    /// shared output, the artifact directory.
    pub fn key(&self) -> &Path {
        if self.is_shared() {
            self.artifact_dir()
        } else {
            &self.primary().root
        }
    }

    pub fn name(&self) -> String {
        match self.members.as_slice() {
            [only] if !self.is_shared() => only.name.clone(),
            [only] => format!("{} (shared target)", only.name),
            all => format!("shared target of {} projects", all.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn project(root: &str, target: &str) -> Project {
        Project {
            ecosystem: "rust",
            name: root.rsplit('/').next().unwrap().into(),
            root: root.into(),
            artifact_dir: target.into(),
            extra_dirs: Vec::new(),
        }
    }

    #[test]
    fn groups_projects_sharing_a_target() {
        let units = Unit::group(vec![
            project("/dev/b", "/cache/target"),
            project("/dev/own", "/dev/own/target"),
            project("/dev/a", "/cache/target"),
        ]);
        assert_eq!(units.len(), 2);

        let shared = &units[0];
        assert!(shared.is_shared());
        assert_eq!(shared.key(), Path::new("/cache/target"));
        assert_eq!(shared.members().len(), 2);
        assert_eq!(shared.name(), "shared target of 2 projects");

        let own = &units[1];
        assert!(!own.is_shared());
        assert_eq!(own.key(), Path::new("/dev/own"));
        assert_eq!(own.name(), "own");
    }

    #[test]
    fn external_target_counts_as_shared_even_alone() {
        let unit = Unit::single(project("/dev/a", "/cache/target"));
        assert!(unit.is_shared());
        assert_eq!(unit.key(), PathBuf::from("/cache/target"));
    }
}
