//! One function per subcommand.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
    time::SystemTime,
};

use anyhow::{Context as _, Result, anyhow, bail};
use jiff::Timestamp;
use purge_config::{Config, Paths, expand_tilde};
use purge_ecosystem::ArtifactGroup;
use purge_engine::{
    Action, Engine, Reason, RunOptions, RunReport, Unit, format_age, format_bytes, stale_parts,
};
use purge_process::ProcessTable;
use purge_session::Session;
use purge_state::Stats;

use crate::{
    args::{ConfigCommand, ServiceCommand},
    output,
};

/// History entries shown by `purge status`.
const STATUS_HISTORY: usize = 5;

const NO_ROOTS: &str = "no roots configured; run `purge config init <folder>`";
const RUN_IN_PROGRESS: &str = "another purge run is in progress";

/// `purge scan` and `purge run --dry-run`.
pub fn scan(session: &Session, folders: &[PathBuf]) -> Result<ExitCode> {
    let folders: Vec<PathBuf> = folders.iter().map(|f| expand_tilde(f)).collect();
    if folders.is_empty() && session.config().roots.is_empty() {
        bail!(NO_ROOTS);
    }
    let report = session.preview((!folders.is_empty()).then_some(&folders[..]))?;
    output::print_report(&report);
    Ok(exit_code(&report))
}

/// `purge run`.
pub fn run(session: &Session, rebuild: bool) -> Result<ExitCode> {
    if session.config().roots.is_empty() {
        bail!(NO_ROOTS);
    }
    let report = session
        .run(rebuild)?
        .ok_or_else(|| anyhow!(RUN_IN_PROGRESS))?;
    output::print_report(&report);
    Ok(exit_code(&report))
}

/// What `purge clean` was asked to do.
#[derive(Debug)]
pub struct CleanRequest<'a> {
    /// Only this profile, instead of everything.
    pub profile: Option<&'a str>,
    /// Build the profile again afterwards.
    pub rebuild: bool,
    /// Only stale builds, keeping what is in use.
    pub stale: bool,
    /// Even if busy or shared.
    pub force: bool,
}

/// `purge clean`.
pub fn clean(session: &Session, path: &Path, request: &CleanRequest) -> Result<ExitCode> {
    let dir =
        dunce::canonicalize(path).with_context(|| format!("cannot open {}", path.display()))?;
    let ecosystems = purge_ecosystems::all();
    let engine = Engine::new(&ecosystems, session.config())?;
    let (ecosystem, project) = ecosystems
        .iter()
        .find_map(|ecosystem| {
            ecosystem
                .detect(&dir)
                .transpose()
                .map(|project| project.map(|project| (ecosystem, project)))
        })
        .transpose()?
        .ok_or_else(|| anyhow!("no project with build output at {}", dir.display()))?;
    if !request.force {
        let processes = ProcessTable::capture().unwrap_or_default();
        if let Some(reason) = ecosystem.busy(&project, &processes) {
            bail!("{reason}; pass --force to clean anyway");
        }
        // Stale builds are unused by every project, so trimming shared
        // output is as safe as trimming a project's own.
        if project.has_external_artifacts() && !request.stale {
            bail!(
                "the build output in {} may be shared with other projects; pass --force to clean it anyway",
                project.artifact_dir.display()
            );
        }
    }
    let unit = Unit::single(project);
    let snapshot = engine.snapshot(ecosystem.as_ref(), &unit)?;
    let groups = match request.profile {
        None => snapshot.artifacts.groups.iter().collect(),
        Some(profile) => matching_groups(&snapshot.artifacts.groups, profile)?,
    };
    let actions: Vec<Action> = if request.stale {
        let policy = engine.policy_for(&unit);
        let now = SystemTime::now();
        groups
            .into_iter()
            .filter_map(|group| {
                let stale = stale_parts(group, policy.max_age_for(&group.profile), now);
                let bytes = stale.iter().map(|part| part.bytes).sum();
                (!stale.is_empty()).then(|| Action::Trim {
                    group: group.id.clone(),
                    reason: Reason::Stale {
                        parts: stale.len(),
                        bytes,
                    },
                    parts: stale.into_iter().map(|part| part.id.clone()).collect(),
                })
            })
            .collect()
    } else if request.profile.is_some() {
        if request.rebuild
            && let Some(group) = groups.iter().find(|group| !group.rebuildable)
        {
            bail!("purge cannot rebuild `{}`; drop --rebuild", group.id);
        }
        groups
            .into_iter()
            .map(|group| Action::CleanGroup {
                group: group.id.clone(),
                reason: Reason::Manual,
                rebuild: request.rebuild,
            })
            .collect()
    } else {
        vec![Action::CleanAll {
            reason: Reason::Manual,
        }]
    };
    if actions.is_empty() {
        println!("nothing stale in {}", output::display_path(unit.key()));
        return Ok(ExitCode::SUCCESS);
    }

    let options = RunOptions {
        dry_run: false,
        rebuild: true,
        max_rebuilds: usize::MAX,
    };
    let report = session
        .with_saved_state(|state| engine.apply(unit, snapshot, actions, state, &options))?
        .ok_or_else(|| anyhow!(RUN_IN_PROGRESS))?;
    output::print_report(&report);
    Ok(exit_code(&report))
}

