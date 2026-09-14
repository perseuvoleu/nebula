mod config;
mod files;
mod ingress;
mod runtime;

use anyhow::{ensure, Context, Result};
use clap::{Args as ClapArgs, Subcommand};
use config::{Policy, Recipe};
use ingress::Published;
use runtime::{Runtime, Target};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Host-local preview lifecycle and policy selection.
#[derive(ClapArgs)]
pub struct Args {
    /// Root-provisioned host policy. Custom policies require --offline.
    #[arg(long, default_value = ingress::POLICY)]
    policy: PathBuf,
    /// Write isolated route artifacts; never invoke the installed publisher.
    #[arg(long)]
    offline: bool,
    /// Policy project name; otherwise infer from the current Git common directory.
    #[arg(long)]
    project: Option<String>,
    #[command(subcommand)]
    command: Action,
}

#[derive(ClapArgs)]
struct Ownership {
    /// Calling agent identity; pass the stable Nebula agent ID explicitly.
    #[arg(long)]
    owner: String,
    /// Explicitly take ownership from another agent or checkout.
    #[arg(long)]
    replace: bool,
}

#[derive(Subcommand)]
enum Action {
    /// Build and health-check a candidate, then atomically promote it.
    Up {
        #[command(flatten)]
        ownership: Ownership,
        /// Worktree to snapshot. Never switches or resets its branch.
        #[arg(long, default_value = ".")]
        checkout: PathBuf,
    },
    /// Refresh the recorded checkout into a new supervised candidate.
    Restart {
        #[command(flatten)]
        ownership: Ownership,
    },
    /// Report recorded identity and observed container health as JSON.
    Status,
    /// Read bounded managed-service logs (may contain application output).
    Logs {
        /// Inspect the last failed or interrupted candidate instead of the active slot.
        #[arg(long)]
        candidate: bool,
        /// Recipe service name or gate.
        #[arg(long, default_value = "gate")]
        service: String,
        /// Maximum log lines, capped at 1000 and 128 KiB total output.
        #[arg(long, default_value_t = 100)]
        lines: u16,
    },
    /// Withdraw this route and stop only its managed containers; preserve data.
    Down {
        #[command(flatten)]
        ownership: Ownership,
    },
    /// Privileged publisher entry point; used only by the installed helper.
    #[command(hide = true)]
    Publish {
        name: String,
        operation: String,
        #[arg(default_value = "")]
        generation: String,
    },
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    active: Option<Target>,
    candidate: Option<Target>,
    previous: Option<Target>,
    stopping: bool,
    publishing: bool,
}

fn save(directory: &Path, state: &State) -> Result<()> {
    files::atomic(
        &directory.join("state.json"),
        &serde_json::to_vec_pretty(state)?,
    )
}

fn published(target: &Target) -> Published {
    Published {
        slot: target.slot.clone(),
        generation: target.generation.clone(),
    }
}

fn reconcile(runtime: &Runtime<'_>, state: &mut State, offline: bool) -> Result<()> {
    if !state.stopping && state.candidate.is_none() {
        return Ok(());
    }
    if state.stopping {
        ingress::set(
            &runtime.directory,
            runtime.name,
            offline,
            None,
            runtime.lock,
        )?;
        for target in [&state.candidate, &state.active, &state.previous]
            .into_iter()
            .flatten()
        {
            runtime.stop(target)?;
        }
        *state = State::default();
        save(&runtime.directory, state)?;
    } else if let Some(candidate) = &state.candidate {
        let current = if state.publishing {
            ingress::current(&runtime.directory, runtime.name, offline, runtime.lock)?
        } else {
            None
        };
        if current.as_ref() == Some(&published(candidate)) {
            // Publication is the transaction commit point. Keep the old slot for
            // existing connections until a later up/down explicitly reclaims it.
            state.previous = state.active.take();
            state.active = state.candidate.take();
        } else {
            runtime.stop(candidate)?;
            state.candidate = None;
        }
        state.publishing = false;
        save(&runtime.directory, state)?;
    }
    Ok(())
}

