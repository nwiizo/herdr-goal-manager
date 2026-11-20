use crate::{
    binding::args,
    contract::{Autonomy, NativeGoalMode},
    herdr_cli as herdr,
    model::Goal,
    store::Store,
    watch::Watch,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub(crate) const MAX_CONDITION: usize = 700;
const TURNS_PER_ASSIGNMENT: u32 = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NativeGoalState {
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The linked assignment it was set for.
    pub assigned_at: DateTime<Utc>,
    /// When hgm set it (Unix seconds).
    pub set_at: i64,
    /// The last outcome read from the screen (`met`, `impossible`, `paused`,
    /// `error_cleared`, `cleared`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}
impl NativeGoalState {
    /// Whether the goal may still be set on the worker as far as hgm knows; a
    /// pending one (queued with the assignment) may run, so it counts.
    pub fn is_set(&self) -> bool {
        matches!(self.outcome.as_deref(), None | Some("paused" | "pending"))
    }
}

pub(crate) fn mode(goal: &Goal, agent: &str) -> Option<NativeGoalMode> {
    let policy = &goal.spec.loop_policy;
    if agent != "claude" || policy.autonomy != Autonomy::Auto {
        return None;
    }
    match policy.native_goal.unwrap_or(NativeGoalMode::Settle) {
        NativeGoalMode::Off => None,
        mode => Some(mode),
    }
}
pub(crate) fn enabled(goal: &Goal, agent: &str) -> bool {
    mode(goal, agent).is_some()
}

pub(crate) fn condition(goal: &Goal, task: Option<&str>) -> String {
    let spec = &goal.spec;
    let head = format!(
        "hgm goal {} task {}: the assigned work is done and you have reported what changed, how each acceptance item was verified, and anything unresolved; or you have stated plainly that a contract boundary, a stop condition or a decision only the user can make blocks it.",
        goal.id,
        task.unwrap_or("(goal)")
    );
    let bound = format!(
        "Or stop after {} turns.",
        spec.loop_policy.max_assignments * TURNS_PER_ASSIGNMENT
    );
    let checks: Vec<String> = spec
        .checks
        .iter()
        .map(|check| format!("`{}`", check.command))
        .collect();
    let boundaries: Vec<String> = spec
        .out_of_scope
        .iter()
        .cloned()
        .chain(
            spec.protected_paths
                .iter()
                .map(|p| format!("{p} (protected)")),
        )
        .collect();
    let build = |checks: Option<String>, boundaries: Option<String>| {
        [Some(head.clone()), checks, boundaries, Some(bound.clone())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let listed = build(
        (!checks.is_empty()).then(|| {
            format!(
                "Show in this conversation that these commands exit 0: {}.",
                checks.join(", ")
            )
        }),
        (!boundaries.is_empty()).then(|| {
            format!(
                "Do not cross these boundaries: {}. At one, stop and report.",
                boundaries.join("; ")
            )
        }),
    );
    if listed.len() <= MAX_CONDITION {
        return listed;
    }
    build(
        (!checks.is_empty()).then(|| {
            "Show in this conversation that the check commands in this goal's contract exit 0."
                .to_string()
        }),
        (!boundaries.is_empty()).then(|| {
            "Do not cross the boundaries in this goal's contract; at one, stop and report."
                .to_string()
        }),
    )
}

pub(crate) enum Typed {
    Occupied,
    Seen,
    Queued,
    Unconfirmed(&'static str),
}

fn answers(screen: &str, answers: &[&str]) -> usize {
    screen
        .lines()
        .filter(|line| answers.iter().any(|answer| line.contains(answer)))
        .count()
}

pub(crate) async fn type_into(
    target: &crate::binding::Target,
    text: &str,
    agent: &str,
    answer: &[&str],
    queued: bool,
) -> Result<Typed> {
    let pane = &target.pane;
    let (live, screen) = crate::watch::fresh_worker(pane).await?;
    if target.check(&live).is_err()
        || !(crate::signals::can_prompt(&live, &screen)
            || (queued && crate::watch::takes_queued_input(&live, &screen)))
    {
        return Ok(Typed::Occupied);
    }
    let before = answers(&screen, answer);
    herdr::query(&args(&["agent", "prompt", pane, text])).await?;
    if !crate::supervision::input_consumed(pane, agent)
        .await
        .unwrap_or(false)
    {
        return Ok(Typed::Unconfirmed("the input line did not empty"));
    }
    if queued {
        return Ok(Typed::Queued);
    }
    for _ in 0..10 {
        if answers(&crate::binding::screen(pane).await?, answer) > before {
            return Ok(Typed::Seen);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    Ok(Typed::Unconfirmed("no answer to it appeared on screen"))
}

pub(crate) async fn set(
    store: &Store,
    watch: &mut Watch,
    goal: &Goal,
    task: Option<&str>,
    assigned_at: DateTime<Utc>,
    agent: &str,
) -> Result<bool> {
    let condition = condition(goal, task);
    let command = format!("/goal {condition}");
    let state = NativeGoalState {
        goal: goal.id.clone(),
        task: task.map(String::from),
        assigned_at,
        set_at: Utc::now().timestamp(),
        outcome: None,
    };
    let previous = watch.native_goal.take();
    watch.native_goal = Some(NativeGoalState {
        outcome: Some("sending".into()),
        ..state.clone()
    });
    crate::watch::save_watch(store, watch)?;
    match type_into(&watch.origin, &command, agent, &["Goal set:"], false).await {
        Ok(Typed::Occupied) => {
            watch.native_goal = previous;
            crate::watch::save_watch(store, watch)?;
            return Ok(false);
        }
        Ok(Typed::Seen | Typed::Queued) => {
            store.record(
                &goal.id,
                "native_goal_set",
                json!({"task": task, "condition": condition, "consumed": true}),
            )?;
            watch.native_goal = Some(state);
        }
        Ok(Typed::Unconfirmed(why)) => {
            store.record(
                &goal.id,
                "native_goal_set",
                json!({"task": task, "condition": condition, "delivery": "uncertain", "why": why}),
            )?;
            watch.native_goal = Some(NativeGoalState {
                outcome: Some("uncertain".into()),
                ..state
            });
        }
        Err(error) => {
            store.record(
                &goal.id,
                "native_goal_set",
                json!({"task": task, "delivery": "uncertain", "error": format!("{error:#}")}),
            )?;
            watch.native_goal = Some(NativeGoalState {
                outcome: Some("uncertain".into()),
                ..state
            });
        }
    }
    Ok(true)
}

pub(crate) async fn clear(store: &Store, watch: &mut Watch, reason: &str) -> Result<bool> {
    let Some(state) = watch.native_goal.clone().filter(NativeGoalState::is_set) else {
        return Ok(true);
    };
    let (live, screen) = crate::watch::fresh_worker(&watch.origin.pane).await?;
    if let Err(error) = watch.origin.check(&live) {
        store.record(
            &state.goal,
            "native_goal_cleared",
            json!({"reason": reason, "delivery": "skipped", "why": format!("{error:#}")}),
        )?;
        watch.native_goal = Some(NativeGoalState {
            outcome: Some("unbound".into()),
            ..state
        });
        return Ok(true);
    }
    let (live, screen) = (&live, screen.as_str());
    let agent = live["agent"].as_str().unwrap_or("");
    let free = crate::signals::can_prompt(live, screen);
    let queued = !free
        && crate::supervision::QUEUES_WHILE_WORKING.contains(&agent)
        && live["agent_status"] == "working"
        && crate::signals::input_refusal(live, screen).is_none();
    if !free && !queued {
        return Ok(false);
    }
    watch.native_goal = Some(NativeGoalState {
        outcome: Some("clearing".into()),
        ..state.clone()
    });
    crate::watch::save_watch(store, watch)?;
    let answer = ["Goal cleared:", "No goal set"];
    let (detail, outcome) =
        match type_into(&watch.origin, "/goal clear", agent, &answer, queued).await {
            Ok(Typed::Occupied) => {
                watch.native_goal = Some(state);
                crate::watch::save_watch(store, watch)?;
                return Ok(false);
            }
            Ok(Typed::Seen) => (json!({"reason": reason, "consumed": true}), "cleared"),
            Ok(Typed::Queued) => (
                json!({"reason": reason, "consumed": true, "queued": true}),
                "cleared",
            ),
            Ok(Typed::Unconfirmed(why)) => (
                json!({"reason": reason, "delivery": "uncertain", "why": why}),
                "uncertain",
            ),
            Err(error) => (
                json!({"reason": reason, "delivery": "uncertain", "error": format!("{error:#}")}),
                "uncertain",
            ),
        };
    store.record(&state.goal, "native_goal_cleared", detail)?;
    watch.native_goal = Some(NativeGoalState {
        outcome: Some(outcome.into()),
        ..state
    });
    Ok(true)
}

pub(crate) async fn clear_for(store: &Store, goal: Option<&str>, reason: &str) -> Result<()> {
    let Ok(context) = crate::binding::session(store) else {
        return Ok(());
    };
    let key = context.key();
    let wanted = |watch: &Watch| {
        watch
            .native_goal
            .as_ref()
            .is_some_and(|state| goal.is_none_or(|goal| state.goal == goal) && state.is_set())
    };
    if !crate::watch::load_watch(store, &key).is_ok_and(|watch| wanted(&watch)) {
        return Ok(());
    }
    let cleared = async {
        let _operation = crate::binding::wait_operation_lease(store, &key).await?;
        let mut watch = crate::watch::load_watch(store, &key)?;
        if !wanted(&watch) {
            return Ok(true);
        }
        let typed = clear(store, &mut watch, reason).await?;
        crate::watch::save_watch(store, &watch)?;
        anyhow::Ok(typed)
    };
    match cleared.await {
        Ok(true) => {}
        Ok(false) => eprintln!(
            "The native goal was not cleared now: the worker's input is not free (a draft, a dialog or an unknown input line). The watch clears it once the input is free and tells the user if it cannot."
        ),
        Err(error) => eprintln!(
            "The native goal was not cleared now ({error:#}); the watch clears it once it can and tells the user if it cannot."
        ),
    }
    Ok(())
}

pub(crate) async fn with_assignment(
    store: &Store,
    goal: &Goal,
    task: Option<&str>,
    pane: &str,
    agent: &str,
    key: &str,
) -> Result<Option<NativeGoalState>> {
    if mode(goal, agent) != Some(NativeGoalMode::Assignment) {
        return Ok(None);
    }
    let Some(assigned) = store
        .events(&goal.id)?
        .into_iter()
        .rev()
        .find(|e| e.kind == "assigned" && e.detail["pane"].as_str() == Some(pane))
    else {
        return Ok(None);
    };
    let (live, screen) = crate::watch::fresh_worker(pane).await?;
    let working = live["agent_status"] == "working";
    let free = crate::signals::can_prompt(&live, &screen)
        || (working
            && crate::supervision::QUEUES_WHILE_WORKING.contains(&agent)
            && crate::signals::input_refusal(&live, &screen).is_none());
    if !free {
        return Ok(None);
    }
    let condition = condition(goal, task);
    let mut state = NativeGoalState {
        goal: goal.id.clone(),
        task: task.map(String::from),
        assigned_at: assigned.at,
        set_at: Utc::now().timestamp(),
        outcome: Some("pending".into()),
    };
    let mut record = crate::watch::load_watch(store, key)?;
    let previous = record.native_goal.replace(NativeGoalState {
        outcome: Some("sending".into()),
        ..state.clone()
    });
    crate::watch::save_watch(store, &record)?;
    match type_into(
        &record.origin,
        &format!("/goal {condition}"),
        agent,
        &["Goal set:"],
        true,
    )
    .await
    {
        Ok(Typed::Occupied) => {
            record.native_goal = previous;
            crate::watch::save_watch(store, &record)?;
            return Ok(None);
        }
        Ok(Typed::Queued | Typed::Seen) => store.record(
            &goal.id,
            "native_goal_queued",
            json!({"task": task, "condition": condition}),
        )?,
        Ok(Typed::Unconfirmed(why)) => {
            store.record(
                &goal.id,
                "native_goal_set",
                json!({"task": task, "condition": condition, "delivery": "uncertain", "why": why}),
            )?;
            state.outcome = Some("uncertain".into());
        }
        Err(error) => {
            store.record(
                &goal.id,
                "native_goal_set",
                json!({"task": task, "delivery": "uncertain", "error": format!("{error:#}")}),
            )?;
            state.outcome = Some("uncertain".into());
        }
    }
    Ok(Some(state))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(yaml: &str) -> Goal {
        Goal::new(serde_norway::from_str(yaml).unwrap(), std::env::temp_dir()).unwrap()
    }

    #[test]
    fn only_claude_code_under_auto_gets_a_native_goal_unless_turned_off() {
        let auto = goal("objective: o\nacceptance: [a]\nchecks: [{name: c, command: 'true'}]\n");
        assert!(enabled(&auto, "claude"));
        assert!(!enabled(&auto, "codex") && !enabled(&auto, "copilot"));
        let off = goal(
            "objective: o\nacceptance: [a]\nchecks: [{name: c, command: 'true'}]\nloop: {native_goal: false}\n",
        );
        assert!(!enabled(&off, "claude"));
        let ask = goal("objective: o\nacceptance: [a]\nloop: {autonomy: ask, native_goal: true}\n");
        assert!(!enabled(&ask, "claude"), "never under autonomy ask");
    }

    #[test]
    fn the_condition_is_built_from_contract_fields_and_stays_short() {
        let g = goal(
            "objective: o\nacceptance: [a]\nchecks: [{name: c, command: 'cargo test --locked'}]\nout_of_scope: [Release]\nprotected_paths: [auth]\nloop: {max_assignments: 2}\n",
        );
        let text = condition(&g, Some("work"));
        for part in [
            g.id.as_str(),
            "task work",
            "how each acceptance item was verified",
            "`cargo test --locked`",
            "Release; auth (protected)",
            "Or stop after 20 turns.",
        ] {
            assert!(text.contains(part), "{part:?} missing: {text}");
        }
        let long = goal(&format!(
            "objective: o\nacceptance: [a]\nchecks: [{{name: c, command: 'true'}}]\nout_of_scope: [{}]\nlong_spec_reason: test\n",
            vec!["x".repeat(300); 20].join(", ")
        ));
        let text = condition(&long, None);
        assert!(text.len() <= MAX_CONDITION, "{}", text.len());
        assert!(format!("/goal {text}").len() < 800);
        assert!(text.ends_with("Or stop after 30 turns."));
        assert!(text.contains("the check commands in this goal's contract exit 0"));
        assert!(text.contains("the boundaries in this goal's contract"));
        assert!(!text.contains("xxxx"), "{text}");
    }

    #[test]
    fn the_native_goal_mode_reads_stored_and_new_values() {
        let with = |field: &str| {
            goal(&format!(
                "objective: o\nacceptance: [a]\nchecks: [{{name: c, command: 'true'}}]\n{field}"
            ))
        };
        assert_eq!(mode(&with(""), "claude"), Some(NativeGoalMode::Settle));
        assert_eq!(
            mode(&with("loop: {native_goal: true}\n"), "claude"),
            Some(NativeGoalMode::Settle)
        );
        assert_eq!(
            mode(&with("loop: {native_goal: settle}\n"), "claude"),
            Some(NativeGoalMode::Settle)
        );
        assert_eq!(
            mode(&with("loop: {native_goal: assignment}\n"), "claude"),
            Some(NativeGoalMode::Assignment)
        );
        assert_eq!(mode(&with("loop: {native_goal: false}\n"), "claude"), None);
        assert_eq!(
            mode(&with("loop: {native_goal: assignment}\n"), "codex"),
            None
        );
        let refused: Result<crate::model::GoalSpec, _> =
            serde_norway::from_str("objective: o\nacceptance: [a]\nloop: {native_goal: soon}\n");
        assert!(format!("{}", refused.unwrap_err()).contains("assignment, settle or false"));
        assert_eq!(
            serde_json::to_value(NativeGoalMode::Off).unwrap(),
            serde_json::json!(false)
        );
    }
}
