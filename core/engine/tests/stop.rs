//! Asked to stop, the engine finishes the action under way and leaves the
//! rest. In a test binary of its own, since the stop request is
//! process-wide.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

use purge_config::Config;
use purge_ecosystem::{ArtifactGroup, Artifacts, DiskUsage, Ecosystem, Project, Result, Snapshot};
use purge_engine::{Action, Engine, Outcome, Reason, RunOptions, Unit};
use purge_process::ProcessTable;
use purge_state::State;

/// Records what it was asked to clean; asks purge to stop during the first.
#[derive(Debug, Default)]
struct Fake {
    cleaned: Mutex<Vec<String>>,
}

impl Ecosystem for Fake {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn detect(&self, _: &Path) -> Result<Option<Project>> {
        Ok(None)
    }
    fn source_modified(&self, _: &Project) -> Option<SystemTime> {
        None
    }
    fn inspect(&self, _: &Project) -> Result<Artifacts> {
        Ok(Artifacts::default())
    }
    fn busy(&self, _: &Project, _: &ProcessTable) -> Option<String> {
        None
    }
    fn clean_all(&self, _: &Project) -> Result<()> {
        Ok(())
    }
    fn clean_group(&self, _: &Project, group: &ArtifactGroup) -> Result<()> {
        // Ctrl-C arrives while this one is being removed.
        purge_process::request_stop();
        self.cleaned.lock().unwrap().push(group.id.clone());
        Ok(())
    }
    fn trim(&self, _: &Project, _: &ArtifactGroup, _: &[String]) -> Result<()> {
        Ok(())
    }
    fn rebuild_group(&self, _: &Project, _: &ArtifactGroup, _: &[String]) -> Result<()> {
        Ok(())
    }
}

fn group(id: &str) -> ArtifactGroup {
    ArtifactGroup {
        id: id.into(),
        profile: id.into(),
        paths: vec![PathBuf::from("/nowhere").join(id)],
        usage: DiskUsage::default(),
        last_used: None,
        rebuildable: false,
        parts: Vec::new(),
    }
}

fn clean(id: &str) -> Action {
    Action::CleanGroup {
        group: id.into(),
        reason: Reason::Manual,
        rebuild: false,
    }
}

#[test]
fn finishes_the_step_under_way_and_leaves_the_rest() {
    let fake = Fake::default();
    let ecosystems: Vec<Box<dyn Ecosystem>> = vec![Box::new(fake)];
    let engine = Engine::new(&ecosystems, &Config::default()).unwrap();
    let unit = Unit::single(Project {
        ecosystem: "fake",
        name: "app".into(),
        root: "/nowhere".into(),
        artifact_dir: "/nowhere/out".into(),
        extra_dirs: Vec::new(),
    });
    let snapshot = Snapshot {
        source_modified: None,
        artifacts: Artifacts {
            groups: vec![group("debug"), group("release")],
            total_bytes: 0,
        },
    };
    let options = RunOptions {
        dry_run: false,
        rebuild: true,
        max_rebuilds: 1,
    };
    let mut state = State::default();
    let report = engine.apply(
        unit,
        snapshot,
        vec![clean("debug"), clean("release")],
        &mut state,
        &options,
    );

    let outcomes: Vec<&Outcome> = report.outcomes().map(|(_, a)| &a.outcome).collect();
    assert!(matches!(outcomes[0], Outcome::Done { .. }), "{outcomes:?}");
    assert_eq!(outcomes[1], &Outcome::Deferred("purge is stopping".into()));

    // A run cut short is not recorded as done, so the next one comes soon.
    engine.run(Vec::new(), &mut state, &options);
    assert_eq!(state.last_run, None);
}