fn authorize(state: &State, ownership: &Ownership, checkout: Option<&Path>) -> Result<()> {
    for active in [&state.active, &state.candidate].into_iter().flatten() {
        ensure!(
            ownership.replace
                || (active.owner == ownership.owner
                    && checkout.is_none_or(|path| path == active.checkout)),
            "preview belongs to another owner or checkout; use --replace for explicit takeover"
        );
    }
    Ok(())
}

fn promote(
    runtime: &Runtime<'_>,
    state: &mut State,
    ownership: &Ownership,
    checkout: &Path,
    offline: bool,
) -> Result<()> {
    let checkout = fs::canonicalize(runtime::git(checkout, &["rev-parse", "--show-toplevel"])?)?;
    authorize(state, ownership, Some(&checkout))?;
    let recipe = Recipe::load(&checkout)?;
    let slot = if state
        .active
        .as_ref()
        .is_some_and(|target| target.slot == "a")
    {
        "b"
    } else {
        "a"
    };
    let target = Target::identify(&checkout, &ownership.owner, slot, &recipe)?;
    ensure!(
        target.git_common_dir == fs::canonicalize(&runtime.project.git_common_dir)?,
        "checkout does not belong to the allowlisted project"
    );
    // The inactive slot's data is independent from the active slot. Retire the
    // previous runtime only when a new candidate actually needs that slot.
    if let Some(previous) = &state.previous {
        runtime.stop(previous)?;
    }
    state.previous = None;
    state.candidate = Some(target.clone());
    save(&runtime.directory, state)?;
    // Retain failed startup output for `logs --candidate`; the next mutation
    // reclaims it. No publication has occurred in this stage.
    runtime.start(&target, &recipe)?;
    state.publishing = true;
    save(&runtime.directory, state)?;
    if let Err(error) = ingress::set(
        &runtime.directory,
        runtime.name,
        offline,
        Some(&published(&target)),
        runtime.lock,
    ) {
        // Keep the journal until cleanup succeeds, so a second command can
        // recover after Docker downtime or an interrupted operation.
        let current = ingress::current(&runtime.directory, runtime.name, offline, runtime.lock)?;
        if current.as_ref() == Some(&published(&target)) {
            ingress::set(
                &runtime.directory,
                runtime.name,
                offline,
                state.active.as_ref().map(published).as_ref(),
                runtime.lock,
            )?;
        }
        runtime.stop(&target)?;
        state.candidate = None;
        state.publishing = false;
        save(&runtime.directory, state)?;
        return Err(error);
    }
    state.previous = state.active.take();
    state.active = state.candidate.take();
    state.publishing = false;
    save(&runtime.directory, state)?;
    Ok(())
}

