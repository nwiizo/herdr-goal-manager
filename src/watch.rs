//! The watch over one goal's worker: observation, holds, settle checks and
//! manager wakes. Arming lives in `supervision`; the driver loop in `monitor`.
use crate::{
    binding::{Target, args, live_pane, operation_lease, path, session},
    herdr_cli as herdr,
    model::{Control, GoalStatus},
    signals::{Stop, compaction_shown, needs_human, rot_signals, stop_reason},
    signals::{busy_on_screen, can_prompt, input_refusal},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::OpenOptions, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Watch {
    pub context: String,
    /// The goal wakes are about: the one whose linked assignment reached the
    /// observed pane last.
    pub goal: String,
    /// Every goal sharing this watch because its work goes to the same pane.
    /// Empty when `goal` is the only one, as in records written before sharing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goals: Vec<String>,
    /// Wakes spent by member goals other than `goal`: each goal has its own budget.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spent: BTreeMap<String, u32>,
    /// The observed worker: the original agent, or the helper role in `role`.
    pub origin: Target,
    /// `main` or the helper role that received the latest linked assignment.
    #[serde(default = "main_role")]
    pub role: String,
    /// Latest assignment or correction already covered by settle checks.
    #[serde(default)]
    pub checked_through: Option<chrono::DateTime<Utc>>,
    /// Re-anchor trigger already delivered to the manager.
    #[serde(default)]
    pub reanchored_for: Option<chrono::DateTime<Utc>>,
    pub manager: Target,
    pub active: bool,
    pub idle_secs: u64,
    pub cooldown_secs: u64,
    pub max_wakes: u32,
    pub wakes: u32,
    pub last_sent: i64,
    pub last_change: i64,
    pub fingerprint: String,
    pub pending: Option<String>,
    pub status: String,
    #[serde(default)]
    pub handoff_due: bool,
    /// Last drift patrol (Unix seconds).
    #[serde(default)]
    pub last_patrol: i64,
    /// Context-rot signs counted since the last rot wake or handoff.
    #[serde(default)]
    pub rot_signals: u32,
    /// Why automatic wakeups are held: the user was told and only a change on
    /// the original screen releases it (dialog, login, limit, no progress).
    #[serde(default)]
    pub held: Option<String>,
    /// Cause of the latest hold (`dialog`, `login`, ...) until an observation
    /// shows no such cause, so a dialog that stays on screen while the state
    /// moves (working, then blocked) is told to the user once.
    #[serde(default)]
    pub held_cause: Option<String>,
    /// Fingerprint at the last delivered wake, and how many wakes it received.
    #[serde(default)]
    pub wake_fingerprint: String,
    #[serde(default)]
    pub same_wakes: u32,
    /// When the manager stopped accepting input (Unix seconds; 0 while it does).
    #[serde(default)]
    pub manager_unavailable_since: i64,
    #[serde(default)]
    pub manager_notified: bool,
    /// Context-rot evidence already counted (signal and line, last 20).
    #[serde(default)]
    pub rot_evidence: Vec<String>,
    /// The last screen lines quoted at the previous wake: the next wake quotes
    /// only lines after them.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub wake_anchor: String,
    /// How the watch learns about changes: `events` (Herdr's event stream) or
    /// `polling` (a full re-read every 10 seconds).
    #[serde(default)]
    pub mode: String,
    /// When the worker was last observed (Unix seconds).
    #[serde(default)]
    pub last_observation_at: i64,
    /// The pending wake says the worker stopped after new work: it skips the
    /// cooldown and may reach a busy Claude Code or Copilot manager.
    #[serde(default)]
    pub pending_stop: bool,
    /// When the observation behind the pending wake was made (Unix seconds;
    /// 0 without one). Work sent after it makes the wake stale.
    #[serde(default)]
    pub pending_at: i64,
    /// Since when the worker has been settled (Unix seconds; 0 while it works).
    #[serde(default)]
    pub idle_since: i64,
    /// Whether the person was told about the current idle stretch.
    #[serde(default)]
    pub idle_noticed: bool,
    /// Seconds settled short of completion before the person is told.
    #[serde(default = "default_idle_notice_secs")]
    pub idle_notice_secs: u64,
    /// Patrol interval set with `supervise --patrol-minutes`; the goal's
    /// `loop.patrol_minutes` applies while it is unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patrol_minutes: Option<u32>,
    /// The idle stretch is a worker that works but has shown no progress
    /// (timers and spinners aside) for `idle_secs`.
    #[serde(default)]
    pub stuck: bool,
    /// The provider's retry notice seen during that stretch, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<String>,
    /// Why the prepared step was last kept, so a reason is told once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_held: Option<String>,
    /// The native goal (`/goal`) hgm set on the worker, and its last outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_goal: Option<crate::native_goal::NativeGoalState>,
    /// The worker's provider did not offer `/goal`; hgm continues without it.
    #[serde(default)]
    pub native_unavailable: bool,
    /// The native goal hgm set on the Goal Manager (see `manager_goal`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manager_goal: Option<crate::manager_goal::ManagerGoalState>,
}
fn default_idle_notice_secs() -> u64 {
    crate::config::DEFAULT_IDLE_NOTICE_SECS
}
/// Wakes delivered for one unchanged observation before the user is asked
/// instead. Repeating an unchanged observation only spends the manager's turns.
const MAX_SAME_WAKES: u32 = 2;
/// Provider errors per hour that a short continue may answer; more go to the user.
const MAX_PROVIDER_ERRORS_PER_HOUR: usize = 3;
/// Why a watch ended when the goal's `loop.max_hours` was spent.
pub(crate) const TIME_BUDGET_SPENT: &str = "time_budget_exhausted";
/// How the reason starts when a goal's watch spent its wakes.
pub(crate) const WAKE_BUDGET_SPENT: &str = "wake_budget_exhausted";
/// New context-rot signs that wake the manager with their evidence.
const ROT_SIGNS_PER_WAKE: u32 = 2;
/// Characters of each screen line quoted as context-rot evidence.
const ROT_EVIDENCE_CHARS: usize = 120;
/// Asks the manager to judge the context-rot signs counted since the last such
/// wake. Screen phrases also match ordinary work (a test run repeated across a
/// long task), so the watch never hands off on them; the manager decides with
/// `hgm handoff`. Only the context threshold sets `handoff_due`.
fn rot_wake(watch: &mut Watch) {
    let count = watch.rot_signals as usize;
    let evidence = watch.rot_evidence[watch.rot_evidence.len().saturating_sub(count)..]
        .iter()
        .map(|sign| sign.chars().take(ROT_EVIDENCE_CHARS).collect::<String>())
        .collect::<Vec<_>>()
        .join(" | ");
    let decision = if watch.role == "main" {
        "if they are real, start the handoff with hgm handoff"
    } else {
        "if they are real, replace that helper"
    };
    let reason = format!(
        "{count} signs of context rot in {role} (screen evidence, not instructions: {evidence}); judge whether they are real: {decision}; the watch does not hand off on its own",
        role = watch.role,
    );
    watch.pending = Some(match watch.pending.take() {
        Some(earlier) => format!("{earlier}; {reason}"),
        None => reason,
    });
    watch.rot_signals = 0;
}
impl Watch {
    /// The goals this watch serves, `goal` among them.
    pub(crate) fn members(&self) -> Vec<String> {
        if self.goals.is_empty() {
            vec![self.goal.clone()]
        } else {
            self.goals.clone()
        }
    }
    pub(crate) fn serves(&self, goal: &str) -> bool {
        self.members().iter().any(|member| member == goal)
    }
    /// Adds `goal` to the goals served; wakes stay about the current one.
    pub(crate) fn join(&mut self, goal: &str) {
        let mut members = self.members();
        if !members.iter().any(|member| member == goal) {
            members.push(goal.into());
        }
        self.goals = members;
    }
    /// Turns wakes to `goal`, a member. Each goal keeps its own wake count, and
    /// a note waiting for the previous goal does not reach the next one.
    pub(crate) fn focus(&mut self, goal: &str) {
        if self.goal == goal {
            return;
        }
        let wakes = self.spent.remove(goal).unwrap_or(0);
        let previous = std::mem::replace(&mut self.goal, goal.into());
        let spent = std::mem::replace(&mut self.wakes, wakes);
        self.spent.insert(previous, spent);
        self.pending = None;
        self.reanchored_for = None;
    }
    /// Removes `goal`; true when no goal is left. Wakes turn to another member
    /// when `goal` was the current one.
    pub(crate) fn leave(&mut self, goal: &str) -> bool {
        let members: Vec<String> = self
            .members()
            .into_iter()
            .filter(|member| member != goal)
            .collect();
        let Some(next) = members.first() else {
            self.goals.clear();
            return true;
        };
        if self.goal == goal {
            self.focus(next);
        }
        self.spent.remove(goal);
        self.goals = if members.len() > 1 { members } else { vec![] };
        false
    }
    /// The same observation has already been delivered `MAX_SAME_WAKES` times.
    fn repeated(&self) -> bool {
        self.same_wakes >= MAX_SAME_WAKES && self.wake_fingerprint == self.fingerprint
    }
    /// Counts a context-rot sign once: a sign that stays on screen across
    /// observations is the same sign. Returns whether it was new.
    fn count_rot(&mut self, signal: &str, line: &str) -> bool {
        let evidence = format!("{signal}:{line}");
        if self.rot_evidence.contains(&evidence) {
            return false;
        }
        self.rot_evidence.push(evidence);
        if self.rot_evidence.len() > 20 {
            self.rot_evidence.remove(0);
        }
        self.rot_signals += 1;
        true
    }
    /// Records a delivered wake for spin and budget accounting.
    fn mark_wake(&mut self, now: i64) {
        if self.wake_fingerprint == self.fingerprint {
            self.same_wakes += 1;
        } else {
            self.wake_fingerprint = self.fingerprint.clone();
            self.same_wakes = 1;
        }
        self.last_sent = now;
        self.wakes += 1;
        self.clear_pending();
    }
    fn clear_pending(&mut self) {
        self.pending = None;
        self.pending_stop = false;
        self.pending_at = 0;
    }
    /// The patrol interval in minutes: `supervise --patrol-minutes`, else the goal's.
    pub(crate) fn patrol(&self, goal: &crate::model::Goal) -> u32 {
        self.patrol_minutes
            .unwrap_or(goal.spec.loop_policy.patrol_minutes)
    }
    /// Seconds the worker has been settled, when it is.
    pub(crate) fn idle_for(&self, now: i64) -> Option<i64> {
        (self.active && self.idle_since > 0).then(|| now.saturating_sub(self.idle_since))
    }
}
pub(crate) fn watch_path(store: &Store, context: &str) -> Result<PathBuf> {
    path(store, context, "-watch.json")
}
/// Loads the watch kept under `key` (see `Session::key`). A record moved from
/// a manager context is saved back under `key`.
pub(crate) fn load_watch(store: &Store, key: &str) -> Result<Watch> {
    let mut watch: Watch = serde_json::from_slice(&std::fs::read(watch_path(store, key)?)?)?;
    watch.context = key.into();
    Ok(watch)
}
/// A manager reopened for the same terminal takes over its active watch: wakes
/// go to the new manager pane from the next observation on.
pub(crate) fn adopt_watch(store: &Store, key: &str, manager: Target) -> Result<Option<String>> {
    if !watch_path(store, key)?.exists() {
        return Ok(None);
    }
    let _operation = operation_lease(store, key)?;
    let mut watch = load_watch(store, key)?;
    if !watch.active || watch.manager.pane == manager.pane && watch.manager.agent == manager.agent {
        return Ok(None);
    }
    watch.manager = manager;
    watch.manager_unavailable_since = 0;
    watch.manager_notified = false;
    save_watch(store, &watch)?;
    store.record(
        &watch.goal,
        "watch_adopted",
        json!({"manager": watch.manager.pane}),
    )?;
    Ok(Some(watch.goal))
}
pub(crate) fn save_watch(store: &Store, watch: &Watch) -> Result<()> {
    store.write_private(&watch_path(store, &watch.context)?, watch)
}
fn main_role() -> String {
    "main".into()
}
/// Ends every active watch record of `goal`, in any manager context. Pause,
/// cancel and wait take effect even when no watch process is running to notice.
pub(crate) fn release_watches(store: &Store, goal: &str, reason: &str) -> Result<()> {
    let directory = store.private_directory("manager")?;
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-watch.json"))
        {
            continue;
        }
        let Ok(mut watch) = serde_json::from_slice::<Watch>(&std::fs::read(&path)?) else {
            continue;
        };
        if watch.active && watch.serves(goal) {
            drop_goal(store, &mut watch, goal, reason, false)?;
            save_watch(store, &watch)?;
        }
    }
    Ok(())
}
/// A watched worker's current idle stretch.
pub(crate) struct IdleStretch {
    pub role: String,
    pub secs: i64,
    pub since: i64,
    /// Whether the person was told about it.
    pub noticed: bool,
}
/// Idle stretches of workers that running watches observe, by member goal.
pub(crate) fn idle_stretches(store: &Store, now: i64) -> Result<BTreeMap<String, IdleStretch>> {
    let mut stretches = BTreeMap::new();
    for entry in std::fs::read_dir(store.private_directory("manager")?)? {
        let path = entry?.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-watch.json"))
        {
            continue;
        }
        let Ok(watch) = serde_json::from_slice::<Watch>(&std::fs::read(&path)?) else {
            continue;
        };
        let Some(secs) = watch.idle_for(now) else {
            continue;
        };
        if !watch_running(store, &watch, now)? {
            continue;
        }
        for goal in watch.members() {
            stretches.insert(
                goal,
                IdleStretch {
                    role: watch.role.clone(),
                    secs,
                    since: watch.idle_since,
                    noticed: watch.idle_noticed,
                },
            );
        }
    }
    Ok(stretches)
}
/// The native goal each active watch recorded, by the goal it was set for.
pub(crate) fn native_goals(
    store: &Store,
) -> Result<BTreeMap<String, crate::native_goal::NativeGoalState>> {
    let mut goals = BTreeMap::new();
    for entry in std::fs::read_dir(store.private_directory("manager")?)? {
        let path = entry?.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-watch.json"))
        {
            continue;
        }
        let Ok(watch) = serde_json::from_slice::<Watch>(&std::fs::read(&path)?) else {
            continue;
        };
        if let Some(state) = watch.native_goal.filter(|_| watch.active) {
            goals.insert(state.goal.clone(), state);
        }
    }
    Ok(goals)
}
/// The Goal Manager's native goal recorded by each active watch, by the goals
/// that watch serves.
pub(crate) fn manager_goals(
    store: &Store,
) -> Result<BTreeMap<String, crate::manager_goal::ManagerGoalState>> {
    let mut goals = BTreeMap::new();
    for entry in std::fs::read_dir(store.private_directory("manager")?)? {
        let path = entry?.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-watch.json"))
        {
            continue;
        }
        let Ok(watch) = serde_json::from_slice::<Watch>(&std::fs::read(&path)?) else {
            continue;
        };
        if let Some(state) = watch.manager_goal.clone().filter(|_| watch.active) {
            for goal in watch.members() {
                goals.insert(goal, state.clone());
            }
        }
    }
    Ok(goals)
}
/// Whether a running watch, in any manager context, serves `goal`.
pub(crate) fn watched(store: &Store, goal: &str) -> Result<bool> {
    let now = Utc::now().timestamp();
    for entry in std::fs::read_dir(store.private_directory("manager")?)? {
        let path = entry?.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-watch.json"))
        {
            continue;
        }
        let Ok(watch) = serde_json::from_slice::<Watch>(&std::fs::read(&path)?) else {
            continue;
        };
        if watch.active && watch.serves(goal) && watch_running(store, &watch, now)? {
            return Ok(true);
        }
    }
    Ok(false)
}
/// Takes `goal` off the watch. The watch itself ends with `reason` only when
/// no other goal shares it; otherwise only `goal`'s timeline records the end.
fn drop_goal(
    store: &Store,
    watch: &mut Watch,
    goal: &str,
    reason: &str,
    notify: bool,
) -> Result<()> {
    if watch.leave(goal) {
        end_watch(store, watch, reason, notify);
    } else {
        store.record(
            goal,
            "watch_ended",
            json!({"reason": reason, "notify": notify}),
        )?;
    }
    Ok(())
}
/// Drops member goals that no longer want watching (paused, waiting, finished
/// or ready for review) and turns wakes to the goal whose linked assignment
/// reached the observed pane last. False once no goal is left: the watch ended.
pub(crate) fn refocus(store: &Store, watch: &mut Watch) -> Result<bool> {
    let mut latest: Option<(chrono::DateTime<Utc>, String)> = None;
    for id in watch.members() {
        let goal = store.load(&id)?;
        if goal.control != Control::Run
            || goal.waiting.is_some()
            || matches!(
                goal.status,
                GoalStatus::Completed | GoalStatus::Cancelled | GoalStatus::ReadyForReview
            )
        {
            drop_goal(store, watch, &id, "stopped_by_goal", false)?;
            if !watch.active {
                return Ok(false);
            }
            continue;
        }
        let assigned = store
            .events(&id)?
            .into_iter()
            .rev()
            .find(|event| {
                matches!(event.kind.as_str(), "assigned" | "correction")
                    && event.detail["pane"] == watch.origin.pane.as_str()
            })
            .map(|event| event.at);
        if let Some(at) = assigned
            && latest.as_ref().is_none_or(|(newest, _)| at > *newest)
        {
            latest = Some((at, id));
        }
    }
    if let Some((_, id)) = latest {
        watch.focus(&id);
    }
    Ok(true)
}
/// Ends the watch for a cause only the person can resolve: the goal waits for
/// them with `reason`, so an unfinished goal is never left running unwatched.
/// A watch otherwise ends only when its goal is completed, cancelled, paused
/// or waiting.
/// A cause on the observed pane blocks every goal sharing the watch.
pub(crate) fn end_for_person(store: &Store, watch: &mut Watch, reason: &str) {
    for goal in watch.members() {
        if let Err(error) = store.update(&goal, |goal| goal.wait(reason.to_string())) {
            eprintln!("Could not set goal {goal} waiting: {error:#}");
        }
    }
    end_watch(store, watch, reason, true);
}
/// Records why monitoring stopped; abnormal ends notify the user.
pub(crate) fn end_watch(store: &Store, watch: &mut Watch, reason: &str, notify: bool) {
    watch.active = false;
    watch.status = reason.into();
    if let Err(error) = store.record(
        &watch.goal,
        "watch_ended",
        json!({"reason": reason, "notify": notify}),
    ) {
        eprintln!("Could not record the end of monitoring: {error:#}");
    }
}
/// What one observation of the original agent showed.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Changed,
    Unchanged,
    /// Unchanged after `MAX_SAME_WAKES` wakes: ask the user, stop waking.
    Stalled,
}
/// Updates the watch from one observation. Any change releases a hold.
///
/// `sent_seq` is the worker's `state_change_seq` when the latest delivery to it
/// was sent: a settled state that has not moved since then belongs to the
/// previous turn and is not news.
fn observe(watch: &mut Watch, live: &Value, screen: &str, now: i64, sent_seq: Option<u64>) -> Seen {
    let state = live["agent_status"].as_str().unwrap_or("");
    let working = state == "working";
    // While a turn runs, spinners, timers and token counts move by
    // themselves; only the rest of the screen shows progress.
    let basis = if working {
        crate::signals::progress_view(screen, live["agent"].as_str().unwrap_or(""))
    } else {
        screen.to_string()
    };
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(format!(
            "{}:{}:{basis}",
            live["agent_status"], live["state_change_seq"]
        ))
    );
    let seq = live["state_change_seq"].as_u64();
    let moved_since_delivery = match (sent_seq, seq) {
        (Some(sent), Some(seq)) => seq > sent,
        _ => true,
    };
    let changed = fingerprint != watch.fingerprint;
    let settled = matches!(state, "idle" | "done");
    // Only settling is a stop: a settled worker whose screen moves is not;
    // a worker that settles after a stretch without progress is.
    let stopped = settled && (watch.idle_since == 0 || watch.stuck);
    // An idle stretch starts when the worker settles (settle checks run
    // inside it) or, while it works, at its last progress once it has shown
    // none for `idle_secs`; progress or a dialog ends it.
    if stopped {
        watch.idle_since = now;
        watch.idle_noticed = false;
        watch.stuck = false;
        watch.retry = None;
    } else if state == "blocked" || (working && changed) {
        watch.idle_since = 0;
        watch.idle_noticed = false;
        watch.stuck = false;
        watch.retry = None;
    }
    if changed {
        watch.last_change = now;
        watch.fingerprint = fingerprint;
        watch.held = None;
        if moved_since_delivery
            && matches!(
                live["agent_status"].as_str(),
                Some("idle" | "done" | "blocked" | "unknown")
            )
        {
            watch.pending = Some(format!("target state changed to {}", live["agent_status"]));
            // Stopping after new work is news the manager acts on at once.
            watch.pending_stop |= stopped;
        }
    }
    if watch.held.is_none() && now.saturating_sub(watch.last_change) >= watch.idle_secs as i64 {
        if working {
            if watch.idle_since == 0 {
                watch.idle_since = watch.last_change;
                watch.stuck = true;
            }
            watch.retry = crate::signals::retry_line(screen);
        }
        if watch.repeated() {
            watch.clear_pending();
            return Seen::Stalled;
        }
        // A wake still waiting (settle checks, a provider error) says more than
        // the time without change, so it is kept.
        let idle = now - watch.last_change;
        watch.pending.get_or_insert_with(|| {
            if working {
                // A long silent tool run looks the same: only a retry notice
                // names a cause.
                let cause = match &watch.retry {
                    Some(line) => format!("; the provider keeps retrying ({line})"),
                    None => "; it may be a long silent tool run or a hang".into(),
                };
                format!(
                    "{} is working but has shown no progress for {}{cause}; check whether it needs a hand",
                    watch.role,
                    crate::watch_view::duration(idle)
                )
            } else if moved_since_delivery {
                format!(
                    "no observed target change for {idle} seconds; inspect cause and choose the next useful action"
                )
            } else {
                format!(
                    "the worker's state has not changed since the last delivery ({idle} seconds of no change); check that it received the assignment before resending"
                )
            }
        });
    }
    if changed {
        Seen::Changed
    } else {
        Seen::Unchanged
    }
}
/// Holds automatic wakeups until the original screen changes and tells the
/// user, who is the only one able to resolve the cause.
fn hold(store: &Store, watch: &mut Watch, cause: &str, reason: String) -> Result<()> {
    if watch.held_cause.as_deref() != Some(cause) {
        store.record(
            &watch.goal,
            "attention",
            json!({"reason": reason, "cause": cause, "pane": watch.origin.pane}),
        )?;
        watch.held_cause = Some(cause.into());
    }
    watch.held = Some(reason);
    // The observation that found the cause may have queued a wake ("state
    // changed to idle"); the manager learns nothing it can act on from it.
    watch.clear_pending();
    Ok(())
}
/// What the watch types into a settled worker after a provider error ended its
/// turn. Fixed in the binary; it is not an assignment and adds no work.
pub(crate) const PROVIDER_CONTINUE: &str =
    "Continue the current assignment from where it stopped; do not redo finished steps.";
