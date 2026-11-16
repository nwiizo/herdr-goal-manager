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
    /// add, confirm, complete, run and resume print the whole goal, not the short result
    #[arg(long, global = true)]
    pub verbose: bool,
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
    /// Attention accounting per goal, from the timelines: wakes and how many
    /// led to an action, idle time, notices, review wait, corrections, refusals
    Attention {
        /// Include goals registered from other tabs of this folder
        #[arg(long)]
        all: bool,
        /// Include completed and cancelled goals (always included in --json)
        #[arg(long)]
        finished: bool,
    },
    /// Show this tab's outcomes and the next useful action
    Status {
        /// Include goals registered from other tabs of this folder
        #[arg(long)]
        all: bool,
        /// Include completed and cancelled goals (always included in --json)
        #[arg(long)]
        finished: bool,
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
        /// Return up to this many recent lines from Herdr's scrollback (1-5000)
        /// instead of the visible screen
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=5000))]
        lines: Option<u32>,
    },
    /// Send an assignment to the original agent, preserving the selected target
    Send {
        #[arg(required_unless_present = "withdraw")]
        text: Option<String>,
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
        /// Queue it as a next step instead: the watch delivers one each time the
        /// recipient settles, in order (up to 5 per role)
        #[arg(long, requires = "goal", conflicts_with = "interrupt")]
        prepare: bool,
        /// Withdraw the steps prepared for the recipient (--to): all, or one with --step
        #[arg(long, conflicts_with_all = ["text", "goal", "prepare"])]
        withdraw: bool,
        /// With --withdraw: the place of the one step to withdraw (1 is next)
        #[arg(long, requires = "withdraw", value_parser = clap::value_parser!(u32).range(1..))]
        step: Option<u32>,
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
    /// (worker) Report progress, completion or a question to the manager, as
    /// text or a file; kept in full on the timeline, in log and the packet
    Report {
        id: String,
        /// The report (up to 3,000 words), or use --file
        #[arg(required_unless_present = "file")]
        text: Option<String>,
        /// Read the report from this file instead
        #[arg(long, conflicts_with = "text")]
        file: Option<PathBuf>,
        #[arg(long)]
        task: Option<String>,
    },
    /// (worker) Record a choice made inside the contract and why, on the goal's
    /// timeline; it completes nothing
    Decision {
        id: String,
        /// What was chosen (up to 150 words)
        text: String,
        /// Why (up to 150 words)
        #[arg(long)]
        why: String,
        #[arg(long)]
        task: Option<String>,
    },
    /// (worker) Record a finding that should not block the work, to take up later
    FollowUp {
        id: String,
        /// The finding (up to 150 words)
        text: String,
        #[arg(long)]
        task: Option<String>,
    },
    /// One summary for review: acceptance, latest checks, counts, decisions,
    /// follow-ups, verdicts and what is not verified; it accepts nothing
    Packet { id: String },
    /// Record the user's agreement to a goal's loop contract before assigning work
    Confirm {
        id: String,
        #[arg(long)]
        evidence: String,
    },
    /// Record a reviewed plan so implementation assignments may start
    /// (loop.plan_first); without --task, the one task whose plan is pending
    ApprovePlan {
        id: String,
        #[arg(long)]
        task: Option<String>,
        /// Who approved and what they checked
        #[arg(long)]
        evidence: String,
    },
    /// What needs a human now: reviews, questions, approvals, spent budgets, idle workers
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
        /// Seconds without change before a wake (default 180 or config watch.idle_secs)
        #[arg(long, value_parser=clap::value_parser!(u64).range(10..=86400))]
        idle_secs: Option<u64>,
        /// Seconds between patrol and unchanged-screen wakes; a stop after new
        /// work skips it (default 180 or config watch.cooldown_secs)
        #[arg(long, value_parser=clap::value_parser!(u64).range(10..=86400))]
        cooldown_secs: Option<u64>,
        /// Wakes per goal (default 160 or config watch.max_wakes)
        #[arg(long, value_parser=clap::value_parser!(u32).range(1..=500))]
        max_wakes: Option<u32>,
        /// Seconds the worker may sit settled short of completion before you
        /// are told (default 180 or config watch.idle_notice_secs)
        #[arg(long, value_parser=clap::value_parser!(u64).range(10..=86400))]
        idle_notice_secs: Option<u64>,
        /// Minutes between patrols of a working agent, overriding the goal's
        /// loop.patrol_minutes for this watch; 0 disables
        #[arg(long, value_parser=clap::value_parser!(u32).range(0..=1440))]
        patrol_minutes: Option<u32>,
    },
    /// Pause automatic wakeups while waiting for a human decision
    Wait {
        id: String,
        #[arg(long)]
        reason: String,
    },
    /// (Goal Manager) Wait for this manager's next wake. Run it in the
    /// background at the end of a turn: the watch hands the wake to it instead
    /// of typing it, and it prints the wake, or why none came, and exits
    Await {
        /// Seconds to wait before it returns without a wake
        #[arg(long, default_value_t = crate::wake_await::DEFAULT_TIMEOUT_SECS, value_parser=clap::value_parser!(u64).range(1..=86400))]
        timeout: u64,
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
        /// Report the contract's problems and words per field without registering it
        #[arg(long, conflicts_with = "replace")]
        validate: bool,
        /// Replace the contract of this unconfirmed goal instead of registering
        /// a new one (same id and timeline; a confirmed goal is never replaced)
        #[arg(long, value_name = "GOAL")]
        replace: Option<String>,
    },
    /// List this tab's goals in the selected workspace
    List {
        #[arg(long)]
        all: bool,
        /// Include completed and cancelled goals (always included in --json)
        #[arg(long)]
        finished: bool,
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
    /// Factory mode work queue of agreed contracts (needs factory.enabled)
    Queue {
        #[command(subcommand)]
        action: QueueAction,
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
    let factory = crate::factory::Factory::load(store)?;
    let needs_you = inbox_rows(store, &goals, factory.as_ref())?;
    let rows = status_rows(store, &goals, factory.as_ref())?;
    // Queued work behind an open gate waits for the manager, not the person.
    let queue = match &factory {
        Some(factory) => factory.ready_line()?,
        None => None,
    };
    if json {
        let mut overview = serde_json::json!({
            "brief": brief,
            "needs_you": needs_you,
            "goals": rows,
            "lessons": lessons,
        });
        if let Some(queue) = queue {
            overview["queue"] = queue.into();
        }
        println!("{}", serde_json::to_string_pretty(&overview)?);
        return Ok(());
    }
    let brief = if brief.is_empty() { "(none)" } else { &brief };
    println!("# Brief\n{brief}\n\n# Needs you");
    crate::view::print_inbox_rows(&needs_you, false)?;
    println!("\n# Goals");
    crate::view::print_status_rows(&rows, false)?;
    if let Some(queue) = queue {
        println!("\n# Queue\n{queue}");
    }
    if !lessons.is_empty() {
        println!("\n# Lessons (candidate AGENTS.md rules)");
        for lesson in lessons {
            println!("- {lesson}");
        }
    }
    Ok(())
}
/// `hgm show`: the goal, plus its queue item and run class when it was taken
/// from the factory queue while factory mode is on.
fn show(store: &Store, goal: &Goal, json: bool) -> Result<()> {
    let factory = crate::factory::Factory::load(store)?;
    let described = factory.map(|f| f.describe(&goal.id)).transpose()?.flatten();
    let native = native_view(store, &goal.id)?;
    let manager = crate::watch::manager_goals(store)?
        .remove(&goal.id)
        .map(|state| state.view());
    let events = store.events(&goal.id)?;
    let mut fingerprints = crate::fingerprint::Fingerprints::default();
    let reviews = crate::roles::review_states(goal, &events, &mut fingerprints);
    // A check that passed on an earlier state of the work dir verifies
    // nothing now. Results kept before fingerprints (unknown) appear only in
    // the packet, so older goals print as before.
    let earlier: Vec<String> = crate::packet::check_states(goal, &events, &mut fingerprints)
        .iter()
        .filter(|c| c["result"] == "passed" && c["state"] == "stale")
        .filter_map(crate::packet::check_unverified)
        .collect();
    if described.is_none()
        && native.is_none()
        && manager.is_none()
        && reviews.is_empty()
        && earlier.is_empty()
    {
        return print_goal(goal, store, json);
    }
    if json {
        let mut value = serde_json::to_value(goal)?;
        if let Some(described) = described {
            value["factory"] = described;
        }
        if let Some(native) = native {
            value["native_goal"] = native;
        }
        if let Some(manager) = &manager {
            value["manager_goal"] = manager.clone();
        }
        if !reviews.is_empty() {
            value["reviews"] = reviews.into();
        }
        if !earlier.is_empty() {
            value["not_verified"] = earlier.into();
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    print_goal(goal, store, false)?;
    if let Some(described) = described {
        let reasons: Vec<&str> = described["run_class_reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect();
        println!(
            "Queue item: {}\nRun class: {}{}",
            described["item"].as_str().unwrap_or_default(),
            described["run_class"].as_str().unwrap_or("pending"),
            if reasons.is_empty() {
                String::new()
            } else {
                format!(" ({})", reasons.join("; "))
            }
        );
    }
    if let Some(native) = native {
        println!("Native goal: {}", native_text(&native));
    }
    if let Some(manager) = &manager {
        println!("Manager goal: {}", crate::manager_goal::text(manager));
    }
    if !reviews.is_empty() {
        println!("Reviews:");
        for review in &reviews {
            println!("  {}", crate::packet::review_line(review));
        }
    }
    if !earlier.is_empty() {
        println!("Not verified:");
        for line in &earlier {
            println!("  {line}");
        }
    }
    Ok(())
}
/// The native goal hgm set for `goal`, with whether it is set and its last
/// recorded outcome, while a watch keeps it.
fn native_view(store: &Store, goal: &str) -> Result<Option<serde_json::Value>> {
    Ok(crate::watch::native_goals(store)?.get(goal).map(|state| {
        serde_json::json!({"set": state.is_set(), "outcome": state.outcome, "task": state.task})
    }))
}
pub(crate) fn native_text(native: &serde_json::Value) -> String {
    match (native["set"].as_bool(), native["outcome"].as_str()) {
        (Some(true), None) => "set".into(),
        (Some(true), Some(outcome)) => format!("set, last outcome {outcome}"),
        (_, Some(outcome)) => format!("not set, last outcome {outcome}"),
        _ => "not set".into(),
    }
}
/// Status rows, with each factory goal's run class while factory mode is on.
/// Also shows how long a watched worker has sat settled short of completion.
fn status_rows(
    store: &Store,
    goals: &[Goal],
    factory: Option<&crate::factory::Factory>,
) -> Result<Vec<serde_json::Value>> {
    let mut rows = crate::view::status_rows(goals);
    if let Some(factory) = factory {
        factory.annotate(&mut rows)?;
    }
    let idle = crate::watch::idle_stretches(store, chrono::Utc::now().timestamp())?;
    let native = crate::watch::native_goals(store)?;
    let manager = crate::watch::manager_goals(store)?;
    let mut fingerprints = crate::fingerprint::Fingerprints::default();
    for (row, goal) in rows.iter_mut().zip(goals) {
        // The latest verdict, and whether the work dir still matches it.
        let events = store.events(&goal.id)?;
        if let Some(review) = crate::roles::review_states(goal, &events, &mut fingerprints).pop() {
            row["review"] = review;
        }
        if let Some(stretch) = idle.get(&goal.id)
            && crate::watch::short_of_completion(goal)
        {
            row["idle"] = serde_json::json!({"role": stretch.role, "secs": stretch.secs});
        }
        if let Some(state) = native.get(&goal.id) {
            row["native_goal"] =
                serde_json::json!({"set": state.is_set(), "outcome": state.outcome});
        }
        if let Some(state) = manager.get(&goal.id) {
            row["manager_goal"] = state.view();
        }
        row["attention"] = crate::accounting::compute(goal, &events, chrono::Utc::now());
    }
    Ok(rows)
}
/// Inbox rows, with the factory queue's hold while factory mode is on, and a
/// row for each watched worker the person was told sits idle short of completion.
fn inbox_rows(
    store: &Store,
    goals: &[Goal],
    factory: Option<&crate::factory::Factory>,
) -> Result<Vec<serde_json::Value>> {
    let mut rows = crate::view::inbox_rows(store, goals)?;
    let idle = crate::watch::idle_stretches(store, chrono::Utc::now().timestamp())?;
    for goal in goals {
        if let Some(stretch) = idle.get(&goal.id)
            && stretch.noticed
            && crate::watch::short_of_completion(goal)
        {
            rows.push(serde_json::json!({
                "id": goal.id,
                "objective": goal.spec.objective,
                "kind": "worker_idle",
                "reason": format!(
                    "{} idle {} short of completion; no next step is prepared",
                    stretch.role,
                    crate::watch_view::duration(stretch.secs)
                ),
                "since": chrono::DateTime::from_timestamp(stretch.since, 0),
            }));
        }
    }
    if let Some(row) = factory.map(|f| f.held_row()).transpose()?.flatten() {
        rows.push(row);
    }
    Ok(rows)
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
        // Helper roles run hgm only to read the contract and report back; each
        // record command also checks the role's own list.
        if let Ok(role) = std::env::var("HGM_ROLE") {
            ensure!(
                matches!(
                    action,
                    Action::Show { .. }
                        | Action::Report { .. }
                        | Action::ReviewSubmit { .. }
                        | Action::Decision { .. }
                        | Action::FollowUp { .. }
                ),
                "the {role} role may only run show, report, review-submit, decision or follow-up"
            );
        }
        match action {
            Action::Manager { agent, model } => {
                let factory = crate::config::load(&store.workspace)?.factory_enabled();
                crate::manager::launch(
                    &store,
                    *agent,
                    model.as_deref(),
                    &command_reference(factory),
                )
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
            Action::Attention { all, finished } => {
                let (goals, hidden) =
                    without_finished(visible_goals(&store, *all)?, *finished || self.json);
                let now = chrono::Utc::now();
                let mut rows = vec![];
                for goal in &goals {
                    let accounting =
                        crate::accounting::compute(goal, &store.events(&goal.id)?, now);
                    rows.push(
                        serde_json::json!({"id": goal.id, "objective": goal.spec.objective,
                        "status": goal.status, "attention": accounting}),
                    );
                }
                if self.json {
                    println!("{}", serde_json::to_string_pretty(&rows)?);
                } else {
                    for row in &rows {
                        println!(
                            "{} — {} · {}\n  {}",
                            row["objective"].as_str().unwrap_or_default(),
                            row["status"].as_str().unwrap_or_default(),
                            row["id"].as_str().unwrap_or_default(),
                            crate::accounting::line(&row["attention"])
                        );
                    }
                    if rows.is_empty() && hidden == 0 {
                        println!("No tracked goals.");
                    }
                    if hidden > 0 {
                        println!("{}", hidden_line("attention", hidden));
                    }
                }
            }
            Action::Status { all, finished } => {
                let factory = crate::factory::Factory::load(&store)?;
                let (goals, hidden) =
                    without_finished(visible_goals(&store, *all)?, *finished || self.json);
                let rows = status_rows(&store, &goals, factory.as_ref())?;
                if rows.is_empty() && hidden > 0 {
                    println!("{}", hidden_line("status", hidden));
                } else {
                    crate::view::print_status_rows(&rows, self.json)?;
                    if hidden > 0 {
                        println!("{}", hidden_line("status", hidden));
                    }
                }
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
                        for line in crate::events::full_text(event)
                            .into_iter()
                            .flat_map(str::lines)
                        {
                            println!("    {line}");
                        }
                    }
                }
            }
            Action::Confirm { id, evidence } => {
                let goal = runner::confirm_goal(&store, id, evidence).await?;
                print_result(&goal, &store, self.json, self.verbose)?;
                if let Some(warning) = shared_work_dir(&store, &goal)? {
                    eprintln!("{warning}");
                }
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
                // Without --task, the one task whose plan awaits approval.
                let task = match task {
                    Some(task) => Some(task.clone()),
                    None => crate::admission::plan_pending(&store.events(id)?, id)?,
                };
                store.record(
                    id,
                    "plan_approved",
                    serde_json::json!({"task": task, "evidence": evidence}),
                )?;
                print_goal(&store.load(id)?, &store, self.json)?;
            }
            Action::Inbox { all } => {
                let factory = crate::factory::Factory::load(&store)?;
                let rows = inbox_rows(&store, &visible_goals(&store, *all)?, factory.as_ref())?;
                crate::view::print_inbox_rows(&rows, self.json)?
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
            Action::Target {
                start,
                adopt,
                lines,
            } => println!(
                "{}",
                serde_json::to_string_pretty(
                    &crate::supervision::target(
                        &store,
                        start.map(|p| p.name()),
                        adopt.as_deref(),
                        *lines
                    )
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
                prepare,
                withdraw,
                step,
            } => {
                if *withdraw {
                    let place = step.map(|n| n as usize);
                    let result = crate::prepared::withdraw(&store, to, place).await?;
                    println!("{}", serde_json::to_string_pretty(&result)?);
                    return Ok(());
                }
                let text = text.as_deref().context("the assignment text is missing")?;
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
                    via: None,
                };
                let result = if *prepare {
                    crate::prepared::prepare(&store, &assignment, text).await?
                } else {
                    crate::supervision::send(&store, text, assignment, helper.as_ref()).await?
                };
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
                    via: None,
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
                    via: None,
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
            Action::Report {
                id,
                text,
                file,
                task,
            } => println!(
                "{}",
                serde_json::to_string_pretty(&crate::roles::report(
                    &store,
                    id,
                    task.as_deref(),
                    text.as_deref(),
                    file.as_deref(),
                )?)?
            ),
            Action::Decision {
                id,
                text,
                why,
                task,
            } => println!(
                "{}",
                serde_json::to_string_pretty(&crate::packet::decide(
                    &store,
                    id,
                    task.as_deref(),
                    text,
                    why
                )?)?
            ),
            Action::FollowUp { id, text, task } => println!(
                "{}",
                serde_json::to_string_pretty(&crate::packet::follow_up(
                    &store,
                    id,
                    task.as_deref(),
                    text
                )?)?
            ),
            Action::Packet { id } => crate::packet::show(&store, id, self.json)?,
            Action::Supervise {
                id,
                role,
                idle_secs,
                cooldown_secs,
                max_wakes,
                idle_notice_secs,
                patrol_minutes,
            } => {
                let pacing = crate::supervision::Pacing {
                    idle_secs: *idle_secs,
                    cooldown_secs: *cooldown_secs,
                    max_wakes: *max_wakes,
                    idle_notice_secs: *idle_notice_secs,
                    patrol_minutes: *patrol_minutes,
                };
                let (watch, report) = crate::supervision::supervise(
                    &store,
                    id,
                    role,
                    crate::roles::resolve(&store, role)?,
                    pacing,
                )
                .await?;
                let mut value = serde_json::to_value(&watch)?;
                value["pacing"] = report;
                println!("{}", serde_json::to_string_pretty(&value)?);
            }
            Action::Wait { id, reason } => {
                let goal = store.update(id, |g| g.wait(reason.clone()))?;
                crate::native_goal::clear_for(&store, Some(id), "the goal waits for the person")
                    .await?;
                crate::watch::release_watches(&store, id, "waiting_for_user")?;
                print_goal(&goal, &store, self.json)?
            }
            Action::Await { timeout } => {
                let result =
                    crate::wake_await::wait(&store, std::time::Duration::from_secs(*timeout))
                        .await?;
                if self.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("{}", crate::wake_await::text(&result));
                }
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
                validate,
                replace,
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
                if *validate {
                    return validate_contract(&store, raw, self.json);
                }
                // Contract validation states what is wrong; this names the next step.
                const FIX: &str = "nothing was registered: fix the contract and check it with hgm add --file PATH --validate";
                let spec = crate::config::goal_spec(&store.workspace, raw).context(FIX)?;
                let goal = match replace {
                    Some(id) => store.replace_contract(id, spec)?,
                    None => {
                        let owner = crate::supervision::origin_terminal(&store)?;
                        store.add(spec, owner).context(FIX)?
                    }
                };
                print_result(&goal, &store, self.json, self.verbose)?;
            }
            Action::List { all, finished } => {
                let (goals, hidden) =
                    without_finished(visible_goals(&store, *all)?, *finished || self.json);
                if self.json {
                    println!("{}", serde_json::to_string_pretty(&goals)?);
                } else {
                    if !goals.is_empty() || hidden == 0 {
                        print_list(&goals);
                    }
                    if hidden > 0 {
                        println!("{}", hidden_line("list", hidden));
                    }
                }
            }
            Action::Show { id } => show(&store, &store.load(id)?, self.json)?,
            Action::Pause { id } => {
                let goal = store.update(id, |g| g.request(Control::Pause))?;
                crate::native_goal::clear_for(&store, Some(id), "the goal is paused").await?;
                crate::watch::release_watches(&store, id, "stopped_by_goal")?;
                print_goal(&goal, &store, self.json)?
            }
            Action::Cancel { id } => {
                let goal = store.update(id, |g| g.request(Control::Cancel))?;
                crate::native_goal::clear_for(&store, Some(id), "the goal is cancelled").await?;
                crate::watch::release_watches(&store, id, "stopped_by_goal")?;
                print_goal(&goal, &store, self.json)?
            }
            Action::Note { id, text } => print_goal(
                &store.update(id, |g| g.note(text.clone()))?,
                &store,
                self.json,
            )?,
            Action::Complete { id, task, evidence } => print_result(
                &store.update(id, |g| g.complete_manual(task, evidence.clone()))?,
                &store,
                self.json,
                self.verbose,
            )?,
            Action::Accept { id, evidence } => {
                let goal = runner::accept(&store, id, evidence, &CommandExecutor).await?;
                if goal.status == GoalStatus::Completed {
                    crate::native_goal::clear_for(&store, Some(id), "the goal is completed")
                        .await?;
                }
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
                        if goal.status == GoalStatus::ReadyForReview {
                            crate::native_goal::clear_for(
                                &store,
                                Some(&goal.id),
                                "the goal is ready for review",
                            )
                            .await?;
                        }
                        print_result(&goal, &store, self.json, self.verbose)?;
                        ensure!(
                            goal.status == GoalStatus::ReadyForReview,
                            "goal needs attention: {:?}; inspect with hgm show {}",
                            goal.status,
                            goal.id
                        );
                    }
                    Err(error) => {
                        if let Ok(goal) = store.load(&args.id) {
                            print_result(&goal, &store, self.json, self.verbose)?;
                        }
                        return Err(error);
                    }
                }
            }
            Action::Queue { action } => queue(&store, action, self.json).await?,
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
/// The factory queue commands are listed only where factory mode is on, so
/// the reference stays the same for everyone else.
pub(crate) fn command_reference(factory: bool) -> String {
    const NOT_FOR_MANAGER: [&str; 6] = [
        "manager",
        "panel",
        "herdr-open",
        "herdr-board",
        "herdr-run",
        "queue",
    ];
    let mut lines = vec![
        "## hgm command reference".to_string(),
        "Complete for this build. Run these directly; do not run --help to rediscover them. Add --json for machine-readable output.".into(),
        "Native goal: the watch sets and clears /goal itself, on a Claude Code main under autonomy auto per loop.native_goal (assignment, settle or false), and on a Claude Code Goal Manager while a confirmed goal of the tab is in progress (config manager.native_goal: false turns that off). There is no command for it, never type /goal, and its verdict is an observation, not evidence.".into(),
    ];
    let cli = Cli::command();
    for command in cli.get_subcommands() {
        let name = command.get_name();
        if command.is_hide_set() || NOT_FOR_MANAGER.contains(&name) {
            continue;
        }
        lines.push(reference_line(&format!("hgm {name}"), command));
    }
    if factory && let Some(queue) = cli.find_subcommand("queue") {
        for command in queue.get_subcommands() {
            lines.push(reference_line(
                &format!("hgm queue {}", command.get_name()),
                command,
            ));
        }
    }
    lines.join("\n")
}
/// One reference line: the command with its arguments, then what it does.
fn reference_line(prefix: &str, command: &clap::Command) -> String {
    let mut usage = prefix.to_string();
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
    format!("- `{usage}` — {about}")
}

#[derive(Debug, Subcommand)]
pub(crate) enum QueueAction {
    /// Queue a user-authored contract with the user's agreement to run it
    Add {
        #[arg(long)]
        file: PathBuf,
        /// The user's words agreeing to run this contract
        #[arg(long)]
        evidence: String,
    },
    /// List queued items with their state, agreement age and history
    List {
        /// Include removed items
        #[arg(long)]
        all: bool,
    },
    /// Hold a queued item back from dispatch; its history stays
    Park {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Return a parked item to the queue
    Unpark { id: String },
    /// Take a queued, parked or failed item off the queue; its history stays
    Remove { id: String },
    /// Turn the oldest queued item into a confirmed goal, unless results
    /// awaiting review or goals in progress reach the factory limits
    Next,
}

/// Runs a factory queue command; each change holds the queue lock and first
/// settles any claim a stopped dispatch left.
async fn queue(store: &Store, action: &QueueAction, json: bool) -> Result<()> {
    let config = crate::config::load(&store.workspace)?;
    config.require_factory()?;
    let queue = crate::queue::Queue::open(store)?;
    let now = chrono::Utc::now();
    if let QueueAction::List { all } = action {
        let views = queue
            .statuses()?
            .iter()
            .filter(|s| *all || s.state != crate::queue::State::Removed)
            .map(|s| queue.view(s, now))
            .collect::<Result<Vec<_>>>()?;
        if json {
            println!("{}", serde_json::to_string_pretty(&views)?);
        } else if views.is_empty() {
            println!(
                "The queue is empty. Queue an agreed contract with hgm queue add --file CONTRACT --evidence TEXT."
            );
        } else {
            views.iter().for_each(print_queue_item);
        }
        return Ok(());
    }
    let locked = queue.lock()?;
    for note in locked.recover().await? {
        eprintln!("{note}");
    }
    if let QueueAction::Next = action {
        if let Some((reason, _)) = crate::factory::Gate::new(&store.list()?, &config).held() {
            anyhow::bail!("hgm queue next refused: {reason}");
        }
        let next = locked
            .statuses()?
            .into_iter()
            .find(|s| s.state == crate::queue::State::Queued)
            .context("no queued item to dispatch; queue an agreed contract with hgm queue add")?;
        // The goal belongs to the manager taking the item out (R1).
        let owner = crate::supervision::origin_terminal(store)?;
        let own = crate::factory::OwnCheckout::new(&config, &store.workspace);
        let (_, goal) = locked
            .dispatch(&next.id, owner, own == crate::factory::OwnCheckout::On)
            .await?;
        let view = locked.view(&locked.status(&next.id)?, now)?;
        let skipped = match &own {
            crate::factory::OwnCheckout::Skipped(reason) if goal.checkout.is_none() => {
                Some(reason.as_str())
            }
            _ => None,
        };
        if json {
            let mut value = serde_json::json!({"item": view, "goal": goal});
            if let Some(reason) = skipped {
                value["checkout_skipped"] = reason.into();
            }
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            println!(
                "Queue item {} (agreed {} ago) is now goal {}; send its first assignment with hgm send --goal {} --task TASK",
                next.id,
                view["agreement_age"].as_str().unwrap_or_default(),
                goal.id,
                goal.id
            );
            if let Some(checkout) = &goal.checkout {
                println!(
                    "Checkout: {} (branch {}); linked assignments say to work there",
                    checkout.path.display(),
                    checkout.branch
                );
            }
            if let Some(reason) = skipped {
                println!("No own checkout: {reason}");
            }
            print_goal(&goal, store, false)?;
        }
        return Ok(());
    }
    let status = match action {
        QueueAction::Add { file, evidence } => locked.add(
            std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?,
            std::path::absolute(file)?,
            evidence,
            crate::supervision::origin_terminal(store)?,
            config.loop_defaults,
        )?,
        QueueAction::Park { id, reason } => locked.park(id, reason.as_deref())?,
        QueueAction::Unpark { id } => locked.unpark(id)?,
        QueueAction::Remove { id } => locked.remove(id)?,
        QueueAction::List { .. } | QueueAction::Next => unreachable!("handled above"),
    };
    let view = locked.view(&status, now)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        print_queue_item(&view);
    }
    Ok(())
}
fn print_queue_item(view: &serde_json::Value) {
    let text = |key: &str| view[key].as_str().unwrap_or_default().to_string();
    println!(
        "{}  {}  agreed {} ago  {}",
        text("id"),
        text("state"),
        text("agreement_age"),
        text("objective")
    );
    if let Some(goal) = view["goal"].as_str() {
        println!("  Goal: {goal}");
    }
    if let Some(checkout) = view.get("checkout") {
        println!(
            "  Checkout: {} (branch {}){}",
            checkout["path"].as_str().unwrap_or_default(),
            checkout["branch"].as_str().unwrap_or_default(),
            if checkout["removed_at"].is_string() {
                ", removed"
            } else {
                ""
            }
        );
    }
    if let Some(reason) = view["reason"].as_str() {
        println!("  Not dispatched: {reason}");
    }
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

/// What add, confirm, complete, run and resume print: the short result, or
/// the whole goal with `--verbose`; `--json` prints the goal as before.
fn print_result(goal: &crate::model::Goal, store: &Store, json: bool, verbose: bool) -> Result<()> {
    if json || verbose {
        return print_goal(goal, store, json);
    }
    crate::view::print_short(goal);
    Ok(())
}

/// `add --validate`: the contract's problems and words per field, with nothing
/// registered. What confirmation would refuse (autonomy auto without a command
/// check) counts as a problem too. Fails when there is one.
fn validate_contract(store: &Store, raw: serde_json::Value, json: bool) -> Result<()> {
    use crate::model::SPEC_WORD_BUDGET;
    let mut problems: Vec<String> = vec![];
    let mut report = serde_json::json!({"registered": false});
    match crate::config::goal_spec(&store.workspace, raw) {
        Err(error) => problems.push(format!("{error:#}")),
        Ok(spec) => {
            if let Err(error) = crate::model::Goal::new(spec.clone(), store.workspace.clone()) {
                problems.push(format!("{error:#}"));
            }
            // A budget refusal ends validation early; look past it for the rest.
            let over = spec.words() > SPEC_WORD_BUDGET
                && spec
                    .long_spec_reason
                    .as_ref()
                    .is_none_or(|r| r.trim().is_empty());
            if over {
                let mut rest = spec.clone();
                rest.long_spec_reason = Some("validate".into());
                if let Err(error) = rest.validate() {
                    problems.push(format!("{error:#}"));
                }
            }
            if spec.loop_policy.autonomy == crate::contract::Autonomy::Auto
                && spec.checks.is_empty()
            {
                problems.push(
                    "autonomy auto needs at least one command check, so hgm confirm would refuse it: add a check, or set loop.autonomy: ask".into(),
                );
            }
            report["words"] = spec.words().into();
            report["budget"] = SPEC_WORD_BUDGET.into();
            report["fields"] = spec
                .word_fields()
                .into_iter()
                .map(|(field, words)| (field.to_string(), serde_json::Value::from(words)))
                .collect::<serde_json::Map<_, _>>()
                .into();
            report["breakdown"] = spec.word_breakdown().into();
        }
    }
    report["valid"] = problems.is_empty().into();
    report["problems"] = problems.clone().into();
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        if let Some(words) = report["words"].as_u64() {
            println!(
                "Contract checked, not registered: about {words} words (budget {SPEC_WORD_BUDGET}).\nWords by field: {}",
                report["breakdown"].as_str().unwrap_or_default()
            );
        }
        if problems.is_empty() {
            println!("No problems: hgm add --file registers it as it is.");
        } else {
            println!("Problems:");
            for problem in &problems {
                println!("- {problem}");
            }
        }
    }
    ensure!(
        problems.is_empty(),
        "the contract has {} problem(s); nothing was registered",
        problems.len()
    );
    Ok(())
}

/// `goals` without completed and cancelled ones unless `keep`, and how many
/// were left out: status and list show what is still open by default.
fn without_finished(goals: Vec<Goal>, keep: bool) -> (Vec<Goal>, usize) {
    if keep {
        return (goals, 0);
    }
    let total = goals.len();
    let open: Vec<Goal> = goals
        .into_iter()
        .filter(|goal| !matches!(goal.status, GoalStatus::Completed | GoalStatus::Cancelled))
        .collect();
    let hidden = total - open.len();
    (open, hidden)
}
/// The line that says how many finished goals `command` left out.
fn hidden_line(command: &str, hidden: usize) -> String {
    format!("({hidden} completed or cancelled goal(s) hidden; hgm {command} --finished shows them)")
}

/// A warning, never a refusal, when other confirmed goals that are not finished
/// work in the same directory as `goal`: their uncommitted changes mix in one
/// tree (2026-11-15: five goals in one tree). Names the checkout option.
fn shared_work_dir(store: &Store, goal: &Goal) -> Result<Option<String>> {
    let Ok(dir) = goal.work_dir() else {
        return Ok(None);
    };
    let others: Vec<String> = store
        .list()?
        .into_iter()
        .filter(|other| {
            other.id != goal.id
                && other.confirmed.is_some()
                && !matches!(other.status, GoalStatus::Completed | GoalStatus::Cancelled)
                && other.work_dir().is_ok_and(|d| d == dir)
        })
        .map(|other| other.id)
        .collect();
    if others.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "Warning: {} also work(s) in {}, confirmed and not finished, so their uncommitted changes mix in one tree. A goal can have its own checkout (a contract `checkout`, a Herdr worktree; see README, Own checkout); to keep this goal apart, cancel it and register it again with one, if the user agrees.",
        others.join(", "),
        dir.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_review_commands_are_in_the_manager_reference() {
        let reference = command_reference(false);
        for line in [
            "`hgm decision <ID> <TEXT> --why <WHY> [--task <TASK>]`",
            "`hgm follow-up <ID> <TEXT> [--task <TASK>]`",
            "`hgm packet <ID>`",
        ] {
            assert!(reference.contains(line), "missing {line}: {reference}");
        }
    }

    #[test]
    fn the_queue_commands_join_the_reference_only_in_factory_mode() {
        let off = command_reference(false);
        assert!(!off.contains("hgm queue"), "{off}");
        let on = command_reference(true);
        assert!(on.starts_with(&off), "only lines are added");
        for line in [
            "`hgm queue add --file <FILE> --evidence <EVIDENCE>`",
            "`hgm queue list [--all]`",
            "`hgm queue park <ID> [--reason <REASON>]`",
            "`hgm queue unpark <ID>`",
            "`hgm queue remove <ID>`",
            "`hgm queue next`",
        ] {
            assert!(on.contains(line), "missing {line}: {on}");
        }
    }
}