/// The groups `profile` names, by id (`x86_64-pc-windows-gnu/release`) or
/// by profile (`release`, matching every target).
fn matching_groups<'a>(
    groups: &'a [ArtifactGroup],
    profile: &str,
) -> Result<Vec<&'a ArtifactGroup>> {
    let matching: Vec<&ArtifactGroup> = groups
        .iter()
        .filter(|group| group.id == profile || group.profile == profile)
        .collect();
    if matching.is_empty() {
        let found: Vec<&str> = groups.iter().map(|g| g.id.as_str()).collect();
        if found.is_empty() {
            bail!("no `{profile}` output purge can remove here; nothing else either");
        }
        bail!(
            "no `{profile}` output purge can remove here; it has: {}",
            found.join(", ")
        );
    }
    Ok(matching)
}

/// `purge status`. Works even when the configuration is broken, since that
/// is when people reach for it.
pub fn status(paths: &Paths) -> Result<()> {
    let config_note = if paths.config_file.exists() {
        ""
    } else {
        " (not created, using defaults)"
    };
    println!("config:   {}{config_note}", paths.config_file.display());
    println!("data:     {}", paths.data_dir.display());
    let session = match Session::open(paths.clone()) {
        Ok(session) => {
            let roots = &session.config().roots;
            if roots.is_empty() {
                println!("roots:    none");
            }
            for (i, root) in roots.iter().enumerate() {
                let label = if i == 0 { "roots:" } else { "" };
                println!("{label:<10}{}", root.display());
            }
            session
        }
        Err(err) => {
            let err = format!("{:#}", anyhow::Error::from(err));
            let first_line = err.lines().next().unwrap_or_default();
            println!("roots:    configuration error: {first_line}");
            Session::with_defaults(paths.clone())
        }
    };
    let service = purge_service::status().unwrap_or_else(|err| err.to_string());
    println!("service:  {service}");

    let last_run = session.load_state()?.last_run.map_or_else(
        || "never".to_owned(),
        |at| {
            let age = SystemTime::now().duration_since(at).unwrap_or_default();
            format!("{} ago", format_age(age))
        },
    );
    let running = if session.is_running()? {
        " (a run is in progress)"
    } else {
        ""
    };
    println!("last run: {last_run}{running}");

    let entries = session.history().read()?;
    let stats = Stats::compute(&entries, SystemTime::now(), 0);
    let failed = match stats.failures {
        0 => String::new(),
        n => format!(" ({n} failed)"),
    };
    println!(
        "freed:    {} in {}{failed}",
        format_bytes(stats.all_time.freed_bytes),
        output::count(stats.all_time.actions, "action")
    );
    if !entries.is_empty() {
        println!();
        output::print_history(last(&entries, STATUS_HISTORY));
    }
    Ok(())
}

