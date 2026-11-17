//! The Goal Manager's own native goal (`/goal` in Claude Code). While the tab
//! has a confirmed goal in progress, the watch sets a short goal on a Claude
//! Code manager so it keeps the loop moving between wakes, and clears it once
//! no such goal is left (`manager.native_goal: false` turns this off). A
//! manager that ends each turn with `hgm await` running in the background does
//! not spin: Claude Code holds the goal's check while a background command
//! runs. When the goal keeps continuing with neither an await nor a manager
//! action, the spin guard clears it once and tells the user. As on the worker,
//! the goal's verdict is an observation: it completes and accepts nothing.
use crate::{
    binding::live_pane,
    native_goal::{Typed, type_into},
    signals::{NativeGoal as N, can_prompt},
    store::Store,
    watch::Watch,
};
use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Continuations with no await running and no manager action before the
/// spin guard clears the goal. Claude Code pauses a goal by itself only after
/// about nine unmet checks in one turn (seen natively, 2026-11-15).
pub(crate) const SPIN_LIMIT: u32 = 3;
/// The `attention` cause when the spin guard cleared the manager's goal.
pub(crate) const SPINNING: &str = "manager_goal_spinning";
/// How the condition starts; with the set time it tells this set's
/// `Goal set:` apart from earlier ones and from quoted worker lines.
const HEAD: &str = "hgm Goal Manager";
/// Seconds a settled manager may show no `Goal set:` for a queued set.
const GRACE_SECS: i64 = 10;
/// Recent lines read to confirm a queued set and to count continuations.
const RECENT_LINES: u32 = 200;

/// The condition: fixed text with the set time only. It names the exits of
/// the acceptance (every goal of the tab ready for review, waiting, completed
/// or cancelled; or a decision only the user can make) and the await.
pub(crate) fn condition(stamp: &str) -> String {
    format!(
        "{HEAD} ({stamp}): done when hgm status shows every goal of this tab ready for review, waiting, completed or cancelled, or when you have stated plainly that only the user can decide the next step. Until then act on each wake and end every turn with hgm await running in the background."
    )
}

/// The native goal hgm set on the Goal Manager, kept in the watch record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagerGoalState {
    /// The manager pane and session it was typed into.
    pub pane: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Value>,
    /// The set time named in the condition (local `HH:MM:SS`).
    pub stamp: String,
    /// When hgm typed it (Unix seconds).
    pub set_at: i64,
    /// `pending` until `Goal set:` shows, none while set, else the last
    /// outcome: `met`, `impossible`, `paused`, `error_cleared`, `cleared`,
    /// `unavailable` or `uncertain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// When it ended (Unix seconds; 0 while set).
    #[serde(default)]
    pub ended_at: i64,
    /// Continuations counted since the last await or manager action.
    #[serde(default)]
    pub continuations: u32,
    /// Continuation lines in the manager's recent lines at the last count.
    #[serde(default)]
    pub seen: usize,
    /// When continuations were last counted (Unix seconds).
    #[serde(default)]
    pub counted_at: i64,
    /// The spin guard cleared it: hgm does not set it again for this session.
    #[serde(default)]
    pub guarded: bool,
}
impl ManagerGoalState {
    /// Whether the goal may still be set on the manager as far as hgm knows.
    pub fn is_set(&self) -> bool {
        matches!(self.outcome.as_deref(), None | Some("pending" | "paused"))
    }
    /// The state for `status`, `show` and the watch tab.
    pub fn view(&self) -> Value {
        json!({"set": self.is_set(), "outcome": self.outcome, "guarded": self.guarded})
    }
}
/// Display text for a [`ManagerGoalState::view`].
pub(crate) fn text(view: &Value) -> String {
    let base = crate::cli::native_text(view);
    if view["guarded"] == true {
        format!("{base} (cleared by the spin guard; not set again for this manager session)")
    } else {
        base
    }
}

