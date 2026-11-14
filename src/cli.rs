//! Command-line interface. Machine-readable output contains only JSON.

use crate::view::{print_goal, print_list};
use crate::{
    model::*,
    runner::{self, CommandExecutor, Options},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use clap::{CommandFactory, Parser, Subcommand};
use std::{io::Write, path::PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "hgm",
    version,
    about = "Manage goals, evidence, and human/command work in Herdr"
)]
pub(crate) struct Cli {
    /// Workspace for this goal; Git is optional
    #[arg(long, global = true, env = "HGM_WORKSPACE", default_value = ".")]
    pub workspace: PathBuf,
    /// Private state root (default: XDG_STATE_HOME/herdr-goal-manager)
    #[arg(long, global = true, env = "HGM_STATE_DIR")]
    pub state_dir: Option<PathBuf>,
    /// Emit machine-readable JSON
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub action: Option<Action>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Action {
    /// Talk to an ordinary agent; this is the default entrypoint
    Manager {
        #[arg(long, value_enum)]
        agent: Option<crate::manager::Provider>,
        #[arg(long, env = "HGM_MODEL")]
        model: Option<String>,
    },
    /// Read or save the short conversation handoff without registering a goal
    Brief {
        #[arg(long)]
        save: Option<PathBuf>,
    },
    /// Everything a manager needs at startup: brief, what needs the user, goals
    Overview {
        #[arg(long)]
        all: bool,
    },
    /// Show configured overrides (user and project config) or the effective manager guide
    Config {
        /// Print the manager guide this build would send, including overrides
        #[arg(long)]
        guide: bool,
    },
    /// Show this tab's outcomes and the next useful action
    Status {
        /// Include goals registered from other tabs of this folder
        #[arg(long)]
        all: bool,
    },
    /// Show what happened: a goal's timeline, or this tab's goals merged
    Log {
        id: Option<String>,
        /// Only events after this time: RFC 3339, or a span such as 30m, 2h, 1d
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Inspect the pinned original pane, start an agent in its idle shell, or adopt a session the user restarted there
    Target {
        #[arg(long, value_enum)]
        start: Option<crate::manager::Provider>,
        /// Rebind to the agent session the user started in the pinned tab; EVIDENCE says why it continues the work
        #[arg(long, value_name = "EVIDENCE", conflicts_with = "start")]
        adopt: Option<String>,
    },
    /// Send an assignment to the original agent, preserving the selected target
    Send {
        text: String,
        /// Record the assignment on this goal and start monitoring it
        #[arg(long)]
        goal: Option<String>,
        /// Task of the goal that this assignment performs
        #[arg(long, requires = "goal")]
        task: Option<String>,
        /// Record on the goal without starting monitoring
        #[arg(long, requires = "goal")]
        no_supervise: bool,
        /// The user's words approving this assignment (needed for autonomy ask)
        #[arg(long, requires = "goal")]
        user_approval: Option<String>,
        /// Ask for a plan instead of implementation (needed first under loop.plan_first)
        #[arg(long, requires = "goal")]
        plan: bool,
        /// Deliver to a working agent that queues typed input (Claude Code), only
        /// with an empty input line and no dialog; others must settle first
        #[arg(long, requires = "goal")]
        interrupt: bool,
        /// Recipient: main (the original tab) or a pinned role such as impl
        #[arg(long, default_value = "main")]
        to: String,
    },
    /// Ask the worker to restate the goal in five lines (when a wake says a goal
    /// check is due); the reply re-anchors it and is not evidence
    Reanchor {
        id: String,
        /// Recipient: main or a pinned role
        #[arg(long, default_value = "main")]
        to: String,
        #[arg(long)]
        task: Option<String>,
        /// Optional short addition to the standard request
        text: Option<String>,
    },
    /// Steer the worker back to the contract: a short correction (150 words)
    /// that reaches a working Claude Code or Copilot if its input is empty;
    /// for a working Codex it is refused until the turn settles
    Correct {
        id: String,
        text: String,
        /// Recipient: main or a pinned role
        #[arg(long, default_value = "main")]
        to: String,
        /// Task being corrected (its correction budget is loop.max_corrections)
        #[arg(long)]
        task: Option<String>,
        /// The user's words approving it (needed for autonomy ask)
        #[arg(long)]
        user_approval: Option<String>,
    },
    /// Start, list or stop helper agents (impl, reviewer) pinned next to main
    Role {
        #[command(subcommand)]
        action: RoleAction,
    },
    /// Manage a goal's own checkout (a Herdr worktree workspace)
    Checkout {
        #[command(subcommand)]
        action: CheckoutAction,
    },
    /// Ask a reviewer role to judge the current result against the contract
    Review {
        id: String,
        /// What to review: files, diff command, the risky part (up to 400 words)
        text: String,
        #[arg(long, default_value = "reviewer")]
        reviewer: String,
    },
    /// (reviewer role) Submit the verdict for a review request
    ReviewSubmit {
        id: String,
        #[arg(long)]
        review: String,
        #[arg(long, value_enum)]
        verdict: crate::roles::Verdict,
        /// JSON array of findings, or - for stdin
        #[arg(long, default_value = "-")]
        findings: PathBuf,
    },
    /// (impl role) Report progress, completion or a question to the manager
    Report {
        id: String,
        text: String,
        #[arg(long)]
        task: Option<String>,
    },
    /// Record the user's agreement to a goal's loop contract before assigning work
    Confirm {
        id: String,
        #[arg(long)]
        evidence: String,
    },
    /// What needs a human now: reviews, questions, approvals, spent budgets
    /// Record a reviewed plan so implementation assignments may start (loop.plan_first)
    ApprovePlan {
        id: String,
        #[arg(long)]
        task: Option<String>,
        /// Who approved and what they checked
        #[arg(long)]
        evidence: String,
    },
    Inbox {
        #[arg(long)]
        all: bool,
    },
    /// Reject the result: reopen completed manual tasks with the human's feedback
    Revise {
        id: String,
        #[arg(long)]
        feedback: String,
        /// Reopen only these tasks (default: every completed manual task)
        #[arg(long)]
        task: Vec<String>,
    },
    /// Observe an active goal and wake the manager when work settles or stalls
    Supervise {
        id: String,
        /// Worker to observe: main or a pinned role
        #[arg(long, default_value = "main")]
        role: String,
        /// Seconds without change before a wake (default 300 or config watch.idle_secs)
        #[arg(long, value_parser=clap::value_parser!(u64).range(10..=86400))]
        idle_secs: Option<u64>,
        /// Seconds between wakes (default 300 or config watch.cooldown_secs)
        #[arg(long, value_parser=clap::value_parser!(u64).range(10..=86400))]
        cooldown_secs: Option<u64>,
        /// Wakes per goal (default 100 or config watch.max_wakes)
        #[arg(long, value_parser=clap::value_parser!(u32).range(1..=100))]
        max_wakes: Option<u32>,
    },
    /// Pause automatic wakeups while waiting for a human decision
    Wait {
        id: String,
        #[arg(long)]
        reason: String,
    },
    /// Ask the original agent for a durable handoff at a safe boundary
    Handoff,
    /// Advance a saved handoff through graceful restart and verified adoption
    Restart,
    /// Inspect the current handoff transfer
    HandoffStatus,
    /// Inspect persisted monitoring state
    Watch,
    #[command(hide = true)]
    WatchLoop {
        #[arg(long)]
        once: bool,
        /// Poll every 10 seconds instead of subscribing to Herdr events
        #[arg(long)]
        polling: bool,
        /// Stop after this many seconds (measurement runs)
        #[arg(long)]
        run_for_secs: Option<u64>,
    },
    #[command(hide = true)]
    HookStop,
    #[command(hide = true)]
    HookSession,
    /// Open the optional status board in a Herdr tab
    HerdrBoard,
    /// Write an editable goal template without overwriting existing files
    Init {
        #[arg(long, default_value = "goal.yaml")]
        output: PathBuf,
        #[arg(long)]
        command_example: bool,
    },
    /// Register a goal from YAML or a manual outcome and acceptance criteria
    Add {
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        objective: Option<String>,
        #[arg(long, conflicts_with_all = ["criterion", "check"])]
        file: Option<PathBuf>,
        #[arg(long, required_unless_present = "file")]
        criterion: Vec<String>,
        #[arg(long)]
        check: Vec<String>,
    },
    /// List this tab's goals in the selected workspace
    List {
        #[arg(long)]
        all: bool,
    },
    /// Inspect acceptance criteria, tasks, attempts, and verification evidence
    Show { id: String },
    /// Plan and run eligible work; stop for manual tasks or human acceptance
    Run(RunArgs),
    /// Continue unfinished work after inspecting partial effects
    Resume(RunArgs),
    /// Interrupt current work while retaining progress
    Pause { id: String },
    /// Permanently cancel a goal; retain its evidence and partial effects
    Cancel { id: String },
    /// Complete a human task with supporting evidence
    Complete {
        id: String,
        task: String,
        #[arg(long)]
        evidence: String,
    },
    /// Add guidance for the next planning, task, or repair attempt
    Note { id: String, text: String },
    /// Recheck the current outcome and accept it with human evidence
    Accept {
        id: String,
        #[arg(long)]
        evidence: String,
    },
    /// Open the interactive goal panel (also works outside Herdr)
    Panel,
    /// Open the Herdr plugin panel for the invoking workspace
    HerdrOpen,
    /// Make a Herdr plugin folder for this hgm (for an install from crates.io):
    /// writes herdr-plugin.toml and links target/release/hgm to this binary;
    /// then run `herdr plugin link FOLDER`
    PluginFolder {
        /// Folder to set up (default: $XDG_DATA_HOME/herdr-goal-manager/plugin,
        /// or ~/.local/share/herdr-goal-manager/plugin)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    #[command(hide = true)]
    HerdrRun,
}

#[derive(Debug, clap::Args)]
pub(crate) struct RunArgs {
    pub id: String,
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: u64,
    /// Lifetime limit per task and planning stage, including interrupted attempts
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub max_attempts: u32,
    /// Lifetime limit for automatic acceptance-check repairs
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(0..=100))]
    pub max_repairs: u32,
}

/// Goals of the current manager's original tab plus unowned ones; `all` lifts the filter.
/// Brief, what needs the user, this tab's goals, and revision lessons.
fn overview(store: &Store, all: bool, json: bool) -> Result<()> {
    let goals = visible_goals(store, all)?;
    let brief = store.brief(crate::supervision::origin_terminal(store)?.as_deref())?;
    // Rejected results are the raw material for project rules (every
    // repeated mistake should become a line in AGENTS.md).
    let mut lessons: Vec<&Evidence> = goals
        .iter()
        .flat_map(|goal| &goal.guidance)
        .filter(|g| g.note.starts_with("Revision requested: "))
        .collect();
    lessons.sort_by_key(|g| std::cmp::Reverse(g.recorded_at));
    let lessons: Vec<&str> = lessons.iter().take(5).map(|g| g.note.as_str()).collect();
    if json {
        let overview = serde_json::json!({
            "brief": brief,
            "needs_you": crate::view::inbox_rows(store, &goals)?,
            "goals": crate::view::status_rows(&goals),
            "lessons": lessons,
        });
        println!("{}", serde_json::to_string_pretty(&overview)?);
        return Ok(());
    }
    let brief = if brief.is_empty() { "(none)" } else { &brief };
    println!("# Brief\n{brief}\n\n# Needs you");
    crate::view::print_inbox(store, &goals, false)?;
    println!("\n# Goals");
    crate::view::print_status(&goals, false)?;
    if !lessons.is_empty() {
        println!("\n# Lessons (candidate AGENTS.md rules)");
        for lesson in lessons {
            println!("- {lesson}");
        }
    }
    Ok(())
}
fn visible_goals(store: &Store, all: bool) -> Result<Vec<Goal>> {
    let owner = if all {
        None
    } else {
        crate::supervision::origin_terminal(store)?
    };
    let mut goals = store.list()?;
    if let Some(owner) = owner {
        goals.retain(|goal| goal.owner.as_ref().is_none_or(|o| o == &owner));
    }
    Ok(goals)
}

/// Where `plugin-folder` puts the plugin by default: under the user's data
/// folder, next to nothing else hgm writes.
fn default_plugin_folder() -> Result<PathBuf> {
    if let Some(data) = std::env::var_os("XDG_DATA_HOME").filter(|d| PathBuf::from(d).is_absolute())
    {
        return Ok(PathBuf::from(data).join("herdr-goal-manager/plugin"));
    }
    let home = std::env::var_os("HOME").context("set HOME or pass --dir")?;
    Ok(PathBuf::from(home).join(".local/share/herdr-goal-manager/plugin"))
}
pub(crate) fn default_state_root() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        ensure!(
            PathBuf::from(&root).is_absolute(),
            "XDG_STATE_HOME must be absolute"
        );
        return Ok(PathBuf::from(root).join("herdr-goal-manager"));
    }
    let home = std::env::var_os("HOME").context("set HOME or pass --state-dir")?;
    Ok(PathBuf::from(home).join(".local/state/herdr-goal-manager"))
}