/// `purge stats`.
pub fn stats(session: &Session, top: usize, json: bool) -> Result<()> {
    let entries = session.history().read()?;
    let stats = Stats::compute(&entries, SystemTime::now(), top);
    let saved = session.load_state()?;
    if json {
        let last_run = saved
            .last_run
            .and_then(|at| Timestamp::try_from(at).ok())
            .map(|at| at.to_string());
        let document = serde_json::json!({
            "stats": stats,
            "footprint": saved.footprint,
            "last_run": last_run,
        });
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else {
        output::print_stats(&stats, saved.footprint, saved.last_run);
    }
    Ok(())
}

/// `purge history`.
pub fn history(session: &Session, limit: usize) -> Result<()> {
    let entries = session.history().read()?;
    if entries.is_empty() {
        println!("nothing yet");
    }
    output::print_history(last(&entries, limit));
    Ok(())
}

/// `purge config ...`.
pub fn config(paths: &Paths, command: ConfigCommand) -> Result<()> {
    let file = &paths.config_file;
    match command {
        ConfigCommand::Path => println!("{}", file.display()),
        ConfigCommand::Show => match fs::read_to_string(file) {
            Ok(text) => print!("{text}"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                println!("# {} does not exist; these defaults apply", file.display());
                print!("{}", Config::template(&[]));
            }
            Err(err) => return Err(err).with_context(|| format!("cannot read {}", file.display())),
        },
        ConfigCommand::Init { roots, force } => {
            if file.exists() && !force {
                bail!(
                    "{} already exists (use --force to overwrite)",
                    file.display()
                );
            }
            let roots = roots
                .iter()
                .map(|root| {
                    let root = expand_tilde(root);
                    dunce::canonicalize(&root)
                        .with_context(|| format!("cannot open {}", root.display()))
                })
                .collect::<Result<Vec<_>>>()?;
            if let Some(dir) = file.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(file, Config::template(&roots))?;
            println!("wrote {}", file.display());
            if roots.is_empty() {
                println!("add folders to `roots` to choose what purge watches");
            } else {
                println!(
                    "next: `purge scan` to preview, `purge service install` to run in the background"
                );
            }
        }
    }
    Ok(())
}

/// `purge service ...`.
pub fn service(paths: &Paths, command: ServiceCommand) -> Result<()> {
    let message = match command {
        ServiceCommand::Install => {
            if Config::load(&paths.config_file)?.roots.is_empty() {
                eprintln!("warning: {NO_ROOTS}; the service idles until you add some");
            }
            purge_service::install(&service_spec(paths)?)?
        }
        ServiceCommand::Uninstall => purge_service::uninstall()?,
        ServiceCommand::Status => purge_service::status()?,
    };
    println!("{message}");
    Ok(())
}

/// Prefers the `purged` binary installed next to this one; falls back to
/// `purge daemon`.
fn service_spec(paths: &Paths) -> Result<purge_service::Spec> {
    let exe = env::current_exe().context("cannot locate the purge executable")?;
    let daemon = exe.with_file_name(format!("purged{}", env::consts::EXE_SUFFIX));
    let (program, args) = if daemon.is_file() {
        (daemon, Vec::new())
    } else {
        (exe, vec!["daemon".to_owned()])
    };
    let default_config = Paths::discover()?.config_file;
    if paths.config_file != default_config {
        eprintln!(
            "warning: the service reads the default configuration at {}",
            default_config.display()
        );
    }
    Ok(purge_service::Spec {
        program,
        args,
        path_env: env::var_os("PATH"),
    })
}

fn exit_code(report: &RunReport) -> ExitCode {
    if report.failures() > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn last<T>(items: &[T], count: usize) -> &[T] {
    &items[items.len().saturating_sub(count)..]
}