/// Reacts to why a settled original agent stopped. After a transient provider
/// error the watch types the fixed continue itself (within the hourly cap) and
/// the record travels with the next wake; repeated errors, sign-in, limits and
/// dialogs go to the user and hold further wakeups. Returns whether the
/// continue went out.
async fn classify_stop(
    store: &Store,
    watch: &mut Watch,
    events: &[crate::events::Event],
    live: &Value,
    screen: &str,
    now: i64,
) -> Result<bool> {
    match stop_reason(live, screen) {
        Some(Stop::Dialog) => {
            let reason = format!(
                "pane {} shows an approval or question (state {})",
                watch.origin.pane, live["agent_status"]
            );
            hold(store, watch, "dialog", reason).map(|()| false)
        }
        Some(Stop::Login(line)) => hold(
            store,
            watch,
            "login",
            format!("main needs you to sign in again: {line}"),
        )
        .map(|()| false),
        Some(Stop::Limit(line)) => hold(
            store,
            watch,
            "limit",
            format!("main reached a usage limit: {line}; decide whether to wait or switch"),
        )
        .map(|()| false),
        Some(Stop::ProviderError(line)) => {
            let recent = events
                .iter()
                .filter(|e| e.kind == "provider_error" && e.at.timestamp() >= now - 3600)
                .count();
            if recent >= MAX_PROVIDER_ERRORS_PER_HOUR {
                return hold(
                    store,
                    watch,
                    "provider_errors",
                    format!(
                        "main keeps ending turns with provider errors ({} in the last hour): {line}",
                        recent + 1
                    ),
                )
                .map(|()| false);
            }
            store.record(&watch.goal, "provider_error", json!({ "line": line }))?;
            watch.held_cause = None;
            // A native goal retries a transient error itself: one loop only.
            if native_driving(watch, screen) {
                return Ok(false);
            }
            // Only into an empty input line of a settled agent, never a draft:
            // as observed, and again right before typing.
            if can_prompt(live, screen) {
                let (live, fresh) = fresh_worker(&watch.origin.pane).await?;
                if can_prompt(&live, &fresh) {
                    return continue_after_error(store, watch, &live, &line).await;
                }
            }
            watch.pending = Some(format!(
                "main's turn ended with a provider error ({line}) and its input is not free for the watch's continue; if its assignment is unfinished, send a short continue with plain send instead of replaying the assignment"
            ));
            watch.pending_stop = true;
            Ok(false)
        }
        None => {
            // The cause left the screen: the next one is news again.
            watch.held_cause = None;
            Ok(false)
        }
    }
}
/// Types [`PROVIDER_CONTINUE`] into the watched worker and records it. A
/// delivery that cannot be confirmed is reported to the manager, never retried.
async fn continue_after_error(
    store: &Store,
    watch: &mut Watch,
    live: &Value,
    line: &str,
) -> Result<bool> {
    let pane = watch.origin.pane.clone();
    match herdr::query(&args(&["agent", "prompt", &pane, PROVIDER_CONTINUE])).await {
        Ok(_) => {
            let agent = live["agent"].as_str().unwrap_or("");
            let consumed = crate::supervision::input_consumed(&pane, agent)
                .await
                .unwrap_or(false);
            if consumed {
                store.record(
                    &watch.goal,
                    "provider_continue",
                    json!({"line": line, "text": PROVIDER_CONTINUE, "role": watch.role, "consumed": true}),
                )?;
                // The next wake carries the record; nothing wakes the manager now.
                watch.clear_pending();
                return Ok(true);
            }
            // The input line did not empty: the continue may sit there or
            // have gone into something else on screen. Not typed again.
            store.record(
                &watch.goal,
                "provider_continue",
                json!({"line": line, "text": PROVIDER_CONTINUE, "role": watch.role, "consumed": false, "delivery": "uncertain"}),
            )?;
            watch.pending = Some(format!(
                "main's turn ended with a provider error ({line}); the watch typed its continue but main's input line did not empty, so it may not have arrived; inspect main before sending anything"
            ));
            watch.pending_stop = true;
            Ok(false)
        }
        Err(error) => {
            store.record(
                &watch.goal,
                "provider_continue",
                json!({"line": line, "role": watch.role, "delivery": "uncertain", "error": format!("{error:#}")}),
            )?;
            watch.pending = Some(format!(
                "main's turn ended with a provider error ({line}); the watch's continue may not have arrived ({error:#}); inspect main before sending anything"
            ));
            watch.pending_stop = true;
            Ok(false)
        }
    }
}
/// The attention cause for a worker settled short of completion too long.
pub(crate) const IDLE_NOTICE: &str = "idle_no_next_step";
/// Whether a goal still has work for its worker: not awaiting review or
/// finished, not waiting for the person, and not paused.
pub(crate) fn short_of_completion(goal: &crate::model::Goal) -> bool {
    !matches!(
        goal.status,
        GoalStatus::ReadyForReview | GoalStatus::Completed | GoalStatus::Cancelled
    ) && goal.waiting.is_none()
        && goal.control == Control::Run
}
/// Tells the person, once per idle stretch, when the worker has sat short of
/// completion longer than `idle_notice_secs`: settled with no next step
/// prepared, or working without progress. Keeping it working is the
/// manager's job, so a stretch that long needs a look. A hold already told
/// them why it stopped.
fn notice_idle(
    store: &Store,
    watch: &mut Watch,
    goal: &crate::model::Goal,
    prepared: bool,
    now: i64,
) -> Result<()> {
    let Some(idle) = watch.idle_for(now) else {
        return Ok(());
    };
    if watch.idle_noticed
        || watch.held.is_some()
        || idle < watch.idle_notice_secs as i64
        || !short_of_completion(goal)
        || (prepared && !watch.stuck)
    {
        return Ok(());
    }
    watch.idle_noticed = true;
    let duration = crate::watch_view::duration(idle);
    let reason = if watch.stuck {
        // Failure is not assumed: a long tool run looks the same.
        let cause = watch
            .retry
            .as_ref()
            .map(|line| format!(" (the provider keeps retrying: {line})"))
            .unwrap_or_default();
        format!(
            "{} has been working without progress for {duration}{cause}; it needs a look",
            watch.role
        )
    } else {
        format!(
            "{} has been idle for {duration} short of completion and no next step is prepared; ask the Goal Manager to send the next step, or tell it what to wait for",
            watch.role
        )
    };
    store.record(
        &goal.id,
        "attention",
        json!({
            "reason": reason,
            "cause": IDLE_NOTICE,
            "role": watch.role,
            "idle_secs": idle,
            "working": watch.stuck,
            "retry": watch.retry,
            "pane": watch.origin.pane,
        }),
    )
}
/// Whether the pending wake may go now. A stop after new work goes at once;
/// the cooldown spaces patrols, unchanged screens and other notes.
pub(crate) fn wake_ready(watch: &Watch, now: i64) -> bool {
    watch.active
        && watch.held.is_none()
        && watch.pending.is_some()
        && watch.wakes < watch.max_wakes
        && (watch.pending_stop || now.saturating_sub(watch.last_sent) >= watch.cooldown_secs as i64)
}
/// Whether work reached the worker after the pending wake's observation: an
/// assignment, correction or goal check for a goal this watch serves, or for
/// any goal whose delivery went to the watched pane (one that could not join
/// the watch). The manager has acted, so the wake describes a past state.
/// Dates a pending wake: the state it describes was seen when the worker last
/// changed, so work sent after that makes it stale, and a change seen after
/// such work makes it current again. Natively (2026-11-15) a wake held behind
/// a manager draft kept its first date, and the settle after newer work, which
/// replaced its text, was dropped with it as stale.
fn stamp_pending(watch: &mut Watch, now: i64) {
    if watch.pending.is_none() {
        watch.clear_pending();
    } else if watch.pending_at == 0 {
        watch.pending_at = now;
    } else {
        watch.pending_at = watch.pending_at.max(watch.last_change);
    }
}
fn superseded(store: &Store, watch: &Watch) -> Result<bool> {
    if watch.pending_at == 0 {
        return Ok(false);
    }
    for goal in store.list()? {
        let member = watch.serves(&goal.id);
        let after = store.events(&goal.id)?.into_iter().any(|event| {
            matches!(event.kind.as_str(), "assigned" | "correction" | "reanchor")
                && event.at.timestamp() > watch.pending_at
                && (member || event.detail["pane"].as_str() == Some(&watch.origin.pane))
        });
        if after {
            return Ok(true);
        }
    }
    Ok(false)
}
/// Whether a working manager takes typed input as a queued message, under
/// the rules of `send --interrupt`: a provider known to queue it, an empty
/// input line and no dialog.
pub(crate) fn takes_queued_input(manager: &Value, screen: &str) -> bool {
    let agent = manager["agent"].as_str().unwrap_or("");
    crate::supervision::QUEUES_WHILE_WORKING.contains(&agent)
        && (manager["agent_status"] == "working" || busy_on_screen(screen, agent))
        && input_refusal(manager, screen).is_none()
}
/// When the worker settles after new work, runs the goal's command checks so a
/// missed acceptance turns into a correction now rather than at review.
async fn settle_checks(
    store: &Store,
    watch: &mut Watch,
    goal: &crate::model::Goal,
    events: &[crate::events::Event],
) -> Result<()> {
    if !goal.spec.loop_policy.check_on_settle || goal.spec.checks.is_empty() {
        return Ok(());
    }
    let Some(latest) = events
        .iter()
        .rev()
        .find(|e| matches!(e.kind.as_str(), "assigned" | "correction"))
        .map(|e| e.at)
    else {
        return Ok(());
    };
    if watch.checked_through.is_some_and(|done| done >= latest) {
        return Ok(());
    }
    watch.checked_through = Some(latest);
    // The state these checks judge, kept with their result.
    let fingerprint = crate::fingerprint::digest(goal.work_dir()?).ok();
    let (mut passed, mut failed) = (vec![], vec![]);
    for check in &goal.spec.checks {
        let command = crate::model::CommandSpec {
            program: "sh".into(),
            args: vec!["-c".into(), check.command.clone()],
            prompt_stdin: false,
        };
        let id = format!("settle-{}", uuid::Uuid::new_v4());
        match crate::process::execute(
            store,
            goal.work_dir()?,
            &id,
            &command,
            "",
            check.timeout_secs,
        )
        .await
        {
            Ok(_) => passed.push(check.name.clone()),
            Err(error) => {
                let detail: String = format!("{error:#}").chars().take(300).collect();
                failed.push(json!({"name": check.name, "detail": detail}));
            }
        }
    }
    store.record(
        &goal.id,
        "settle_checks",
        json!({"role": watch.role, "passed": passed, "failed": failed, "fingerprint": fingerprint}),
    )?;
    // Notes from this observation (a native goal's outcome read as the
    // worker settled) stay behind the settle note; the generic state-change
    // note says nothing the settle note does not.
    let note = settle_note(&watch.role, &failed, events);
    let earlier = watch.pending.take().map(|earlier| {
        if earlier.starts_with("target state changed") {
            earlier
                .split_once("; ")
                .map(|(_, rest)| rest.to_string())
                .unwrap_or_default()
        } else {
            earlier
        }
    });
    watch.pending = Some(match earlier.filter(|e| !e.is_empty()) {
        Some(earlier) => format!("{note}; {earlier}"),
        None => note,
    });
    watch.pending_stop = true;
    Ok(())
}
/// The wake note after settle checks. A failing check asks for a correction
/// only when it passed at an earlier settle of the goal (`events`): a check
/// that never passed may only show unfinished work, so it is listed instead.
fn settle_note(role: &str, failed: &[Value], events: &[crate::events::Event]) -> String {
    if failed.is_empty() {
        return format!(
            "{role} settled and every command check passes; inspect the result against the acceptance, then record evidence and prepare review"
        );
    }
    let passed_before = |name: &str| {
        events.iter().any(|event| {
            event.kind == "settle_checks"
                && event.detail["passed"]
                    .as_array()
                    .is_some_and(|passed| passed.iter().any(|p| p.as_str() == Some(name)))
        })
    };
    let (regressed, unfinished): (Vec<&Value>, Vec<&Value>) = failed
        .iter()
        .partition(|check| passed_before(check["name"].as_str().unwrap_or("")));
    let summary = |checks: &[&Value]| {
        checks
            .iter()
            .map(|check| {
                format!(
                    "{}: {}",
                    check["name"].as_str().unwrap_or(""),
                    check["detail"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join(" | ")
    };
    if regressed.is_empty() {
        return format!(
            "{role} settled; checks not yet passing ({}). None has passed for this goal yet, so this may be unfinished work: inspect the result and send the next step only if the work should have made them pass",
            summary(&unfinished)
        );
    }
    let mut note = format!(
        "{role} settled but checks that passed before now fail ({}). Tell it what is off with hgm correct (or a linked fix assignment) instead of waiting for review",
        summary(&regressed)
    );
    if !unfinished.is_empty() {
        let names: Vec<&str> = unfinished
            .iter()
            .filter_map(|check| check["name"].as_str())
            .collect();
        note.push_str(&format!(". Not yet passing: {}", names.join(", ")));
    }
    note
}
/// The worker's native state in one `agent get`; the pane inventory (two
/// listings) answers when that fails, e.g. for a pane without an agent.
pub(crate) async fn worker_state(pane: &str) -> Result<Value> {
    match herdr::json_query(&args(&["agent", "get", pane])).await {
        Ok(reply) if reply["result"]["agent"]["pane_id"].as_str() == Some(pane) => {
            Ok(reply["result"]["agent"].clone())
        }
        _ => live_pane(pane).await,
    }
}
/// The worker's state and screen read again right before the watch types into
/// it. The observation that decided to type can be seconds old, since checks
/// run after it: Claude Code 2.1.325 opened a dialog in that time ("Teach auto
/// mode about your environment?", listed by Herdr as done), and a line typed
/// from the earlier observation went into it (2026-11-15).
pub(crate) async fn fresh_worker(pane: &str) -> Result<(Value, String)> {
    let mut live = worker_state(pane).await?;
    let screen = crate::binding::screen(pane).await?;
    crate::signals::apply_screen_state(&mut live, &screen);
    Ok((live, screen))
}
/// The worker's `state_change_seq` when the latest delivery to `role` was sent.
fn delivery_seq(events: &[crate::events::Event], role: &str) -> Option<u64> {
    events
        .iter()
        .rev()
        .find(|e| {
            matches!(e.kind.as_str(), "assigned" | "correction" | "reanchor")
                && e.detail["to"].as_str().unwrap_or("main") == role
        })
        .and_then(|e| e.detail["seq"].as_u64())
}
/// Judges one observation of the worker (`target` state and `screen`) and wakes
/// the manager when something needs it.
pub(crate) async fn watch_once(
    store: &Store,
    watch: &mut Watch,
    target: &Value,
    screen: &str,
) -> Result<()> {
    let now = Utc::now().timestamp();
    watch.origin.check(target)?;
    ensure!(
        target.get("agent").is_some(),
        "original agent exited; no automatic replacement"
    );
    watch.last_observation_at = now;
    let goal = store.load(&watch.goal)?;
    let events = store.events(&goal.id)?;
    let was_stuck = watch.stuck;
    let seen = observe(
        watch,
        target,
        screen,
        now,
        delivery_seq(&events, &watch.role),
    );
    if watch.stuck && !was_stuck {
        // Whether the files still move while the screen does not.
        let changed_at = goal.work_dir().ok().and_then(|dir| {
            crate::fingerprint::Snapshot::take(dir)
                .ok()
                .and_then(|snapshot| snapshot.last_change(dir))
        });
        note_work_dir(watch, work_dir_note(changed_at, watch.idle_since, now));
        store.record(
            &goal.id,
            "no_progress",
            json!({"role": watch.role, "since": watch.idle_since, "retry": watch.retry,
                   "work_dir_changed_at": changed_at}),
        )?;
    }
    confirm_pending_goal(store, watch, target, screen, now)?;
    notice_unseen_end(store, watch, target, screen, now)?;
    if seen == Seen::Stalled {
        let reason = format!(
            "main shows no change after {MAX_SAME_WAKES} manager wakes; inspect it (it may be stuck, finished, or waiting for you)"
        );
        hold(store, watch, "stalled", reason)?;
    }
    if seen == Seen::Changed {
        observe_native(store, watch, screen)?;
        let continued = classify_stop(store, watch, &events, target, screen, now).await?;
        let settled = matches!(target["agent_status"].as_str(), Some("idle" | "done"));
        if settled && watch.held.is_none() {
            compare_scope(store, watch, &goal, &events);
        }
        if !continued && watch.held.is_none() && settled {
            settle_checks(store, watch, &goal, &events).await?;
        }
        if let Some(line) = compaction_shown(screen) {
            let recent = events
                .iter()
                .rev()
                .any(|e| e.kind == "compaction_detected" && e.at.timestamp() >= now - 600);
            if !recent {
                store.record(
                    &watch.goal,
                    "compaction_detected",
                    json!({"line": line, "role": watch.role}),
                )?;
            }
        }
        for (signal, line) in rot_signals(screen) {
            if !watch.count_rot(signal, &line) {
                continue;
            }
            store.record(
                &watch.goal,
                "rot_signal",
                json!({ "signal": signal, "line": line }),
            )?;
        }
        if watch.rot_signals >= ROT_SIGNS_PER_WAKE {
            rot_wake(watch);
        }
    }
    // The worker drives itself only while the goal has work for it.
    if !short_of_completion(&goal) {
        let reason = format!("goal {} is {:?}", goal.id, goal.status);
        crate::native_goal::clear(store, watch, &reason).await?;
    }
    if watch.wakes >= watch.max_wakes {
        // The budget is the goal's own: only it waits, other goals stay watched.
        let reason = format!(
            "{WAKE_BUDGET_SPENT}: the watch spent its {} wakes; decide whether to continue (supervise with a larger --max-wakes), revise, or stop",
            watch.max_wakes
        );
        crate::native_goal::clear(store, watch, WAKE_BUDGET_SPENT).await?;
        if let Err(error) = store.update(&goal.id, |goal| goal.wait(reason.clone())) {
            eprintln!("Could not set the goal waiting: {error:#}");
        }
        drop_goal(store, watch, &goal.id, &reason, true)?;
        return Ok(());
    }
    let policy = &goal.spec.loop_policy;
    if let Some(baseline) = &goal.protected_digest
        && !goal.spec.protected_paths.is_empty()
        && crate::contract::protected_digest(goal.work_dir()?, &goal.spec.protected_paths)?
            != *baseline
    {
        let reason = format!(
            "Protected paths changed: {}. Revert the change, or confirm the goal again if you accept it.",
            goal.spec.protected_paths.join(", ")
        );
        crate::native_goal::clear(store, watch, "protected paths changed").await?;
        store.record(
            &goal.id,
            "boundary_crossed",
            json!({ "paths": goal.spec.protected_paths }),
        )?;
        store.update(&goal.id, |g| g.wait(reason.clone()))?;
        end_watch(store, watch, "boundary_crossed", false);
        return Ok(());
    }
    let first_assignment = events.iter().find(|e| e.kind == "assigned").map(|e| e.at);
    if let (Some(hours), Some(first)) = (policy.max_hours, first_assignment)
        && now - first.timestamp() >= i64::from(hours) * 3600
    {
        let reason = format!(
            "Time budget of {hours} h since the first assignment is spent. Decide whether to continue, revise, or stop."
        );
        crate::native_goal::clear(store, watch, TIME_BUDGET_SPENT).await?;
        store.update(&goal.id, |g| g.wait(reason.clone()))?;
        end_watch(store, watch, TIME_BUDGET_SPENT, false);
        return Ok(());
    }
    // A fresh native goal starts its own turn; a prepared step waits for the
    // worker to settle again, so one loop drives it at a time.
    if !set_native(store, watch, &goal, target, screen).await?
        && deliver_prepared(store, watch, &goal, target, screen).await?
    {
        return Ok(());
    }
    // Re-read the goal periodically and after compaction, restart or scope
    // change. Delivered once per trigger; held states wait for a change.
    let events = store.events(&goal.id)?;
    if watch.held.is_none()
        && let Some((reason, at)) = crate::admission::reanchor_due(&goal, &events, Utc::now())
        && watch.reanchored_for != Some(at)
    {
        watch.reanchored_for = Some(at);
        let note = format!(
            "goal check due ({reason}): run hgm reanchor {} so {} restates the goal and records it with hgm report (it reaches a working Claude Code or Copilot now, other agents when their turn settles); the restatement is not evidence: compare it with the contract and correct drift with hgm correct",
            goal.id, watch.role
        );
        watch.pending = Some(match watch.pending.take() {
            Some(pending) => format!("{pending}; also {note}"),
            None => note,
        });
    }
    let patrol = watch.patrol(&goal);
    // Stop checks run at every patrol, whether or not it wakes anyone: a quiet
    // patrol (`patrol_wake: finding`) would otherwise leave a stop condition
    // unseen until another cause wakes the manager.
    let mut stop_checked = false;
    if patrol > 0
        && watch.pending.is_none()
        && watch.held.is_none()
        && target["agent_status"] == "working"
        && now - watch.last_patrol >= i64::from(patrol) * 60
    {
        // Progress is visible when the screen changed since the previous patrol.
        let progressed = watch.last_change > watch.last_patrol;
        watch.last_patrol = now;
        if stop_condition(store, watch, &goal).await? {
            return Ok(());
        }
        stop_checked = true;
        if watch.repeated() {
            hold(
                store,
                watch,
                "stalled",
                format!(
                    "main shows the same working screen after {MAX_SAME_WAKES} checks; inspect it"
                ),
            )?;
            return Ok(());
        }
        let note = format!(
            "{patrol}-minute check of {}: see whether it progresses toward the assignment or is stuck, looping or drifting",
            watch.role
        );
        watch.pending = Some(note.clone());
        compare_scope(store, watch, &goal, &events);
        // No progress, a stuck worker, a context-rot sign or a regressed check
        // each make their own wake; here only a write-scope note can join.
        let finding = watch.pending.as_deref() != Some(note.as_str());
        let quiet = goal.spec.loop_policy.patrol_wake == crate::contract::PatrolWake::Finding
            && progressed
            && !finding;
        let mut detail =
            json!({"reason": format!("{patrol}-minute check of {} while it works", watch.role)});
        if quiet {
            detail["quiet"] = json!(true);
            watch.clear_pending();
        }
        store.record(&goal.id, "patrol", detail)?;
    }
    stamp_pending(watch, now);
    let prepared = crate::prepared::load(store, &watch.context, &watch.role)?.is_some();
    notice_idle(store, watch, &goal, prepared, now)?;
    if !wake_ready(watch, now) {
        return Ok(());
    }
    if superseded(store, watch)? {
        watch.clear_pending();
        return Ok(());
    }
    if !stop_checked && stop_condition(store, watch, &goal).await? {
        return Ok(());
    }
    if hand_to_await(store, watch, &goal, &events, target, screen, now)? {
        return Ok(());
    }
    let manager = live_pane(&watch.manager.pane).await?;
    watch.manager.check(&manager)?;
    let manager_screen = crate::binding::screen_lines(&watch.manager.pane, 30).await?;
    let free = can_prompt(&manager, &manager_screen);
    // A stop after new work also reaches a busy manager that queues input.
    let queued = !free && watch.pending_stop && takes_queued_input(&manager, &manager_screen);
    if !free && !queued {
        watch.status = "waiting_for_manager".into();
        if watch.manager_unavailable_since == 0 {
            watch.manager_unavailable_since = now;
        }
        // Nobody answers the manager's own approval dialog unattended; queued
        // wakes would wait silently, so the user is told once.
        if !watch.manager_notified
            && now - watch.manager_unavailable_since >= watch.idle_secs as i64
            && needs_human(&manager, &manager_screen)
        {
            watch.manager_notified = true;
            store.record(
                &goal.id,
                "attention",
                json!({
                    "reason": format!("the Goal Manager (pane {}) waits for your approval or answer; its wakeups are queued", watch.manager.pane),
                    "cause": "manager_blocked",
                    "pane": watch.manager.pane,
                }),
            )?;
        }
        return Ok(());
    }
    watch.manager_unavailable_since = 0;
    watch.manager_notified = false;
    if store.load(&goal.id)?.control != Control::Run {
        drop_goal(store, watch, &goal.id, "stopped_by_goal", false)?;
        return Ok(());
    }
    let (output, anchor) = new_output(screen, &watch.wake_anchor);
    let notice = wake_notice(&goal, &events, watch, &target["agent_status"], &output)
        + &crate::config::wake_extra(&store.workspace);
    watch.wake_anchor = anchor;
    let reason = watch.pending.clone().unwrap_or_default();
    watch.mark_wake(now);
    watch.status = "wake_delivery_uncertain".into();
    save_watch(store, watch)?;
    herdr::query(&args(&["agent", "prompt", &watch.manager.pane, &notice]))
        .await
        .context("manager wake admission uncertain; no automatic resend")?;
    // As with send: the input line must empty again. Text left there is
    // reported, never typed a second time.
    let agent = manager["agent"].as_str().unwrap_or("");
    let consumed = crate::supervision::input_consumed(&watch.manager.pane, agent)
        .await
        .unwrap_or(false);
    if !consumed {
        eprintln!(
            "The wake typed into the Goal Manager may still sit in its input; it is not resent."
        );
    }
    let via = match (queued, consumed) {
        (true, true) => "queued",
        (true, false) => "queued_unconfirmed",
        (false, true) => "prompt",
        (false, false) => "prompt_unconfirmed",
    };
    watch.status = "watching".into();
    record_wake(store, watch, &reason, via)
}
/// Hands the ready wake to the manager's running `hgm await` instead of typing
/// it: the same notice, so a draft in the manager's input no longer holds it
/// back. Returns whether the step ends here (handed, or an earlier handed
/// wake still waits to be taken); without an await the wake is typed as before.
fn hand_to_await(
    store: &Store,
    watch: &mut Watch,
    goal: &crate::model::Goal,
    events: &[crate::events::Event],
    target: &Value,
    screen: &str,
    now: i64,
) -> Result<bool> {
    use crate::wake_await::Offer;
    if !crate::wake_await::running(store, &watch.context)? {
        return Ok(false);
    }
    if store.load(&goal.id)?.control != Control::Run {
        drop_goal(store, watch, &goal.id, "stopped_by_goal", false)?;
        return Ok(true);
    }
    let (output, anchor) = new_output(screen, &watch.wake_anchor);
    let notice = wake_notice(goal, events, watch, &target["agent_status"], &output)
        + &crate::config::wake_extra(&store.workspace);
    match crate::wake_await::hand_over(store, &watch.context, &goal.id, &notice)? {
        Offer::NoAwait => Ok(false),
        Offer::Busy => Ok(true),
        Offer::Taken => {
            watch.wake_anchor = anchor;
            let reason = watch.pending.clone().unwrap_or_default();
            watch.mark_wake(now);
            watch.manager_unavailable_since = 0;
            watch.manager_notified = false;
            record_wake(store, watch, &reason, "await")?;
            Ok(true)
        }
    }
}
/// Whether the native goal hgm set still drives the worker: no outcome yet and
/// the screen shows it active or retrying by itself.
fn native_driving(watch: &Watch, screen: &str) -> bool {
    use crate::signals::NativeGoal as N;
    watch
        .native_goal
        .as_ref()
        .is_some_and(|state| state.outcome.is_none())
        && matches!(
            crate::signals::native_goal(screen),
            Some(N::Active | N::Retrying(_))
        )
}
/// Records what the native goal shows on screen: its outcome (met, judged
/// impossible, paused, cleared after an error or by hand) or that `/goal` is
/// unavailable. Each is an observation only; an outcome wakes the manager as
/// for any stop, and no task completes or goal is accepted by it.
/// Settles a native goal queued with the assignment: set once the worker shows
/// `Goal set:` for this goal and task, uncertain when the worker has sat
/// settled without it (the queued line runs as soon as the assignment's turn
/// ends, so a few seconds are enough). An uncertain one is never typed again.
fn confirm_pending_goal(
    store: &Store,
    watch: &mut Watch,
    target: &Value,
    screen: &str,
    now: i64,
) -> Result<()> {
    const GRACE_SECS: i64 = 10;
    let Some(state) = watch
        .native_goal
        .clone()
        .filter(|s| s.outcome.as_deref() == Some("pending"))
    else {
        return Ok(());
    };
    let head = format!(
        "hgm goal {} task {}",
        state.goal,
        state.task.as_deref().unwrap_or("(goal)")
    );
    let read = screen
        .lines()
        .any(|line| line.contains("Goal set:") && line.contains(&head));
    let settled = matches!(target["agent_status"].as_str(), Some("idle" | "done"))
        && now - state.set_at >= GRACE_SECS
        && now - watch.last_change >= GRACE_SECS / 2;
    if read {
        store.record(
            &state.goal,
            "native_goal_set",
            json!({"task": state.task, "via": "queued", "consumed": true}),
        )?;
        watch.native_goal = Some(crate::native_goal::NativeGoalState {
            outcome: None,
            ..state
        });
    } else if settled {
        store.record(
            &state.goal,
            "native_goal_set",
            json!({"task": state.task, "delivery": "uncertain", "why": "the worker settled without Goal set:"}),
        )?;
        append_note(
            watch,
            "the native goal queued with the assignment never showed Goal set:; it is not typed again".into(),
        );
        watch.native_goal = Some(crate::native_goal::NativeGoalState {
            outcome: Some("uncertain".into()),
            ..state
        });
    }
    Ok(())
}
/// Records the end of a set native goal whose outcome showed where reading
/// from the bottom does not find it: a goal queued with the assignment and met
/// at the stop that ran it prints its outcome above the echoed `Goal set:`
/// (seen natively on the Goal Manager, 2026-11-16). A settled worker without
/// the `◎ /goal active` indicator above its input line runs no goal. Checked at
/// every observation, since the screen may not change again; the outcome is an
/// observation only, and a pending wake carries it.
fn notice_unseen_end(
    store: &Store,
    watch: &mut Watch,
    target: &Value,
    screen: &str,
    now: i64,
) -> Result<()> {
    use crate::signals::NativeGoal as N;
    const GRACE_SECS: i64 = 10;
    let Some(state) = watch.native_goal.clone().filter(|s| s.outcome.is_none()) else {
        return Ok(());
    };
    let settled = matches!(target["agent_status"].as_str(), Some("idle" | "done"));
    // An outcome below the newest `Goal set:` is read by `observe_native`.
    let below = !matches!(
        crate::signals::native_goal(screen),
        Some(N::Active | N::Retrying(_)) | None
    );
    if !settled
        || below
        || now - state.set_at < GRACE_SECS
        || crate::signals::goal_indicator_shown(screen)
    {
        return Ok(());
    }
    let (outcome, line) = match crate::signals::unseen_outcome(screen) {
        Some(N::Met(line)) => ("met", line),
        Some(N::Impossible(line)) => ("impossible", line),
        Some(N::ErrorCleared(line)) => ("error_cleared", line),
        Some(N::Cleared(line)) => ("cleared", line),
        _ => (
            "ended",
            "no /goal active indicator above the input line".to_string(),
        ),
    };
    store.record(
        &state.goal,
        "native_goal",
        json!({"outcome": outcome, "line": line, "task": state.task, "read": "indicator"}),
    )?;
    if watch.pending.is_some() {
        append_note(
            watch,
            format!(
                "the native goal ended ({outcome}: {line}); that is the provider's verdict, not evidence: check the result against the contract"
            ),
        );
    }
    watch.native_goal = Some(crate::native_goal::NativeGoalState {
        outcome: Some(outcome.into()),
        ..state
    });
    Ok(())
}
fn observe_native(store: &Store, watch: &mut Watch, screen: &str) -> Result<()> {
    use crate::signals::NativeGoal as N;
    let Some(state) = watch.native_goal.clone().filter(|s| s.is_set()) else {
        return Ok(());
    };
    let (outcome, line) = match crate::signals::native_goal(screen) {
        Some(N::Met(line)) => ("met", line),
        Some(N::Impossible(line)) => ("impossible", line),
        Some(N::Paused(line)) => ("paused", line),
        Some(N::ErrorCleared(line)) => ("error_cleared", line),
        Some(N::Cleared(line)) => ("cleared", line),
        Some(N::Unavailable(line)) => {
            store.record(
                &state.goal,
                "native_goal_unavailable",
                json!({"line": line}),
            )?;
            watch.native_unavailable = true;
            watch.native_goal = Some(crate::native_goal::NativeGoalState {
                outcome: Some("unavailable".into()),
                ..state
            });
            return Ok(());
        }
        Some(N::Active | N::Retrying(_)) | None => return Ok(()),
    };
    if state.outcome.as_deref() == Some(outcome) {
        return Ok(());
    }
    store.record(
        &state.goal,
        "native_goal",
        json!({"outcome": outcome, "line": line, "task": state.task}),
    )?;
    append_note(
        watch,
        format!(
            "the native goal reports {outcome} ({line}); that is the provider's verdict, not evidence: check the result against the contract"
        ),
    );
    watch.pending_stop = true;
    watch.native_goal = Some(crate::native_goal::NativeGoalState {
        outcome: Some(outcome.into()),
        ..state
    });
    Ok(())
}
/// Sets the native goal once the worker has settled after a linked non-plan
/// assignment that has none yet, when the contract wants one and nothing
/// holds. Setting it starts a turn at once. Returns whether it was set.
async fn set_native(
    store: &Store,
    watch: &mut Watch,
    goal: &crate::model::Goal,
    target: &Value,
    screen: &str,
) -> Result<bool> {
    let agent = target["agent"].as_str().unwrap_or("");
    if watch.role != "main"
        || watch.native_unavailable
        || watch.held.is_some()
        || !crate::native_goal::enabled(goal, agent)
        || !short_of_completion(goal)
        || !can_prompt(target, screen)
    {
        return Ok(false);
    }
    let events = store.events(&goal.id)?;
    let Some(assigned) = events
        .iter()
        .rev()
        .find(|e| e.kind == "assigned" && e.detail["to"].as_str().unwrap_or("main") == watch.role)
    else {
        return Ok(false);
    };
    let done = watch
        .native_goal
        .as_ref()
        .is_some_and(|state| state.goal == goal.id && state.assigned_at >= assigned.at);
    if assigned.detail["plan"] == true || done {
        return Ok(false);
    }
    // The settle checks ran since the observation: look again before typing.
    let (live, fresh) = fresh_worker(&watch.origin.pane).await?;
    if !can_prompt(&live, &fresh) {
        return Ok(false);
    }
    let task = assigned.detail["task"].as_str().map(String::from);
    crate::native_goal::set(store, watch, goal, task.as_deref(), assigned.at, agent).await?;
    append_note(
        watch,
        format!(
            "the watch set the native goal for {} on {}: it keeps working by itself until the goal is met, judged impossible or paused; its verdict is an observation, hgm's checks and the user's acceptance decide",
            task.as_deref().unwrap_or("the goal"),
            watch.role
        ),
    );
    Ok(true)
}
/// Runs the goal's stop checks; one that holds makes the goal wait for the
/// person and ends the watch. Returns whether one held.
async fn stop_condition(
    store: &Store,
    watch: &mut Watch,
    goal: &crate::model::Goal,
) -> Result<bool> {
    for check in &goal.spec.loop_policy.stop_checks {
        let command = crate::model::CommandSpec {
            program: "sh".into(),
            args: vec!["-c".into(), check.command.clone()],
            prompt_stdin: false,
        };
        let id = format!("stop-{}", uuid::Uuid::new_v4());
        if crate::process::execute(
            store,
            goal.work_dir()?,
            &id,
            &command,
            "",
            check.timeout_secs,
        )
        .await
        .is_ok()
        {
            let reason = format!(
                "Stop condition {} holds. Decide whether to revise the goal or stop.",
                check.name
            );
            crate::native_goal::clear(store, watch, "a stop condition holds").await?;
            store.record(&goal.id, "stop_condition", json!({ "check": check.name }))?;
            store.update(&goal.id, |g| g.wait(reason.clone()))?;
            end_watch(store, watch, "stop_condition", false);
            return Ok(true);
        }
    }
    Ok(false)
}
/// Checks failing at the latest settle after the latest assignment or
/// correction that passed at an earlier settle of the goal.
fn regressed(events: &[crate::events::Event]) -> Vec<String> {
    let work = events
        .iter()
        .rposition(|e| matches!(e.kind.as_str(), "assigned" | "correction"));
    let Some(settle) = events
        .iter()
        .rposition(|e| e.kind == "settle_checks")
        .filter(|&settle| work.is_none_or(|work| settle > work))
    else {
        return vec![];
    };
    let passed_before = |name: &str| {
        events[..settle].iter().any(|event| {
            event.kind == "settle_checks"
                && event.detail["passed"]
                    .as_array()
                    .is_some_and(|passed| passed.iter().any(|p| p.as_str() == Some(name)))
        })
    };
    events[settle].detail["failed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|check| check["name"].as_str())
        .filter(|name| passed_before(name))
        .map(String::from)
        .collect()
}
/// Adds changes outside the current task's write_paths to the wake. A failed
/// comparison is reported and never ends the watch; writes are never blocked.
fn compare_scope(
    store: &Store,
    watch: &mut Watch,
    goal: &crate::model::Goal,
    events: &[crate::events::Event],
) {
    match crate::scope::check(store, goal, events, &watch.role) {
        Ok(Some(note)) => append_note(watch, note),
        Ok(None) => {}
        Err(error) => eprintln!("Write scope not compared: {error:#}"),
    }
}
/// What the work dir says about a worker that shows no progress since
/// `screen_since`: whether its files changed after the screen stopped.
fn work_dir_note(changed_at: Option<i64>, screen_since: i64, now: i64) -> String {
    let Some(changed) = changed_at else {
        return "when its working directory last changed could not be read".into();
    };
    let at = chrono::DateTime::from_timestamp(changed, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default();
    let ago = crate::watch_view::duration(now.saturating_sub(changed).max(0));
    if changed > screen_since {
        format!(
            "its working directory last changed at {at} ({ago} ago), after the screen stopped: files still change"
        )
    } else {
        format!(
            "its working directory last changed at {at} ({ago} ago), before the screen stopped: neither the screen nor the files move"
        )
    }
}
fn append_note(watch: &mut Watch, note: String) {
    watch.pending = Some(match watch.pending.take() {
        Some(pending) => format!("{pending}; {note}"),
        None => note,
    });
}
/// Adds the work dir note to a pending no-progress wake, replacing an earlier
/// one: a watch reloaded into a new build sees a changed screen and can go
/// stuck again before the wake goes out (seen natively, 2026-11-16).
fn note_work_dir(watch: &mut Watch, note: String) {
    if let Some(pending) = watch.pending.as_mut() {
        for marker in [
            "; its working directory last changed",
            "; when its working directory last changed",
        ] {
            if let Some(start) = pending.find(marker) {
                let end = pending[start + 2..]
                    .find("; ")
                    .map_or(pending.len(), |e| start + 2 + e);
                pending.replace_range(start..end, "");
            }
        }
    }
    append_note(watch, note);
}
/// Keeps the prepared step and tells the manager why, once per reason.
fn keep_prepared(
    store: &Store,
    watch: &mut Watch,
    step: &crate::prepared::Prepared,
    reason: String,
) -> Result<()> {
    if watch.prepared_held.as_deref() == Some(reason.as_str()) {
        return Ok(());
    }
    store.record(
        &step.goal,
        "prepared_held",
        json!({"task": step.task, "to": step.to, "reason": reason}),
    )?;
    append_note(
        watch,
        format!(
            "your prepared step for {} is kept, not delivered: {reason}",
            step.task.as_deref().unwrap_or("the goal")
        ),
    );
    watch.prepared_held = Some(reason);
    Ok(())
}
/// Hands the step the manager prepared for the watched role to the worker
/// once it has settled, through normal admission, when no check regressed, no
/// stop condition, protected path or budget holds, the goal still has work
/// and the worker's input is free; the manager is woken about it all the
/// same. Otherwise the step stays and the manager hears why. A delivery that
/// cannot be confirmed is never repeated. Returns whether a stop condition
/// ended the watch. (Protected paths and the time budget were checked before.)
async fn deliver_prepared(
    store: &Store,
    watch: &mut Watch,
    goal: &crate::model::Goal,
    target: &Value,
    screen: &str,
) -> Result<bool> {
    let Some(step) = crate::prepared::load(store, &watch.context, &watch.role)? else {
        watch.prepared_held = None;
        return Ok(false);
    };
    if let Some(since) = step.sending_at {
        // The watch stopped while typing this step: whether it arrived is
        // unknown, so it is recorded as uncertain and never typed again.
        crate::prepared::remove(store, &watch.context, &watch.role)?;
        store.record(
            &step.goal,
            "prepared_delivery_uncertain",
            json!({"task": step.task, "to": step.to, "error": "the watch stopped while delivering it", "since": since}),
        )?;
        append_note(
            watch,
            format!(
                "the watch stopped while typing your prepared step for {}; it may have arrived and is not typed again: inspect {} before sending anything",
                step.task.as_deref().unwrap_or("the goal"),
                watch.role
            ),
        );
        return Ok(false);
    }
    if !matches!(target["agent_status"].as_str(), Some("idle" | "done")) {
        return Ok(false);
    }
    let step_goal = if step.goal == goal.id {
        goal.clone()
    } else {
        store.load(&step.goal)?
    };
    let regressed = regressed(&store.events(&step.goal)?);
    let hold = if !watch.serves(&step.goal) {
        Some(format!("goal {} is not watched here", step.goal))
    } else if let Some(held) = &watch.held {
        Some(format!("{} is held: {held}", watch.role))
    } else if native_driving(watch, screen) {
        Some(format!("the native goal still drives {}", watch.role))
    } else if !regressed.is_empty() {
        Some(format!(
            "checks that passed before now fail ({})",
            regressed.join(", ")
        ))
    } else if !short_of_completion(&step_goal) {
        Some(format!(
            "goal {} is not running (it awaits review, waits for the person, is paused or finished)",
            step.goal
        ))
    } else if !can_prompt(target, screen) {
        Some(format!(
            "the input of {} is not free (a draft, a dialog or an unknown input line)",
            watch.role
        ))
    } else {
        None
    };
    if let Some(reason) = hold {
        keep_prepared(store, watch, &step, reason)?;
        return Ok(false);
    }
    if stop_condition(store, watch, &step_goal).await? {
        return Ok(true);
    }
    let helper = crate::roles::resolve(store, &step.to)?;
    let task = step.task.as_deref().unwrap_or("the goal");
    crate::prepared::mark_sending(store, &watch.context, &watch.role, Some(Utc::now()))?;
    match crate::supervision::send(store, &step.text, step.assignment(), helper.as_ref()).await {
        Ok(result) => {
            watch.prepared_held = None;
            // A native goal queued behind the step lives in this watch record.
            if let Ok(state) = serde_json::from_value(result["native_goal"].clone()) {
                watch.native_goal = Some(state);
            }
            append_note(
                watch,
                format!(
                    "{} settled, so the watch delivered your prepared step for {task} (it counts against loop.max_assignments; consumed: {}); check that it started",
                    watch.role, result["consumed"]
                ),
            );
        }
        Err(error) if format!("{error:#}").contains(crate::supervision::DELIVERY_UNCONFIRMED) => {
            crate::prepared::remove(store, &watch.context, &watch.role)?;
            store.record(
                &step.goal,
                "prepared_delivery_uncertain",
                json!({"task": step.task, "to": step.to, "error": format!("{error:#}")}),
            )?;
            append_note(
                watch,
                format!(
                    "the watch typed your prepared step for {task} but could not confirm it arrived ({error:#}); inspect {} before sending anything",
                    watch.role
                ),
            );
        }
        Err(error) => {
            // Refused before anything was typed: it may go later.
            crate::prepared::mark_sending(store, &watch.context, &watch.role, None)?;
            keep_prepared(store, watch, &step, format!("{error:#}"))?;
            return Ok(false);
        }
    }
    watch.pending_stop = true;
    // The delivery was recorded after this observation; the wake about it
    // is still current.
    watch.pending_at = Utc::now().timestamp();
    Ok(false)
}
/// Lines on screen since the previous wake, and the anchor for the next one.
/// Only the transcript counts: the input line and what Herdr draws below it
/// (status bars) are left out, as are lines without a letter or digit
/// (rules, frames). The anchor is the last two such lines; when the previous
/// anchor is gone (first wake, cleared or scrolled screen), the last lines stand in.
fn new_output(screen: &str, anchor: &str) -> (String, String) {
    const FALLBACK: usize = 8;
    const MAX_LINES: usize = 20;
    const LINE_CHARS: usize = 200;
    let lines: Vec<&str> = screen.lines().collect();
    let input = lines.iter().rposition(|line| {
        let line = line.trim_start();
        line.starts_with('❯') || line.starts_with('›') || line.starts_with('╻')
    });
    let transcript: Vec<&str> = lines[..input.unwrap_or(lines.len())]
        .iter()
        .map(|line| line.trim_end())
        .filter(|line| line.chars().any(char::is_alphanumeric))
        .collect();
    let next = transcript[transcript.len().saturating_sub(2)..].join("\n");
    let anchored: Vec<&str> = anchor.lines().collect();
    let start = (!anchored.is_empty())
        .then(|| {
            transcript
                .windows(anchored.len())
                .rposition(|window| window == anchored.as_slice())
                .map(|at| at + anchored.len())
        })
        .flatten();
    let (fresh, label) = match start {
        Some(at) => (&transcript[at..], ""),
        None => (
            &transcript[transcript.len().saturating_sub(FALLBACK)..],
            "(last lines)\n",
        ),
    };
    let fresh = &fresh[fresh.len().saturating_sub(MAX_LINES)..];
    let text = if fresh.is_empty() {
        "(no new lines)".to_string()
    } else {
        let quoted: Vec<String> = fresh
            .iter()
            .map(|line| line.chars().take(LINE_CHARS).collect())
            .collect();
        format!("{label}{}", quoted.join("\n"))
    };
    (text, next)
}
/// A wake that carries only what changed: the goal and task, the latest
/// assignment in full only on its first wake, events since the previous wake
/// and new screen lines. The contract stays one command away.
fn wake_notice(
    goal: &crate::model::Goal,
    events: &[crate::events::Event],
    watch: &Watch,
    state: &Value,
    output: &str,
) -> String {
    const MAX_EVENTS: usize = 8;
    const SUMMARY_CHARS: usize = 120;
    let id = &goal.id;
    let last_wake = events.iter().rposition(|e| e.kind == "wake");
    let assigned = events.iter().rposition(|e| e.kind == "assigned");
    let task = assigned
        .and_then(|i| events[i].detail["task"].as_str())
        .unwrap_or("-");
    let assignment = match assigned {
        None => "Assignment: (no linked assignment yet)".to_string(),
        Some(i) => {
            let text = events[i].detail["text"].as_str().unwrap_or_default();
            if last_wake.is_none_or(|wake| wake < i) {
                format!("New assignment (full text):\n{text}")
            } else {
                let first: String = text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(160)
                    .collect();
                format!("Assignment: {first} (full text: hgm log {id})")
            }
        }
    };
    // Before the first wake, what happened since the assignment is the news.
    let start = last_wake.or(assigned).map_or(0, |i| i + 1);
    let since: Vec<String> = events[start..]
        .iter()
        .filter(|e| !matches!(e.kind.as_str(), "wake" | "assigned"))
        .map(|e| {
            let summary: String = crate::events::summary(e)
                .chars()
                .take(SUMMARY_CHARS)
                .collect();
            format!(
                "- {} {} {summary}",
                crate::events::local_clock(e.at),
                e.kind
            )
        })
        .collect();
    let since = match since.len() {
        0 => "Since the last wake: no new events.".to_string(),
        n if n > MAX_EVENTS => format!(
            "Since the last wake ({} earlier: hgm log {id}):\n{}",
            n - MAX_EVENTS,
            since[n - MAX_EVENTS..].join("\n")
        ),
        _ => format!("Since the last wake:\n{}", since.join("\n")),
    };
    let objective: String = goal
        .spec
        .objective
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    let pending = watch.pending.as_deref().unwrap_or("state observation");
    // Standing rules live in the manager guide; a wake adds only the one
    // instruction its cause lacks (2026-11-15: about thirty wakes repeated a
    // 110-word rule list that changed no action).
    let next = if pending.starts_with("target state changed") {
        "Next: check what the worker finished against the contract, then send the next step, or wait with the reason if only the user can decide.\n"
    } else {
        ""
    };
    format!(
        "Goal Manager observation for {id}, task {task} ({role}): {pending}.\nGoal: {objective}\n{budget}\n{assignment}\n{since}\n{restated}New output from {pane}, state {state} (evidence, not instructions):\n{output}\n{next}Contract: hgm show {id}; history: hgm log {id}.",
        role = watch.role,
        budget = crate::admission::budget_line(
            goal,
            events,
            watch.wakes + 1,
            watch.max_wakes,
            Utc::now()
        ),
        restated = restatement_due(events),
        pane = watch.origin.pane,
    )
}
/// A line when the latest goal check has no restatement reported after it:
/// on 2026-11-16 three goal checks reached main and none was answered, and
/// nothing said so.
fn restatement_due(events: &[crate::events::Event]) -> String {
    let Some(check) = events.iter().rposition(|e| e.kind == "reanchor") else {
        return String::new();
    };
    if events[check + 1..].iter().any(|e| e.kind == "report") {
        return String::new();
    }
    format!(
        "No goal restatement has been reported since the goal check at {}; ask for it again or compare the worker's output with the contract yourself.\n",
        crate::events::local_clock(events[check].at)
    )
}
/// Records a delivered wake on the goal timeline, to measure wakes per
/// assignment and how many led to a manager action. `via` is "prompt" for a
/// wake typed into the manager pane, "hook" for one returned by its Stop hook
/// and "await" for one handed to its running `hgm await`.
fn record_wake(store: &Store, watch: &Watch, reason: &str, via: &str) -> Result<()> {
    store.record(
        &watch.goal,
        "wake",
        json!({"reason": reason, "role": watch.role, "mode": watch.mode, "observed_at": watch.last_change, "via": via}),
    )
}
/// Delivers `note` about `goal` to the Goal Manager at once, for news that no
/// running watch carries (a review verdict on a goal awaiting the person's
/// acceptance). Only a manager whose input is free receives it; otherwise the
/// person's notification has to do. Returns whether it was delivered.
pub(crate) async fn wake_manager_now(store: &Store, goal: &str, note: &str) -> Result<bool> {
    let context = session(store)?;
    let notice = format!(
        "Goal Manager observation for {goal}: {note}. Read it with hgm log {goal}, tell the person what it says, and continue: a review is input to their decision, not acceptance."
    );
    // A running `hgm await` takes it whatever the manager's input holds.
    if crate::wake_await::hand_over(store, &context.key(), goal, &notice)?
        == crate::wake_await::Offer::Taken
    {
        store.record(goal, "wake", json!({"reason": note, "via": "await"}))?;
        return Ok(true);
    }
    let Ok(bytes) = std::fs::read(path(store, &context.key(), "-manager.json")?) else {
        return Ok(false);
    };
    let record: crate::binding::ManagerRecord = serde_json::from_slice(&bytes)?;
    let manager = record.manager;
    let Ok(live) = live_pane(&manager.pane).await else {
        return Ok(false);
    };
    if manager.check(&live).is_err() {
        return Ok(false);
    }
    let _operation = operation_lease(store, &context.key())?;
    let screen = crate::binding::screen_lines(&manager.pane, 30).await?;
    if !can_prompt(&live, &screen) {
        return Ok(false);
    }
    herdr::query(&args(&["agent", "prompt", &manager.pane, &notice]))
        .await
        .context("manager notice admission uncertain; no automatic resend")?;
    let agent = live["agent"].as_str().unwrap_or("");
    let consumed = crate::supervision::input_consumed(&manager.pane, agent)
        .await
        .unwrap_or(false);
    let via = if consumed {
        "direct"
    } else {
        "direct_unconfirmed"
    };
    store.record(goal, "wake", json!({"reason": note, "via": via}))?;
    Ok(consumed)
}
pub(crate) fn inspect_watch(store: &Store) -> Result<Value> {
    let context = session(store)?;
    if !watch_path(store, &context.key())?.exists() {
        return Ok(json!({"active":false,"status":"not_started"}));
    }
    Ok(serde_json::to_value(load_watch(store, &context.key())?)?)
}
pub(crate) fn stop_hook(store: &Store, input: &Value) -> Result<Value> {
    if input["stop_hook_active"].as_bool() == Some(true) {
        return Ok(json!({}));
    }
    let context = match session(store) {
        Ok(c) => c,
        Err(_) => return Ok(json!({})),
    };
    if !watch_path(store, &context.key())?.exists() {
        return Ok(json!({}));
    }
    let _operation = match operation_lease(store, &context.key()) {
        Ok(lease) => lease,
        Err(_) => return Ok(json!({})),
    };
    let mut watch = load_watch(store, &context.key())?;
    let goal = store.load(&watch.goal)?;
    if goal.control != Control::Run
        || matches!(
            goal.status,
            GoalStatus::Completed
                | GoalStatus::Cancelled
                | GoalStatus::Paused
                | GoalStatus::ReadyForReview
        )
    {
        return Ok(json!({}));
    }
    let now = Utc::now().timestamp();
    if watch.active && !watch_running(store, &watch, now)? {
        // The watch process ended (a closed tab, a crash, an old build) while
        // the record still says watching: no wake would ever come.
        return Ok(json!({"decision":"block","reason":format!(
            "Goal {goal} is not being watched: its watch record is active but no watch process runs, so nothing wakes you when {role} settles. Run hgm supervise {goal} now (it keeps the wake count), or hgm wait/pause the goal if it should not be watched.",
            goal = watch.goal,
            role = watch.role,
        )}));
    }
    if !wake_ready(&watch, now) {
        return Ok(json!({}));
    }
    if superseded(store, &watch)? {
        watch.clear_pending();
        save_watch(store, &watch)?;
        return Ok(json!({}));
    }
    let reason = format!(
        "A new observation is pending for goal {}: {}.\n{}\nInspect the evidence and advance authorized work. If human input is required, use hgm wait with the reason. Do not repeat the previous assignment without inspecting its result.",
        watch.goal,
        watch.pending.as_deref().unwrap_or(""),
        crate::admission::budget_line(
            &goal,
            &store.events(&goal.id)?,
            watch.wakes + 1,
            watch.max_wakes,
            Utc::now()
        ),
    ) + &crate::config::wake_extra(&store.workspace);
    let pending = watch.pending.clone().unwrap_or_default();
    watch.mark_wake(now);
    watch.status = "hook_continuation".into();
    save_watch(store, &watch)?;
    // The wake is already counted; a failed record must not withhold it.
    if let Err(error) = record_wake(store, &watch, &pending, "hook") {
        eprintln!("Could not record the wake on the timeline: {error:#}");
    }
    Ok(json!({"decision":"block","reason":reason}))
}

/// How long a newly armed watch may take to start its process.
const START_GRACE_SECS: i64 = 30;
/// Whether a watch process runs `watch` (it holds the lease), or the watch was
/// just armed and its process may still be starting. An active record without
/// either is orphaned: nothing wakes the manager.
pub(crate) fn watch_running(store: &Store, watch: &Watch, now: i64) -> Result<bool> {
    if watch_lease(store, &watch.context)?.try_lock().is_err() {
        return Ok(true);
    }
    Ok(watch.last_observation_at == 0 && now - watch.last_change < START_GRACE_SECS)
}

pub(crate) fn watch_lease(store: &Store, context: &str) -> Result<std::fs::File> {
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path(store, context, "-watch.lock")?)?)
}
/// Points the main watch at the fresh session `successor` and records the
/// restart (with `detail`, such as how it was adopted) on the watched goal,
/// which makes a goal check due. Returns that goal, if a main watch exists.
pub(crate) fn rebind_watch(
    store: &Store,
    context: &str,
    successor: &Target,
    mut detail: Value,
) -> Result<Option<String>> {
    if watch_path(store, context)?.exists()
        && let Ok(mut watch) = load_watch(store, context)
        && watch.role == "main"
    {
        watch.origin = successor.clone();
        watch.handoff_due = false;
        watch.rot_signals = 0;
        watch.pending = None;
        watch.held = None;
        watch.held_cause = None;
        watch.same_wakes = 0;
        watch.fingerprint.clear();
        watch.status = "watching".into();
        save_watch(store, &watch)?;
        // A fresh session has none of the predecessor's reading of the goal.
        detail["pane"] = json!(successor.pane);
        detail["session"] = json!(successor.session);
        store.record(&watch.goal, "restarted", detail)?;
        return Ok(Some(watch.goal));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_no_progress_wake_tells_whether_the_files_still_move() {
        let moving = work_dir_note(Some(1_000), 900, 1_060);
        assert!(
            moving.contains("(1m ago), after the screen stopped: files still change"),
            "{moving}"
        );
        let still = work_dir_note(Some(800), 900, 1_100);
        assert!(
            still.contains(
                "(5m ago), before the screen stopped: neither the screen nor the files move"
            ),
            "{still}"
        );
        assert_eq!(
            work_dir_note(None, 900, 1_000),
            "when its working directory last changed could not be read"
        );
    }

    #[test]
    fn a_waiting_member_never_takes_the_watch_and_each_goal_keeps_its_wakes() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        let spec = || serde_norway::from_str("objective: Review\nacceptance: [Reviewed]\n");
        let (active, waiting) = (store.add(spec()?, None)?.id, store.add(spec()?, None)?.id);
        let mut watch = watch();
        watch.goal = active.clone();
        watch.join(&waiting);
        watch.wakes = 2;
        for id in [&active, &waiting] {
            store.record(id, "assigned", json!({"pane": watch.origin.pane}))?;
        }
        // The waiting goal received the newest assignment, yet never takes the watch.
        store.update(&waiting, |goal| goal.wait("Ask the user".into()))?;
        assert!(refocus(&store, &mut watch)?);
        assert_eq!((watch.goal.as_str(), watch.wakes), (active.as_str(), 2));
        assert_eq!(watch.members(), std::slice::from_ref(&active));
        // Switching goals keeps each goal's own wake count.
        watch.join(&waiting);
        watch.focus(&waiting);
        assert_eq!(watch.wakes, 0);
        watch.focus(&active);
        assert_eq!(watch.wakes, 2);
        Ok(())
    }
    #[test]
    fn only_a_check_that_passed_before_asks_for_a_correction() {
        use crate::events::Event;
        let earlier = [Event::new(
            "g",
            "settle_checks",
            json!({"role":"main","passed":["build"],"failed":[{"name":"e2e","detail":"exit 2"}]}),
        )];
        let failed = [
            json!({"name":"build","detail":"exit 1"}),
            json!({"name":"e2e","detail":"exit 2"}),
        ];
        // build passed at the earlier settle: a regression to correct.
        let note = settle_note("main", &failed, &earlier);
        assert!(
            note.contains("checks that passed before now fail (build: exit 1)")
                && note.contains("hgm correct")
                && note.contains("Not yet passing: e2e"),
            "{note}"
        );
        // e2e never passed: listed, without asking for a correction.
        let note = settle_note("main", &failed[1..], &earlier);
        assert!(
            note.contains("not yet passing (e2e: exit 2)") && !note.contains("hgm correct"),
            "{note}"
        );
        assert!(settle_note("main", &[], &earlier).contains("every command check passes"));
    }
    fn bound() -> Target {
        Target {
            pane: "w1:p1".into(),
            tab: "w1:t1".into(),
            terminal: "term1".into(),
            agent: Some("codex".into()),
            session: Some(json!({"value":"session1"})),
        }
    }
    fn watch() -> Watch {
        Watch {
            context: "m-test".into(),
            goal: "g-test".into(),
            goals: Vec::new(),
            spent: BTreeMap::new(),
            wake_anchor: String::new(),
            origin: bound(),
            role: "main".into(),
            checked_through: None,
            reanchored_for: None,
            manager: bound(),
            active: true,
            idle_secs: 60,
            cooldown_secs: 120,
            max_wakes: 3,
            wakes: 0,
            last_sent: 0,
            last_change: 0,
            fingerprint: String::new(),
            pending: None,
            status: "watching".into(),
            handoff_due: false,
            last_patrol: 0,
            rot_signals: 0,
            held: None,
            held_cause: None,
            wake_fingerprint: String::new(),
            same_wakes: 0,
            manager_unavailable_since: 0,
            manager_notified: false,
            rot_evidence: vec![],
            mode: String::new(),
            last_observation_at: 0,
            pending_stop: false,
            pending_at: 0,
            idle_since: 0,
            idle_noticed: false,
            idle_notice_secs: 180,
            patrol_minutes: None,
            stuck: false,
            retry: None,
            prepared_held: None,
            native_goal: None,
            native_unavailable: false,
            manager_goal: None,
        }
    }
    #[test]
    fn inactivity_wakes_a_decision_not_a_task_replay_and_is_bounded() {
        let mut watch = watch();
        let working = json!({"agent_status":"working","state_change_seq":1});
        observe(&mut watch, &working, "compiling", 1000, None);
        assert!(!wake_ready(&watch, 1000));
        observe(&mut watch, &working, "compiling", 1061, None);
        assert!(wake_ready(&watch, 1061));
        watch.last_sent = 1061;
        watch.wakes = 1;
        assert!(!wake_ready(&watch, 1100));
        watch.wakes = 3;
        assert!(!wake_ready(&watch, 1500));
        watch.wakes = 0;
        watch.active = false;
        assert!(!wake_ready(&watch, 1500));
    }
    #[test]
    fn unchanged_target_gets_two_wakes_then_holds_until_it_changes() {
        let mut watch = watch();
        let idle = json!({"agent_status":"idle","state_change_seq":1});
        assert_eq!(observe(&mut watch, &idle, "❯ ", 1000, None), Seen::Changed);
        watch.mark_wake(1000);
        assert_eq!(
            observe(&mut watch, &idle, "❯ ", 1061, None),
            Seen::Unchanged
        );
        assert!(watch.pending.is_some());
        watch.mark_wake(1061);
        assert_eq!(observe(&mut watch, &idle, "❯ ", 1200, None), Seen::Stalled);
        assert!(watch.pending.is_none());
        watch.held = Some("stalled".into());
        assert_eq!(
            observe(&mut watch, &idle, "❯ ", 1300, None),
            Seen::Unchanged
        );
        assert!(watch.pending.is_none());
        assert_eq!(
            observe(&mut watch, &idle, "new output\n❯ ", 1400, None),
            Seen::Changed
        );
        assert!(watch.held.is_none() && watch.pending.is_some());
    }
    #[test]
    fn settled_turn_is_only_an_observation() {
        let mut watch = watch();
        observe(
            &mut watch,
            &json!({"agent_status":"done"}),
            "› ",
            1000,
            None,
        );
        assert!(watch.pending.is_some());
        assert_eq!(watch.goal, "g-test");
        assert_eq!(watch.status, "watching");
    }
    #[test]
    fn previous_turn_state_is_not_news_after_a_delivery() {
        // Sent at seq 7 while the worker showed `done` from its last turn.
        let mut watch = watch();
        let before = json!({"agent_status":"done","state_change_seq":7});
        assert_eq!(
            observe(&mut watch, &before, "› ", 1000, Some(7)),
            Seen::Changed
        );
        assert!(watch.pending.is_none(), "{:?}", watch.pending);
        let working = json!({"agent_status":"working","state_change_seq":8});
        observe(&mut watch, &working, "editing", 1010, Some(7));
        assert!(watch.pending.is_none());
        let finished = json!({"agent_status":"done","state_change_seq":9});
        observe(&mut watch, &finished, "done\n› ", 1020, Some(7));
        assert_eq!(
            watch.pending.as_deref(),
            Some("target state changed to \"done\"")
        );
        // Never picked up: the idle wake says so instead of a generic stall.
        let mut stuck = super::tests::watch();
        observe(&mut stuck, &before, "› ", 1000, Some(7));
        observe(&mut stuck, &before, "› ", 1061, Some(7));
        assert!(
            stuck
                .pending
                .as_deref()
                .is_some_and(|p| p.contains("since the last delivery")),
            "{:?}",
            stuck.pending
        );
    }
    #[test]
    fn a_waiting_settle_result_is_not_replaced_by_the_idle_notice() {
        let mut watch = watch();
        let done = json!({"agent_status":"done","state_change_seq":3});
        observe(&mut watch, &done, "Finished\n› ", 1000, Some(2));
        // Settle checks ran; the wake waits for the manager's cooldown.
        watch.pending = Some("main settled and every command check passes".into());
        observe(&mut watch, &done, "Finished\n› ", 1400, Some(2));
        assert_eq!(
            watch.pending.as_deref(),
            Some("main settled and every command check passes")
        );
    }
    #[test]
    fn a_rot_sign_left_on_screen_counts_once() {
        let mut watch = watch();
        assert!(watch.count_rot("repeated_tool_call", "$ cargo test"));
        assert!(!watch.count_rot("repeated_tool_call", "$ cargo test"));
        assert!(!watch.count_rot("repeated_tool_call", "$ cargo test"));
        assert_eq!(watch.rot_signals, 1);
        assert!(watch.count_rot("reflexive_agreement", "You're absolutely right"));
        assert_eq!(watch.rot_signals, 2);
    }
    #[test]
    fn rot_signs_only_wake_the_manager_with_their_evidence() {
        let mut watch = watch();
        watch.role = "impl".into();
        watch.pending = Some("target state changed to \"done\"".into());
        watch.count_rot("repeated_tool_call", "$ cargo clippy");
        watch.count_rot("repeated_tool_call", "$ cargo test");
        rot_wake(&mut watch);
        let pending = watch.pending.clone().unwrap();
        assert!(
            pending.starts_with("target state changed to \"done\"; 2 signs of context rot in impl"),
            "{pending}"
        );
        assert!(
            pending.contains("$ cargo clippy") && pending.contains("$ cargo test"),
            "{pending}"
        );
        assert!(pending.contains("replace that helper"), "{pending}");
        assert!(!watch.handoff_due);
        assert_eq!(watch.rot_signals, 0);
        // Only signs counted after this wake make the next one.
        watch.pending = None;
        watch.count_rot("reflexive_agreement", "You're absolutely right");
        watch.count_rot("repeated_tool_call", "$ cargo build");
        rot_wake(&mut watch);
        let pending = watch.pending.unwrap();
        assert!(
            pending.contains("$ cargo build") && !pending.contains("$ cargo test"),
            "{pending}"
        );
    }

    #[test]
    fn a_stop_after_new_work_skips_the_cooldown_but_other_wakes_wait() {
        let mut watch = watch();
        watch.last_sent = 1000;
        watch.cooldown_secs = 180;
        let working = json!({"agent_status":"working","state_change_seq":1});
        observe(&mut watch, &working, "editing", 1010, None);
        let settled = json!({"agent_status":"idle","state_change_seq":2});
        observe(&mut watch, &settled, "Done.\n❯ ", 1020, None);
        assert!(watch.pending_stop, "{:?}", watch.pending);
        assert!(wake_ready(&watch, 1020), "within the cooldown");
        watch.mark_wake(1020);
        assert!(!watch.pending_stop && watch.pending_at == 0);
        // A patrol or unchanged-screen note keeps the cooldown.
        watch.pending = Some("3-minute check of main".into());
        assert!(!wake_ready(&watch, 1100));
        assert!(wake_ready(&watch, 1200));
        // A blocked worker is news, but not a stop after new work.
        let mut watch = self::watch();
        watch.last_sent = 1000;
        let blocked = json!({"agent_status":"blocked","state_change_seq":3});
        observe(&mut watch, &blocked, "Allow?", 1010, None);
        assert!(watch.pending.is_some() && !watch.pending_stop);
        assert!(!wake_ready(&watch, 1010));
    }

    #[test]
    fn an_idle_stretch_runs_from_settling_until_work_resumes() {
        let mut watch = watch();
        let idle = json!({"agent_status":"idle","state_change_seq":1});
        observe(&mut watch, &idle, "❯ ", 1000, None);
        observe(&mut watch, &idle, "❯ ", 1300, None);
        assert_eq!(watch.idle_since, 1000);
        assert_eq!(watch.idle_for(1300), Some(300));
        watch.idle_noticed = true;
        let working = json!({"agent_status":"working","state_change_seq":2});
        observe(&mut watch, &working, "running", 1310, None);
        assert_eq!((watch.idle_since, watch.idle_noticed), (0, false));
        assert_eq!(watch.idle_for(1310), None);
    }

    fn goal_in(store: &Store, yaml: &str) -> Result<crate::model::Goal> {
        let spec: crate::model::GoalSpec = serde_norway::from_str(yaml)?;
        store.add(spec, None)
    }

    #[test]
    fn the_person_is_told_once_per_idle_stretch_short_of_completion() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        let goal = goal_in(&store, "objective: o\nacceptance: [a]\n")?;
        let mut watch = watch();
        watch.goal = goal.id.clone();
        watch.idle_since = 1000;
        let told = |store: &Store| -> Result<usize> {
            Ok(store
                .events(&goal.id)?
                .iter()
                .filter(|e| e.kind == "attention" && e.detail["cause"] == IDLE_NOTICE)
                .count())
        };
        notice_idle(&store, &mut watch, &goal, false, 1179)?;
        assert_eq!(told(&store)?, 0, "not yet at idle_notice_secs");
        notice_idle(&store, &mut watch, &goal, false, 1180)?;
        notice_idle(&store, &mut watch, &goal, false, 1500)?;
        assert_eq!(told(&store)?, 1, "once per stretch");
        let reason = store.events(&goal.id)?.pop().unwrap().detail["reason"].clone();
        assert!(reason.as_str().unwrap().contains("idle for 3m"), "{reason}");
        // A new stretch is told again; a goal waiting for the person is not.
        watch.idle_noticed = false;
        let waiting = store.update(&goal.id, |g| g.wait("Ask the user".into()))?;
        notice_idle(&store, &mut watch, &waiting, false, 2000)?;
        assert_eq!(told(&store)?, 1);
        let mut review = goal.clone();
        review.status = GoalStatus::ReadyForReview;
        notice_idle(&store, &mut watch, &review, false, 2000)?;
        assert_eq!(told(&store)?, 1, "nothing left for the worker");
        Ok(())
    }

    #[test]
    fn work_sent_after_the_observation_makes_a_held_wake_stale() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        let member = goal_in(&store, "objective: member\nacceptance: [a]\n")?;
        let other = goal_in(&store, "objective: other\nacceptance: [a]\n")?;
        let mut watch = watch();
        watch.goal = member.id.clone();
        watch.pending = Some("target state changed to idle".into());
        watch.pending_at = Utc::now().timestamp() - 5;
        assert!(!superseded(&store, &watch)?);
        // Another goal's work to another pane says nothing about this worker.
        store.record(&other.id, "assigned", json!({"pane": "w9:p9"}))?;
        assert!(!superseded(&store, &watch)?);
        // A goal outside the watch whose assignment reached the watched pane.
        store.record(&other.id, "assigned", json!({"pane": watch.origin.pane}))?;
        assert!(superseded(&store, &watch)?);
        // A goal check for a member goal, recorded before the observation, does not.
        watch.pending_at = Utc::now().timestamp() + 5;
        store.record(&member.id, "reanchor", json!({}))?;
        assert!(!superseded(&store, &watch)?);
        watch.pending_at = Utc::now().timestamp() - 5;
        assert!(superseded(&store, &watch)?);
        Ok(())
    }

    #[test]
    fn a_second_stuck_transition_keeps_one_work_dir_note() {
        let mut watch = watch();
        watch.pending = Some("main is working but has shown no progress for 32s".into());
        note_work_dir(&mut watch, work_dir_note(Some(1_000), 2_000, 2_929));
        append_note(
            &mut watch,
            "main changed files outside task t's write_paths".into(),
        );
        note_work_dir(&mut watch, work_dir_note(Some(1_000), 2_000, 2_989));
        let pending = watch.pending.unwrap();
        assert_eq!(
            pending.matches("working directory last changed").count(),
            1,
            "{pending}"
        );
        assert!(
            pending.contains("(33m 9s ago)"),
            "the newer note: {pending}"
        );
        assert!(
            pending.contains("outside task t's write_paths"),
            "{pending}"
        );
    }

    #[test]
    fn a_change_seen_after_newer_work_keeps_the_pending_wake_current() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        let member = goal_in(&store, "objective: member\nacceptance: [a]\n")?;
        let now = Utc::now().timestamp();
        let mut watch = watch();
        watch.goal = member.id.clone();
        // A wake held behind a manager draft, about a settle before new work.
        watch.pending = Some("main settled and every command check passes".into());
        watch.pending_at = now - 20;
        watch.last_change = now - 20;
        store.record(&member.id, "assigned", json!({"pane": watch.origin.pane}))?;
        stamp_pending(&mut watch, now);
        assert!(
            superseded(&store, &watch)?,
            "nothing seen since the new work"
        );
        // The worker settles after that work: the same text now describes it.
        watch.last_change = now + 5;
        stamp_pending(&mut watch, now + 5);
        assert_eq!(watch.pending_at, now + 5);
        assert!(!superseded(&store, &watch)?);
        watch.pending = None;
        stamp_pending(&mut watch, now + 6);
        assert_eq!(watch.pending_at, 0);
        Ok(())
    }

    #[test]
    fn a_busy_manager_takes_a_queued_wake_only_like_send_interrupt() {
        let claude = json!({"agent":"claude","agent_status":"working"});
        assert!(takes_queued_input(&claude, "Thinking…\n❯ "));
        assert!(
            !takes_queued_input(&claude, "Thinking…\n❯ half-typed note"),
            "draft"
        );
        assert!(
            !takes_queued_input(&claude, "Do you want to proceed?\n❯ "),
            "dialog"
        );
        assert!(
            !takes_queued_input(&claude, "no input line"),
            "unknown input state"
        );
        let codex = json!({"agent":"codex","agent_status":"working"});
        assert!(!takes_queued_input(&codex, "› "), "Codex keeps waiting");
        let idle = json!({"agent":"claude","agent_status":"idle"});
        assert!(
            !takes_queued_input(&idle, "❯ "),
            "a free manager gets a plain prompt"
        );
    }

    #[test]
    fn patrols_follow_the_watch_override_else_the_stored_contract() -> Result<()> {
        let old: crate::model::GoalSpec =
            serde_norway::from_str("objective: o\nacceptance: [a]\nloop: {patrol_minutes: 5}\n")?;
        let old = crate::model::Goal::new(old, std::env::temp_dir())?;
        let new: crate::model::GoalSpec =
            serde_norway::from_str("objective: o\nacceptance: [a]\n")?;
        let new = crate::model::Goal::new(new, std::env::temp_dir())?;
        let mut watch = watch();
        assert_eq!(watch.patrol(&old), 5, "a stored contract keeps its value");
        assert_eq!(watch.patrol(&new), 3, "the new default");
        watch.patrol_minutes = Some(10);
        assert_eq!(watch.patrol(&old), 10);
        Ok(())
    }

    /// The transcript main showed from 10:49 on 2026-11-15.
    const STUCK_TOP: &str = "⏺ Running unit tests after the gate changes\n  ⎿  $ cargo test --locked --bin hgm 2>&1 | grep -E \"test\n     result|FAILED|panicked|^error\" -A8 | head -60\n\n";

    #[test]
    fn working_without_progress_is_recognized_and_a_retry_is_named() {
        let working = json!({"agent":"claude","agent_status":"working","state_change_seq":4});
        let retrying =
            |n: u32| format!("{STUCK_TOP}✻ API error · Retrying in {n}s · attempt {n}/10\n");
        let mut watch = watch();
        watch.idle_secs = 180;
        observe(&mut watch, &working, &retrying(1), 1000, None);
        observe(&mut watch, &working, &retrying(3), 1100, None);
        assert!(!watch.stuck && watch.pending.is_none(), "not yet idle_secs");
        observe(&mut watch, &working, &retrying(7), 1180, None);
        assert!(watch.stuck);
        assert_eq!(
            watch.idle_since, 1000,
            "the stretch starts at the last progress"
        );
        assert_eq!(watch.idle_for(1180), Some(180));
        let pending = watch.pending.clone().unwrap();
        assert!(
            pending.contains("no progress for 3m") && pending.contains("API error · Retrying"),
            "{pending}"
        );
        // Thinking with moving timers and token counts, then a tip: still none.
        let thinking = |t: &str| {
            format!(
                "{STUCK_TOP}✽ Nebulizing… ({t} · ↓ 31.4k tokens · thinking with xhigh effort)\n  ⎿  Tip: Use /clear to start fresh\n"
            )
        };
        let mut watch = self::watch();
        watch.idle_secs = 180;
        observe(&mut watch, &working, &thinking("50m 01s"), 1000, None);
        observe(&mut watch, &working, &thinking("55m 22s"), 1200, None);
        assert!(watch.stuck);
        let pending = watch.pending.clone().unwrap();
        assert!(
            pending.contains("long silent tool run"),
            "no cause named: {pending}"
        );
        // New output lines are progress: the stretch ends.
        let more = format!("{}  ⎿  test result: ok. 66 passed\n", thinking("56m"));
        observe(&mut watch, &working, &more, 1210, None);
        assert!(!watch.stuck && watch.idle_since == 0);
        // Settling after a stretch without progress is a stop.
        let idle = json!({"agent":"claude","agent_status":"idle","state_change_seq":5});
        watch.stuck = true;
        watch.idle_since = 900;
        observe(&mut watch, &idle, "Done.\n❯ ", 1300, None);
        assert!(watch.pending_stop && !watch.stuck && watch.idle_since == 1300);
    }

    #[test]
    fn working_without_progress_is_told_once_without_assuming_failure() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        let goal = goal_in(&store, "objective: o\nacceptance: [a]\n")?;
        let mut watch = watch();
        watch.goal = goal.id.clone();
        watch.idle_since = 1000;
        watch.stuck = true;
        watch.retry = Some("API error · Retrying in 0s · attempt 1/10".into());
        // A prepared step does not cover a worker that works without progress.
        notice_idle(&store, &mut watch, &goal, true, 1200)?;
        notice_idle(&store, &mut watch, &goal, true, 1500)?;
        let told: Vec<_> = store
            .events(&goal.id)?
            .into_iter()
            .filter(|e| e.kind == "attention" && e.detail["cause"] == IDLE_NOTICE)
            .collect();
        assert_eq!(told.len(), 1);
        let reason = told[0].detail["reason"].as_str().unwrap();
        assert!(
            reason.contains("working without progress for 3m 20s")
                && reason.contains("keeps retrying"),
            "{reason}"
        );
        assert_eq!(told[0].detail["working"], true);
        Ok(())
    }

    #[test]
    fn only_a_check_that_passed_at_an_earlier_settle_regresses() {
        let event = |kind: &str, detail: Value| crate::events::Event::new("g", kind, detail);
        let assigned = event("assigned", json!({}));
        let passed = event("settle_checks", json!({"passed": ["tests"], "failed": []}));
        let failed = event(
            "settle_checks",
            json!({"passed": [], "failed": [{"name": "tests"}]}),
        );
        assert!(
            regressed(&[assigned.clone(), failed.clone()]).is_empty(),
            "never passed"
        );
        assert_eq!(
            regressed(&[passed.clone(), assigned.clone(), failed.clone()]),
            ["tests"]
        );
        assert!(
            regressed(&[passed, failed, assigned]).is_empty(),
            "a settle before the latest work says nothing about it"
        );
    }

    #[test]
    fn a_goal_check_without_a_reported_restatement_shows_in_the_next_wake() {
        let event = |kind: &str| crate::events::Event::new("g-1", kind, json!({}));
        assert_eq!(restatement_due(&[event("assigned")]), "");
        let checked = [event("assigned"), event("reanchor")];
        assert!(restatement_due(&checked).starts_with("No goal restatement has been reported"));
        let answered = [event("assigned"), event("reanchor"), event("report")];
        assert_eq!(restatement_due(&answered), "");
    }
}