use crate::signals::own_lines;
/// Goal continuations (`◯ Goal not yet met… continuing`) among `lines`.
fn continuations(lines: &str) -> usize {
    own_lines(lines)
        .lines()
        .filter(|line| line.to_lowercase().contains("goal not yet met"))
        .count()
}
/// Whether a confirmed goal of this tab is in progress: one the user agreed
/// to that is not ready for review, finished, waiting for them or paused.
fn tab_in_progress(store: &Store) -> Result<bool> {
    let owner = crate::binding::origin_terminal(store)?;
    Ok(store.list()?.iter().any(|goal| {
        owner
            .as_ref()
            .is_none_or(|owner| goal.owner.as_ref().is_none_or(|o| o == owner))
            && goal.confirmed.is_some()
            && crate::watch::short_of_completion(goal)
    }))
}
/// Whether the manager acted (sent work, corrected, completed a task, made a
/// goal wait) on a goal of the watch since `since` (Unix seconds; the same
/// second counts, so an action right after a count is not missed).
fn manager_acted(store: &Store, watch: &Watch, since: i64) -> Result<bool> {
    for goal in watch.members() {
        if store
            .events(&goal)?
            .iter()
            .any(|e| e.at.timestamp() >= since && crate::accounting::manager_action(e))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// One step of the manager goal, after each watch observation: set it, read
/// it back, clear it when no confirmed goal is in progress, and guard against
/// spinning. Only a Claude Code manager has one; for other providers nothing
/// is read or typed.
pub(crate) async fn step(store: &Store, watch: &mut Watch) -> Result<()> {
    if watch.manager.agent.as_deref() != Some("claude") {
        return Ok(());
    }
    // A goal typed into an earlier manager session is not this one's.
    if watch
        .manager_goal
        .as_ref()
        .is_some_and(|s| s.pane != watch.manager.pane || s.session != watch.manager.session)
    {
        watch.manager_goal = None;
    }
    let enabled = crate::config::load(&store.workspace)?.manager_native_goal();
    let in_progress = tab_in_progress(store)?;
    let set = watch
        .manager_goal
        .as_ref()
        .is_some_and(ManagerGoalState::is_set);
    let wanted = !set && enabled && in_progress && may_set(store, watch)?;
    if !set && !wanted {
        return Ok(());
    }
    let pane = watch.manager.pane.clone();
    let live = live_pane(&pane).await?;
    watch.manager.check(&live)?;
    let screen = crate::binding::screen_lines(&pane, 30).await?;
    let now = Utc::now().timestamp();
    if wanted {
        return set_goal(store, watch, &live, &screen, now).await;
    }
    let recent = crate::binding::recent_lines(&pane, RECENT_LINES).await?;
    observe(store, watch, &live, &screen, &recent, now)?;
    if !watch
        .manager_goal
        .as_ref()
        .is_some_and(ManagerGoalState::is_set)
    {
        return Ok(());
    }
    if !enabled || !in_progress {
        let reason = if enabled {
            "no confirmed goal of this tab is in progress"
        } else {
            "manager.native_goal is off"
        };
        return clear(store, watch, &live, &screen, reason, false).await;
    }
    spin(store, watch, &live, &screen, &recent, now).await
}

/// After the watch ended (the caller holds the leases and saves `watch`):
/// clears the manager goal when no confirmed goal of the tab is in progress or
/// the setting is off. Returns whether a clear is still due, because the
/// manager's input held a draft, a dialog or an unknown line.
pub(crate) async fn after_watch(store: &Store, watch: &mut Watch) -> Result<bool> {
    if !watch
        .manager_goal
        .as_ref()
        .is_some_and(ManagerGoalState::is_set)
    {
        return Ok(false);
    }
    step(store, watch).await?;
    let due =
        !crate::config::load(&store.workspace)?.manager_native_goal() || !tab_in_progress(store)?;
    Ok(due
        && watch
            .manager_goal
            .as_ref()
            .is_some_and(ManagerGoalState::is_set))
}

/// Whether hgm may set the goal now: never after the spin guard cleared it
/// for this session, and after an earlier goal ended only once the manager
/// acted again (an exit it took is not undone).
fn may_set(store: &Store, watch: &Watch) -> Result<bool> {
    match &watch.manager_goal {
        None => Ok(true),
        Some(state) if state.guarded => Ok(false),
        Some(state) => manager_acted(store, watch, state.ended_at.max(state.set_at)),
    }
}

/// Types the goal under the delivery rules: into a settled manager's empty
/// input line, or queued into a working one with an empty input line and no
/// dialog. Otherwise a later observation tries again.
async fn set_goal(
    store: &Store,
    watch: &mut Watch,
    live: &Value,
    screen: &str,
    now: i64,
) -> Result<()> {
    let free = can_prompt(live, screen);
    let queued = !free && crate::watch::takes_queued_input(live, screen);
    if !free && !queued {
        return Ok(());
    }
    let stamp = chrono::Local::now().format("%H:%M:%S").to_string();
    let condition = condition(&stamp);
    let mut state = ManagerGoalState {
        pane: watch.manager.pane.clone(),
        session: watch.manager.session.clone(),
        stamp,
        set_at: now,
        outcome: Some("pending".into()),
        counted_at: now,
        ..ManagerGoalState::default()
    };
    // Saved before typing (see `watch::settle_interrupted`).
    watch.manager_goal = Some(ManagerGoalState {
        outcome: Some("sending".into()),
        ..state.clone()
    });
    crate::watch::save_watch(store, watch)?;
    let typed = type_into(
        &state.pane,
        &format!("/goal {condition}"),
        "claude",
        &["Goal set:"],
        queued,
    )
    .await;
    let detail = match typed {
        Ok(Typed::Seen) => {
            state.outcome = None;
            json!({"condition": condition, "via": "prompt", "consumed": true})
        }
        Ok(Typed::Queued) => json!({"condition": condition, "via": "queued", "pending": true}),
        Ok(Typed::Unconfirmed(why)) => {
            state.outcome = Some("uncertain".into());
            state.ended_at = now;
            json!({"condition": condition, "delivery": "uncertain", "why": why})
        }
        Err(error) => {
            state.outcome = Some("uncertain".into());
            state.ended_at = now;
            json!({"condition": condition, "delivery": "uncertain", "error": format!("{error:#}")})
        }
    };
    if state.outcome.is_none() {
        let recent = crate::binding::recent_lines(&state.pane, RECENT_LINES).await?;
        state.seen = continuations(&recent);
    }
    store.record(&watch.goal, "manager_goal_set", detail)?;
    watch.manager_goal = Some(state);
    Ok(())
}

/// Reads the goal back: a queued set becomes set once its own `Goal set:`
/// shows, uncertain when the manager has settled without it (never typed
/// again); a set goal's outcome is read from the lines Claude Code printed.
fn observe(
    store: &Store,
    watch: &mut Watch,
    live: &Value,
    screen: &str,
    recent: &str,
    now: i64,
) -> Result<()> {
    let Some(mut state) = watch.manager_goal.clone() else {
        return Ok(());
    };
    if state.outcome.as_deref() == Some("pending") {
        let head = format!("{HEAD} ({})", state.stamp);
        let read = recent
            .lines()
            .chain(screen.lines())
            .any(|line| line.contains("Goal set:") && line.contains(&head));
        let settled = matches!(live["agent_status"].as_str(), Some("idle" | "done"));
        if read {
            state.outcome = None;
            state.seen = continuations(recent);
            state.counted_at = now;
            store.record(
                &watch.goal,
                "manager_goal_set",
                json!({"via": "queued", "consumed": true}),
            )?;
        } else if settled && now - state.set_at >= GRACE_SECS {
            state.outcome = Some("uncertain".into());
            state.ended_at = now;
            store.record(
                &watch.goal,
                "manager_goal_set",
                json!({"delivery": "uncertain", "why": "the manager settled without Goal set:"}),
            )?;
        }
        watch.manager_goal = Some(state);
        return Ok(());
    }
    let outcome = |notice: Option<N>| match notice {
        Some(N::Met(line)) => Some(("met", line)),
        Some(N::Impossible(line)) => Some(("impossible", line)),
        Some(N::Paused(line)) => Some(("paused", line)),
        Some(N::ErrorCleared(line)) => Some(("error_cleared", line)),
        Some(N::Cleared(line)) => Some(("cleared", line)),
        Some(N::Unavailable(line)) => Some(("unavailable", line)),
        Some(N::Active | N::Retrying(_)) | None => None,
    };
    let settled = matches!(live["agent_status"].as_str(), Some("idle" | "done"));
    let (outcome, line) = match outcome(crate::signals::native_goal(&own_lines(screen))) {
        Some(found) => found,
        // A goal set as queued input can end at the very stop that runs it;
        // Claude Code then prints the outcome above the echoed `Goal set:`
        // (seen natively, 2026-11-16). A settled manager without the
        // `◎ /goal active` indicator above its input line runs no goal.
        None if settled
            && now - state.set_at >= GRACE_SECS
            && !crate::signals::goal_indicator_shown(screen) =>
        {
            // Its outcome line, if one shows, only names how it ended.
            outcome(crate::signals::unseen_outcome(screen)).unwrap_or((
                "ended",
                "no /goal active indicator above the input line".into(),
            ))
        }
        None => return Ok(()),
    };
    if state.outcome.as_deref() == Some(outcome) {
        return Ok(());
    }
    store.record(
        &watch.goal,
        "manager_goal",
        json!({"outcome": outcome, "line": line}),
    )?;
    if outcome != "paused" {
        state.ended_at = now;
    }
    state.outcome = Some(outcome.into());
    watch.manager_goal = Some(state);
    Ok(())
}

/// Counts continuations seen since the last step. An await running or a
/// manager action resets the count; reaching [`SPIN_LIMIT`] clears the goal
/// once and tells the user.
async fn spin(
    store: &Store,
    watch: &mut Watch,
    live: &Value,
    screen: &str,
    recent: &str,
    now: i64,
) -> Result<()> {
    let Some(mut state) = watch.manager_goal.clone() else {
        return Ok(());
    };
    let total = continuations(recent);
    if crate::wake_await::running(store, &watch.context)?
        || manager_acted(store, watch, state.counted_at)?
    {
        state.continuations = 0;
    } else if total > state.seen {
        state.continuations += u32::try_from(total - state.seen).unwrap_or(u32::MAX);
    }
    // A smaller total means earlier lines scrolled away.
    state.seen = total;
    state.counted_at = now;
    let spinning = state.continuations >= SPIN_LIMIT;
    watch.manager_goal = Some(state);
    if spinning {
        let reason = "the goal kept continuing with no hgm await running and no manager action";
        clear(store, watch, live, screen, reason, true).await?;
    }
    Ok(())
}

/// Types `/goal clear` under the delivery rules (otherwise a later
/// observation tries again) and records it. From the spin guard it also tells
/// the user and keeps hgm from setting the goal again for this session.
async fn clear(
    store: &Store,
    watch: &mut Watch,
    live: &Value,
    screen: &str,
    reason: &str,
    guard: bool,
) -> Result<()> {
    let Some(mut state) = watch.manager_goal.clone() else {
        return Ok(());
    };
    let free = can_prompt(live, screen);
    let queued = !free && crate::watch::takes_queued_input(live, screen);
    if !free && !queued {
        return Ok(());
    }
    watch.manager_goal = Some(ManagerGoalState {
        outcome: Some("clearing".into()),
        ..state.clone()
    });
    crate::watch::save_watch(store, watch)?;
    let answer = ["Goal cleared:", "No goal set"];
    let typed = type_into(&state.pane, "/goal clear", "claude", &answer, queued).await;
    let (mut detail, outcome) = match typed {
        Ok(Typed::Seen) => (json!({"consumed": true}), "cleared"),
        Ok(Typed::Queued) => (json!({"consumed": true, "queued": true}), "cleared"),
        Ok(Typed::Unconfirmed(why)) => (json!({"delivery": "uncertain", "why": why}), "uncertain"),
        Err(error) => (
            json!({"delivery": "uncertain", "error": format!("{error:#}")}),
            "uncertain",
        ),
    };
    detail["reason"] = json!(reason);
    if guard {
        detail["spin_guard"] = json!(true);
        detail["continuations"] = json!(state.continuations);
    }
    store.record(&watch.goal, "manager_goal_cleared", detail)?;
    state.outcome = Some(outcome.into());
    state.ended_at = Utc::now().timestamp();
    state.guarded |= guard;
    let pane = state.pane.clone();
    watch.manager_goal = Some(state);
    if guard {
        store.record(
            &watch.goal,
            "attention",
            json!({
                "cause": SPINNING,
                "pane": pane,
                "reason": format!(
                    "the Goal Manager's /goal kept continuing {SPIN_LIMIT} times with no hgm await running and no manager action, so hgm cleared it once and will not set it again for this manager session; the manager continues without it. Ask it to end each turn with hgm await in the background"
                ),
            }),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_condition_names_the_exits_and_stays_short() {
        let text = condition("14:05:09");
        assert!(text.starts_with("hgm Goal Manager (14:05:09): "), "{text}");
        for exit in [
            "ready for review, waiting, completed or cancelled",
            "only the user can decide",
            "hgm await running in the background",
        ] {
            assert!(text.contains(exit), "{exit:?} missing: {text}");
        }
        assert!(
            text.len() <= crate::native_goal::MAX_CONDITION,
            "{}",
            text.len()
        );
    }

    #[test]
    fn only_lines_claude_code_printed_itself_count() {
        // A wake quoting the worker's continuation is indented; the
        // manager's own continuation starts the line.
        let screen = "◯ Goal not yet met… continuing\n  ⎿  ◯ Goal not yet met… continuing\n❯ Goal Manager observation\n  ◯ Goal not yet met… continuing\n◯ Goal not yet met… continuing\n";
        assert_eq!(continuations(screen), 2);
        assert_eq!(
            crate::signals::native_goal(&own_lines("  ✔ Goal achieved (9s · 1 turn)\n❯ ")),
            None,
            "a quoted worker outcome is not the manager's"
        );
    }

    #[test]
    fn the_active_indicator_is_read_right_above_the_input_line() {
        // Claude Code 2.1.325: the indicator sits between the turn's last
        // line and the input box while a goal is set.
        let active = "⏺ 待機\n✻ Crunched for 2s · done 3:05 AM\n    ◎ /goal active (12s)\n────\n❯ \n────\n  ⏵⏵ bypass permissions on";
        assert!(crate::signals::goal_indicator_shown(active));
        // Natively (2026-11-16) a queued goal met at the stop that ran it:
        // the outcome shows above the echoed `Goal set:`, no indicator below.
        let ended = "⏺ 待機\n✔ Goal achieved (4s · 1 turn · 647 tokens)\n✻ Sautéed for 5s · done 3:05 AM\n❯ Goal set: hgm Goal Manager (03:05:34): done when hgm status shows\n⏺ 待機\n✻ Crunched for 2s · done 3:05 AM\n────\n❯ \n────\n";
        assert!(!crate::signals::goal_indicator_shown(ended));
        // A quoted indicator far above the input line is not this one.
        assert!(!crate::signals::goal_indicator_shown(
            "  ◎ /goal active (59s)\n⏺ a\n⏺ b\n⏺ c\n⏺ d\n❯ "
        ));
    }

    #[test]
    fn a_state_is_set_until_it_ends() {
        let mut state = ManagerGoalState {
            outcome: Some("pending".into()),
            ..ManagerGoalState::default()
        };
        assert!(state.is_set());
        state.outcome = None;
        assert!(state.is_set());
        state.outcome = Some("paused".into());
        assert!(state.is_set());
        state.outcome = Some("cleared".into());
        state.guarded = true;
        assert!(!state.is_set());
        assert!(
            text(&state.view()).contains("spin guard"),
            "{}",
            text(&state.view())
        );
    }
}