/// Execute a preview operation independently of the Nebula daemon.
pub fn run(args: Args) -> Result<()> {
    if let Action::Publish {
        name,
        operation,
        generation,
    } = &args.command
    {
        ensure!(
            !args.offline && args.policy == Path::new(ingress::POLICY) && args.project.is_none(),
            "publisher accepts no policy or path overrides"
        );
        return ingress::publish(name, operation, generation);
    }
    ensure!(
        cfg!(target_os = "linux"),
        "preview runtime currently requires Linux"
    );
    ensure!(
        args.offline || args.policy == Path::new(ingress::POLICY),
        "custom policy requires --offline"
    );
    let policy = Policy::load(&args.policy, args.offline)?;
    let name = match args.project {
        Some(name) => name,
        None => {
            let cwd = std::env::current_dir()?;
            let common = fs::canonicalize(runtime::git(
                &cwd,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )?)?;
            let names: Vec<_> = policy
                .projects
                .iter()
                .filter(|(_, project)| {
                    fs::canonicalize(&project.git_common_dir).ok().as_ref() == Some(&common)
                })
                .map(|(name, _)| name.clone())
                .collect();
            ensure!(
                names.len() == 1,
                "current checkout does not identify exactly one policy project; use --project"
            );
            names[0].clone()
        }
    };
    config::identifier(&name)?;
    let project = policy
        .projects
        .get(&name)
        .context("project is not allowlisted")?;
    files::directory(&policy.state_dir)?;
    let mode_dir = policy
        .state_dir
        .join(if args.offline { "offline" } else { "live" });
    files::directory(&mode_dir)?;
    let mode_dir = mode_dir.join(&policy.namespace);
    files::directory(&mode_dir)?;
    let _lock = files::lock(&mode_dir.join(format!("{name}.lock")))?;
    let directory = mode_dir.join(&name);
    files::directory(&directory)?;
    let runtime = Runtime {
        policy: &policy,
        project,
        name: &name,
        offline: args.offline,
        lock: &_lock,
        directory,
    };
    let state_file = runtime.directory.join("state.json");
    let mut state: State = if state_file.exists() {
        serde_json::from_slice(&files::read_private(&state_file, false)?)?
    } else {
        State::default()
    };
    for target in [&state.active, &state.candidate, &state.previous]
        .into_iter()
        .flatten()
    {
        ingress::validate_published(&published(target))?;
        for service in &target.services {
            config::identifier(service)?;
            ensure!(service != "gate", "invalid state service");
        }
        ensure!(
            target.git_common_dir == project.git_common_dir,
            "state project identity differs from policy"
        );
    }
    // Read-only status observes pending work without silently claiming ownership
    // or mutating a route. Mutations reconcile under the same project lock.
    match &args.command {
        Action::Up { ownership, .. } | Action::Restart { ownership } => {
            authorize(&state, ownership, None)?;
            reconcile(&runtime, &mut state, args.offline)?;
        }
        Action::Down { ownership } => authorize(&state, ownership, None)?,
        _ => (),
    }
    let observing = matches!(args.command, Action::Status);
    match args.command {
        Action::Up {
            ownership,
            checkout,
        } => promote(&runtime, &mut state, &ownership, &checkout, args.offline)?,
        Action::Restart { ownership } => {
            let checkout = state
                .active
                .as_ref()
                .context("no preview to restart; use up")?
                .checkout
                .clone();
            promote(&runtime, &mut state, &ownership, &checkout, args.offline)?;
        }
        Action::Down { ownership } => {
            authorize(&state, &ownership, None)?;
            state.stopping = true;
            save(&runtime.directory, &state)?;
            reconcile(&runtime, &mut state, args.offline)?;
        }
        Action::Logs {
            service,
            lines,
            candidate,
        } => {
            return runtime.logs(
                if candidate {
                    state.candidate.as_ref().context("no candidate preview")?
                } else {
                    state.active.as_ref().context("no active preview")?
                },
                &service,
                lines,
            )
        }
        Action::Status => (),
        Action::Publish { .. } => unreachable!(),
    }
    let mut observation_error = None;
    let current = match ingress::current(&runtime.directory, &name, args.offline, runtime.lock) {
        Ok(current) => current,
        Err(error) if observing => {
            observation_error = Some(format!("route observation failed: {error:#}"));
            None
        }
        Err(error) => return Err(error),
    };
    let running = if let Some(target) = &state.active {
        match runtime.running(target) {
            Ok(running) => running,
            Err(error) if observing => {
                observation_error = Some(format!("runtime observation failed: {error:#}"));
                false
            }
            Err(error) => return Err(error),
        }
    } else {
        false
    };
    let phase = if state.stopping || state.candidate.is_some() {
        "recovering"
    } else if let Some(target) = &state.active {
        if current.as_ref() == Some(&published(target)) && running && observation_error.is_none() {
            "running"
        } else {
            "stale"
        }
    } else if current.is_some() {
        "orphaned_route"
    } else if observation_error.is_some() {
        "unknown"
    } else {
        "stopped"
    };
    println!(
        "{}",
        serde_json::json!({"project": name, "domain": project.domain, "phase": phase, "offline": args.offline, "active": state.active.as_ref().map(runtime::status_json), "candidate": state.candidate.as_ref().map(runtime::status_json), "previous": state.previous.as_ref().map(runtime::status_json), "published": current, "observation_error": observation_error})
    );
    Ok(())
}