impl Cli {
    fn state_root(&self) -> Result<PathBuf> {
        Ok(match &self.state_dir {
            Some(path) if path.is_absolute() => path.clone(),
            Some(path) => std::env::current_dir()?.join(path),
            None => default_state_root()?,
        })
    }
    pub fn store(&self) -> Result<Store> {
        Store::new(&self.workspace, &self.state_root()?)
    }
    pub async fn execute(&self) -> Result<()> {
        let default_action = Action::Manager {
            agent: None,
            model: None,
        };
        let action = self.action.as_ref().unwrap_or(&default_action);
        if matches!(action, Action::HerdrOpen | Action::HerdrBoard) {
            let entrypoint = if matches!(action, Action::HerdrBoard) {
                "board"
            } else {
                "panel"
            };
            return crate::herdr::open_panel(entrypoint, &self.state_root()?).await;
        }
        if let Action::PluginFolder { dir } = action {
            let folder = match dir {
                Some(dir) if dir.is_absolute() => dir.clone(),
                Some(dir) => std::env::current_dir()?.join(dir),
                None => default_plugin_folder()?,
            };
            let result = crate::herdr::plugin_folder(&folder)?;
            if self.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "Herdr plugin folder: {}\nAdd it to Herdr with: {}",
                    folder.display(),
                    result["link"].as_str().unwrap_or_default()
                );
            }
            return Ok(());
        }
        let store = self.store()?;
        // Helper roles run hgm only to read the contract and report back.
        if let Ok(role) = std::env::var("HGM_ROLE") {
            ensure!(
                matches!(
                    action,
                    Action::Show { .. } | Action::Report { .. } | Action::ReviewSubmit { .. }
                ),
                "the {role} role may only run show, report or review-submit"
            );
        }
        match action {
            Action::Manager { agent, model } => {
                crate::manager::launch(&store, *agent, model.as_deref(), &command_reference())
                    .await?
            }
            Action::Brief { save } => {
                let scope = crate::supervision::origin_terminal(&store)?;
                let brief = if let Some(file) = save {
                    crate::manager::save_brief(&store, scope.as_deref(), file)?
                } else {
                    store.brief(scope.as_deref())?
                };
                if self.json {
                    println!("{}", serde_json::json!({"brief":brief}));
                } else {
                    println!("{brief}");
                }
            }
            Action::Overview { all } => overview(&store, *all, self.json)?,
            Action::Config { guide } => {
                let config = crate::config::load(&store.workspace)?;
                if *guide {
                    println!("{}", config.guide());
                } else {
                    let mut value = serde_json::to_value(&config)?;
                    value["user_config"] = serde_json::json!(crate::config::user_path());
                    value["project_config"] =
                        serde_json::json!(crate::config::project_path(&store.workspace));
                    value["guide_replaced"] = config.guide.is_some().into();
                    value["guide_append"] = serde_json::json!(
                        config
                            .guide_append
                            .iter()
                            .map(|(p, _)| p)
                            .collect::<Vec<_>>()
                    );
                    println!("{}", serde_json::to_string_pretty(&value)?);
                }
            }
            Action::Status { all } => {
                crate::view::print_status(&visible_goals(&store, *all)?, self.json)?
            }
            Action::Log { id, since, all } => {
                let since = since.as_deref().map(crate::events::since).transpose()?;
                let mut events = match id {
                    Some(id) => store.events(id)?,
                    None => visible_goals(&store, *all)?
                        .iter()
                        .map(|goal| store.events(&goal.id))
                        .collect::<Result<Vec<_>>>()?
                        .concat(),
                };
                events.retain(|event| since.is_none_or(|since| event.at > since));
                events.sort_by_key(|event| event.at);
                if self.json {
                    println!("{}", serde_json::to_string_pretty(&events)?);
                } else {
                    for event in &events {
                        println!("{}", crate::events::describe(event));
                    }
                }
            }
            Action::Confirm { id, evidence } => {
                let mut goal = store.load(id)?;
                // A contract that cannot be confirmed creates no checkout.
                goal.clone().confirm(evidence.clone(), String::new())?;
                if goal.spec.checkout.is_some() && goal.checkout.is_none() {
                    goal.checkout = Some(crate::checkout::prepare(&goal).await?);
                }
                let digest = crate::contract::protected_digest(
                    goal.work_dir()?,
                    &goal.spec.protected_paths,
                )?;
                let goal = store.update(id, |g| {
                    if g.checkout.is_none() {
                        g.checkout = goal.checkout.clone();
                    }
                    g.confirm(evidence.clone(), digest)
                })?;
                print_goal(&goal, &store, self.json)?;
            }
            Action::Checkout {
                action: CheckoutAction::Remove { id },
            } => print_goal(
                &crate::checkout::remove(&store, id).await?,
                &store,
                self.json,
            )?,
            Action::ApprovePlan { id, task, evidence } => {
                ensure!(
                    !evidence.trim().is_empty(),
                    "plan approval must quote the reviewer"
                );
                let goal = store.load(id)?;
                ensure!(
                    task.as_deref()
                        .is_none_or(|t| goal.tasks.iter().any(|x| x.spec.id == t)),
                    "unknown task ID"
                );
                store.record(
                    id,
                    "plan_approved",
                    serde_json::json!({"task": task, "evidence": evidence}),
                )?;
                print_goal(&store.load(id)?, &store, self.json)?;
            }
            Action::Inbox { all } => {
                crate::view::print_inbox(&store, &visible_goals(&store, *all)?, self.json)?
            }
            Action::Revise { id, feedback, task } => {
                let mut reopened = vec![];
                let goal = store.update(id, |g| {
                    reopened = g.revise(feedback.clone(), task)?;
                    Ok(())
                })?;
                print_goal(&goal, &store, self.json)?;
                if !self.json {
                    println!("Reopened: {}", reopened.join(", "));
                }
            }
            Action::Target { start, adopt } => println!(
                "{}",
                serde_json::to_string_pretty(
                    &crate::supervision::target(&store, start.map(|p| p.name()), adopt.as_deref())
                        .await?
                )?
            ),
            Action::Send {
                text,
                goal,
                task,
                no_supervise,
                user_approval,
                plan,
                interrupt,
                to,
            } => {
                let helper = crate::roles::resolve(&store, to)?;
                let assignment = crate::supervision::Assignment {
                    goal: goal.as_deref(),
                    task: task.as_deref(),
                    supervise: !no_supervise,
                    user_approval: user_approval.as_deref(),
                    plan: *plan,
                    to,
                    kind: crate::supervision::Delivery::Assignment,
                    interrupt: *interrupt,
                };
                let result =
                    crate::supervision::send(&store, text, assignment, helper.as_ref()).await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            Action::Correct {
                id,
                text,
                to,
                task,
                user_approval,
            } => {
                let helper = crate::roles::resolve(&store, to)?;
                let assignment = crate::supervision::Assignment {
                    goal: Some(id),
                    task: task.as_deref(),
                    supervise: true,
                    user_approval: user_approval.as_deref(),
                    plan: false,
                    to,
                    kind: crate::supervision::Delivery::Correction,
                    interrupt: true,
                };
                let result =
                    crate::supervision::send(&store, text, assignment, helper.as_ref()).await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            Action::Reanchor { id, to, task, text } => {
                let helper = crate::roles::resolve(&store, to)?;
                let assignment = crate::supervision::Assignment {
                    goal: Some(id),
                    task: task.as_deref(),
                    supervise: true,
                    user_approval: None,
                    plan: false,
                    to,
                    kind: crate::supervision::Delivery::Reanchor,
                    interrupt: true,
                };
                let text = text.as_deref().unwrap_or(" ");
                let result =
                    crate::supervision::send(&store, text, assignment, helper.as_ref()).await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            Action::Role { action } => {
                let result = match action {
                    RoleAction::Start {
                        kind,
                        name,
                        agent,
                        model,
                        cwd,
                    } => {
                        crate::roles::start(
                            &store,
                            *kind,
                            name.as_deref(),
                            *agent,
                            model.as_deref(),
                            cwd.as_deref(),
                        )
                        .await?
                    }
                    RoleAction::List => crate::roles::list(&store).await?,
                    RoleAction::Stop { name, force } => {
                        crate::roles::stop(&store, name, *force).await?
                    }
                };
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            Action::Review { id, text, reviewer } => println!(
                "{}",
                serde_json::to_string_pretty(
                    &crate::roles::request_review(&store, id, reviewer, text).await?
                )?
            ),
            Action::ReviewSubmit {
                id,
                review,
                verdict,
                findings,
            } => {
                let mut text = String::new();
                if findings == std::path::Path::new("-") {
                    use std::io::Read;
                    std::io::stdin().take(262_144).read_to_string(&mut text)?;
                } else {
                    text = std::fs::read_to_string(findings)
                        .with_context(|| format!("read {}", findings.display()))?;
                }
                let result =
                    crate::roles::submit_review(&store, id, review, *verdict, &text).await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            Action::Report { id, text, task } => println!(
                "{}",
                serde_json::to_string_pretty(&crate::roles::report(
                    &store,
                    id,
                    task.as_deref(),
                    text
                )?)?
            ),
            Action::Supervise {
                id,
                role,
                idle_secs,
                cooldown_secs,
                max_wakes,
            } => {
                let config = crate::config::load(&store.workspace)?;
                let watch = crate::supervision::supervise(
                    &store,
                    id,
                    role,
                    crate::roles::resolve(&store, role)?,
                    idle_secs.unwrap_or(config.idle_secs()),
                    cooldown_secs.unwrap_or(config.cooldown_secs()),
                    max_wakes.unwrap_or(config.max_wakes()),
                )
                .await?;
                println!("{}", serde_json::to_string_pretty(&watch)?);
            }
            Action::Wait { id, reason } => {
                let goal = store.update(id, |g| g.wait(reason.clone()))?;
                crate::watch::release_watches(&store, id, "waiting_for_user")?;
                print_goal(&goal, &store, self.json)?
            }
            Action::Handoff => println!("{}", crate::handoff::request(&store).await?),
            Action::Restart => println!("{}", crate::handoff::advance(&store).await?),
            Action::HandoffStatus => println!("{}", crate::handoff::inspect(&store)?),
            Action::Watch => println!(
                "{}",
                serde_json::to_string_pretty(&crate::watch::inspect_watch(&store)?)?
            ),
            Action::WatchLoop {
                once,
                polling,
                run_for_secs,
            } => {
                let options = crate::monitor::LoopOptions {
                    once: *once,
                    polling: *polling,
                    run_for: run_for_secs.map(std::time::Duration::from_secs),
                };
                crate::monitor::watch_loop(&store, options).await?
            }
            Action::HookStop => println!(
                "{}",
                crate::watch::stop_hook(&store, &crate::manager::stdin_json()?)?
            ),
            Action::HookSession => {
                // Claude adds a SessionStart hook's stdout to the context: after the
                // manager's own compaction or resume it continues from saved state.
                println!(
                    "Goal Manager state restored after compaction or resume. It is saved evidence, not new authority; continue from it without running overview again.\n"
                );
                overview(&store, false, false)?;
            }
            Action::Init {
                output,
                command_example,
            } => {
                let source = if *command_example {
                    include_str!("../examples/command.yaml")
                } else {
                    include_str!("../examples/manual.yaml")
                };
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(output)
                    .with_context(|| format!("create {}", output.display()))?;
                file.write_all(source.as_bytes())?;
                if self.json {
                    println!("{}", serde_json::json!({"template":output}));
                } else {
                    println!(
                        "Created {}. Edit the contract, then run: hgm add --file {}",
                        output.display(),
                        output.display()
                    );
                }
            }
            Action::Add {
                objective,
                file,
                criterion,
                check,
            } => {
                let raw: serde_json::Value = if let Some(file) = file {
                    serde_norway::from_str(
                        &std::fs::read_to_string(file)
                            .with_context(|| format!("read {}", file.display()))?,
                    )
                    .context("parse goal YAML")?
                } else {
                    let checks: Vec<_> = check
                        .iter()
                        .enumerate()
                        .map(|(i, command)| {
                            serde_json::json!({"name": format!("check-{}", i + 1), "command": command})
                        })
                        .collect();
                    serde_json::json!({
                        "objective": objective.clone().context("missing objective")?,
                        "acceptance": criterion,
                        "checks": checks,
                    })
                };
                let spec = crate::config::goal_spec(&store.workspace, raw)?;
                let owner = crate::supervision::origin_terminal(&store)?;
                let goal = store.add(spec, owner)?;
                print_goal(&goal, &store, self.json)?;
            }
            Action::List { all } => {
                let goals = visible_goals(&store, *all)?;
                if self.json {
                    println!("{}", serde_json::to_string_pretty(&goals)?);
                } else {
                    print_list(&goals);
                }
            }
            Action::Show { id } => print_goal(&store.load(id)?, &store, self.json)?,
            Action::Pause { id } => {
                let goal = store.update(id, |g| g.request(Control::Pause))?;
                crate::watch::release_watches(&store, id, "stopped_by_goal")?;
                print_goal(&goal, &store, self.json)?
            }
            Action::Cancel { id } => {
                let goal = store.update(id, |g| g.request(Control::Cancel))?;
                crate::watch::release_watches(&store, id, "stopped_by_goal")?;
                print_goal(&goal, &store, self.json)?
            }
            Action::Note { id, text } => print_goal(
                &store.update(id, |g| g.note(text.clone()))?,
                &store,
                self.json,
            )?,
            Action::Complete { id, task, evidence } => print_goal(
                &store.update(id, |g| g.complete_manual(task, evidence.clone()))?,
                &store,
                self.json,
            )?,
            Action::Accept { id, evidence } => {
                let goal = runner::accept(&store, id, evidence, &CommandExecutor).await?;
                print_goal(&goal, &store, self.json)?;
                ensure!(
                    goal.status == GoalStatus::Completed,
                    "goal was not accepted: {:?}",
                    goal.status
                );
            }
            Action::Run(args) | Action::Resume(args) => {
                let result = runner::run(
                    &store,
                    &args.id,
                    &CommandExecutor,
                    Options {
                        resume: matches!(action, Action::Resume(_)),
                        timeout_secs: args.timeout,
                        max_attempts: args.max_attempts as usize,
                        max_repairs: args.max_repairs as usize,
                    },
                )
                .await;
                match result {
                    Ok(goal) => {
                        print_goal(&goal, &store, self.json)?;
                        ensure!(
                            goal.status == GoalStatus::ReadyForReview,
                            "goal needs attention: {:?}; inspect with hgm show {}",
                            goal.status,
                            goal.id
                        );
                    }
                    Err(error) => {
                        if let Ok(goal) = store.load(&args.id) {
                            print_goal(&goal, &store, self.json)?;
                        }
                        return Err(error);
                    }
                }
            }
            Action::Panel => crate::herdr::panel(&store).await?,
            Action::HerdrRun => crate::herdr::run_pane(&store).await?,
            Action::HerdrOpen | Action::HerdrBoard | Action::PluginFolder { .. } => {
                unreachable!("handled before workspace resolution")
            }
        }
        Ok(())
    }
}

/// Manager-facing commands with exact arguments, generated from this build's
/// CLI so the agent acts directly instead of rediscovering them with --help.
pub(crate) fn command_reference() -> String {
    const NOT_FOR_MANAGER: [&str; 5] =
        ["manager", "panel", "herdr-open", "herdr-board", "herdr-run"];
    let mut lines = vec![
        "## hgm command reference".to_string(),
        "Complete for this build. Run these directly; do not run --help to rediscover them. Add --json for machine-readable output.".into(),
    ];
    for command in Cli::command().get_subcommands() {
        let name = command.get_name();
        if command.is_hide_set() || NOT_FOR_MANAGER.contains(&name) {
            continue;
        }
        let mut usage = format!("hgm {name}");
        for arg in command.get_arguments() {
            if arg.is_global_set() || arg.is_hide_set() {
                continue;
            }
            let value = arg
                .get_value_names()
                .and_then(|names| names.first())
                .map(ToString::to_string)
                .unwrap_or_else(|| arg.get_id().as_str().to_uppercase());
            let mut piece = match arg.get_long() {
                None => format!("<{value}>"),
                Some(long) if arg.get_action().takes_values() => format!("--{long} <{value}>"),
                Some(long) => format!("--{long}"),
            };
            if matches!(arg.get_action(), clap::ArgAction::Append) {
                piece.push_str("...");
            }
            usage.push(' ');
            usage.push_str(&if arg.is_required_set() {
                piece
            } else {
                format!("[{piece}]")
            });
        }
        let about = command
            .get_about()
            .map(ToString::to_string)
            .unwrap_or_default();
        lines.push(format!("- `{usage}` — {about}"));
    }
    lines.join("\n")
}

#[derive(Debug, Subcommand)]
pub(crate) enum CheckoutAction {
    /// Remove the checkout; refused with uncommitted changes, a running watch
    /// or a running goal. The branch stays.
    Remove { id: String },
}

#[derive(Debug, Subcommand)]
pub(crate) enum RoleAction {
    /// Start a helper in a new unfocused tab of this workspace and pin it
    Start {
        #[arg(value_enum)]
        kind: crate::roles::RoleKind,
        /// Role name (default: impl or reviewer)
        #[arg(long)]
        name: Option<String>,
        /// Provider; defaults to main's (prefer another one for reviewers)
        #[arg(long, value_enum)]
        agent: Option<crate::manager::Provider>,
        #[arg(long)]
        model: Option<String>,
        /// Folder the helper works in (default: this workspace)
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Pinned roles and their live state
    List,
    /// Close a role's tab and unpin it
    Stop {
        name: String,
        #[arg(long)]
        force: bool,
    },
}
