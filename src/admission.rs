use crate::{
    contract::Autonomy,
    events::Event,
    model::{Goal, GoalStatus, word_units},
    store::Store,
};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Local, Utc};

pub(crate) struct Assignment<'a> {
    pub goal: Option<&'a str>,
    pub task: Option<&'a str>,
    pub supervise: bool,
    /// The user's words approving this assignment; required by `autonomy: ask`.
    pub user_approval: Option<&'a str>,
    /// Asks for a plan instead of implementation (`loop.plan_first`).
    pub plan: bool,
    /// Recipient role: `main` (the original tab) or a helper such as `impl`.
    pub to: &'a str,
    pub kind: Delivery,
    /// May reach a working agent whose provider queues typed input.
    pub interrupt: bool,
    /// Who hands it over: `None` for the manager's own send, `watch` for a
    /// prepared step the watch delivers. Recorded on the timeline.
    pub via: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    Assignment,
    Correction,
    Reanchor,
}
impl Delivery {
    /// Timeline event recorded for this kind.
    pub fn event(self) -> &'static str {
        match self {
            Self::Assignment => "assigned",
            Self::Correction => "correction",
            Self::Reanchor => "reanchor",
        }
    }
    /// The hgm command that makes this delivery, for refusal records.
    pub fn command(self) -> &'static str {
        match self {
            Self::Assignment => "send",
            Self::Correction => "correct",
            Self::Reanchor => "reanchor",
        }
    }
}

pub(crate) const ASSIGNMENT_WORD_BUDGET: usize = 400;
pub(crate) const CORRECTION_WORD_BUDGET: usize = 150;
pub(crate) const ASSIGNMENT_BUDGET_SPENT: &str = "Assignment budget exhausted";
pub(crate) const CORRECTION_BUDGET_SPENT: &str = "Correction budget exhausted";

#[derive(Default)]
struct TaskHistory<'a> {
    assigned: u32,
    corrections: u32,
    plan_approved: bool,
    owner: Option<&'a str>,
}
pub(crate) fn plan_pending(events: &[Event], goal: &str) -> Result<Option<String>> {
    let mut pending: Vec<Option<String>> = vec![];
    for event in events {
        let task = event.detail["task"].as_str().map(String::from);
        match event.kind.as_str() {
            "assigned" if event.detail["plan"] == true && !pending.contains(&task) => {
                pending.push(task);
            }
            "plan_approved" => pending.retain(|t| *t != task),
            _ => {}
        }
    }
    match pending.as_slice() {
        [] => Ok(None),
        [task] => Ok(task.clone()),
        tasks => bail!(
            "plans for several tasks await approval; approve each one: {}",
            tasks
                .iter()
                .map(|task| match task {
                    Some(task) => format!("hgm approve-plan {goal} --task {task} --evidence TEXT"),
                    None => format!("hgm approve-plan {goal} --evidence TEXT"),
                })
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}
fn task_history<'a>(events: &'a [Event], task: Option<&str>) -> TaskHistory<'a> {
    let mut history = TaskHistory::default();
    for event in events {
        if event.detail["task"].as_str() != task {
            continue;
        }
        match event.kind.as_str() {
            "assigned" => {
                history.assigned += 1;
                history.owner = Some(event.detail["to"].as_str().unwrap_or("main"));
            }
            "correction" => history.corrections += 1,
            "plan_approved" => history.plan_approved = true,
            "task_reopened" => history = TaskHistory::default(),
            _ => {}
        }
    }
    history
}

pub(crate) fn budget_numbers(
    goal: &Goal,
    events: &[Event],
    now: DateTime<Utc>,
) -> (Option<(u32, u32)>, Option<i64>) {
    let policy = &goal.spec.loop_policy;
    let assigned = || events.iter().filter(|e| e.kind == "assigned");
    let assignments = assigned().next_back().map(|last| {
        let used = task_history(events, last.detail["task"].as_str()).assigned;
        (used, policy.max_assignments)
    });
    let left = assigned()
        .next()
        .zip(policy.max_hours)
        .map(|(first, hours)| i64::from(hours) * 60 - (now - first.at).num_minutes().max(0));
    (assignments, left)
}

pub(crate) fn budget_line(
    goal: &Goal,
    events: &[Event],
    wake: u32,
    max_wakes: u32,
    now: DateTime<Utc>,
) -> String {
    let hm = |minutes: i64| format!("{}h{:02}m", minutes / 60, minutes % 60);
    let policy = &goal.spec.loop_policy;
    let mut line = format!(
        "Now {}. Wake {wake} of {max_wakes}.",
        now.with_timezone(&Local).format("%Y-%m-%d %H:%M %:z")
    );
    let assigned = || events.iter().filter(|e| e.kind == "assigned");
    if let Some(last) = assigned().next_back() {
        let task = last.detail["task"].as_str();
        let used = task_history(events, task).assigned;
        line += &format!(
            " Assignments for {}: {used} of {}.",
            task.unwrap_or("this goal"),
            policy.max_assignments
        );
    }
    let Some(first) = assigned().next() else {
        return line;
    };
    let used = (now - first.at).num_minutes().max(0);
    let Some(hours) = policy.max_hours else {
        return line + &format!(" {} since the first assignment (no time budget).", hm(used));
    };
    let total = i64::from(hours) * 60;
    let left = (total - used).max(0);
    line += &format!(" Time used {} of {hours}h ({} left).", hm(used), hm(left));
    if left * 4 <= total {
        line += "\nFinalize window: finish and verify the current task, run the checks and prepare review; start no new scope.";
    }
    line
}

pub(crate) fn reanchor_due(
    goal: &Goal,
    events: &[Event],
    now: DateTime<Utc>,
) -> Option<(String, DateTime<Utc>)> {
    let first_work = events.iter().find(|e| e.kind == "assigned")?.at;
    let since = events
        .iter()
        .rev()
        .find(|e| e.kind == "reanchor")
        .map_or(first_work, |e| e.at.max(first_work));
    let first_confirmation = events.iter().find(|e| e.kind == "confirmed").map(|e| e.at);
    for event in events.iter().filter(|e| e.at > since) {
        let reason = match event.kind.as_str() {
            "compaction_detected" => "the agent's context was compacted",
            "restarted" => "the agent restarted in a fresh session",
            "task_reopened" => "the user changed the scope (revision)",
            "confirmed" if Some(event.at) != first_confirmation => {
                "the user confirmed a changed contract"
            }
            _ => continue,
        };
        return Some((reason.into(), event.at));
    }
    let period = chrono::Duration::minutes(i64::from(goal.spec.loop_policy.reanchor_minutes));
    (now - since >= period).then(|| {
        (
            format!(
                "{} minutes of work since the last goal check",
                goal.spec.loop_policy.reanchor_minutes
            ),
            since + period,
        )
    })
}

pub(crate) fn admit(store: &Store, assignment: &Assignment<'_>, text: &str) -> Result<String> {
    admission(store, assignment, text, true)
}
pub(crate) fn check(store: &Store, assignment: &Assignment<'_>, text: &str) -> Result<()> {
    admission(store, assignment, text, false).map(|_| ())
}
fn spent(store: &Store, id: &str, reason: String, wait: bool) -> anyhow::Error {
    if !wait {
        return anyhow::anyhow!("{reason}");
    }
    match store.update(id, |g| g.wait(reason.clone())) {
        Ok(_) => anyhow::anyhow!("{reason} The user has been asked."),
        Err(error) => error.context(reason),
    }
}
fn admission(
    store: &Store,
    assignment: &Assignment<'_>,
    text: &str,
    wait_on_spent: bool,
) -> Result<String> {
    let words = word_units(text);
    let limit = match assignment.kind {
        Delivery::Assignment => ASSIGNMENT_WORD_BUDGET,
        Delivery::Correction | Delivery::Reanchor => CORRECTION_WORD_BUDGET,
    };
    ensure!(
        words <= limit,
        "the text is about {words} words (budget {limit}); send a one-shot spec and point to files for the rest"
    );
    let Some(id) = assignment.goal else {
        ensure!(
            assignment.kind == Delivery::Assignment && !assignment.interrupt,
            "corrections, goal checks and interrupting sends belong to a goal; pass --goal GOAL"
        );
        return Ok(text.into());
    };
    let goal = store.load(id)?;
    ensure!(
        !matches!(goal.status, GoalStatus::Completed | GoalStatus::Cancelled),
        "goal {} is {:?}; register a new goal for further work",
        goal.id,
        goal.status
    );
    if let Some(task) = assignment.task {
        let (index, task) = goal
            .tasks
            .iter()
            .enumerate()
            .find(|(_, t)| t.spec.id == task)
            .with_context(|| format!("unknown task ID {task}"))?;
        ensure!(
            task.spec.runner.is_none(),
            "task {} runs a configured command; it is not assigned to an agent: run it with hgm run GOAL",
            task.spec.id
        );
        if assignment.kind != Delivery::Reanchor {
            ensure!(
                !task.succeeded(),
                "task {} is complete; revise it before assigning more work",
                task.spec.id
            );
            ensure!(
                goal.dependencies_done(index),
                "task {} has unfinished dependencies",
                task.spec.id
            );
        }
    }
    if assignment.kind != Delivery::Reanchor {
        ensure!(
            crate::watch::short_of_completion(&goal),
            "goal {id} is paused, waiting, or ready for review; resolve its current state before assigning work"
        );
        ensure!(
            goal.tasks.is_empty() || goal.tasks.iter().any(|task| !task.succeeded()),
            "all tasks on goal {id} are complete; verify them or revise the goal before assigning work"
        );
    }
    ensure!(
        goal.confirmed.is_some(),
        "goal {id} has no confirmed loop contract; show the user the outcome, checks and budget, then record their words with hgm confirm {id} --evidence TEXT"
    );
    let policy = &goal.spec.loop_policy;
    ensure!(
        policy.autonomy != Autonomy::Ask
            || assignment.kind == Delivery::Reanchor
            || assignment
                .user_approval
                .is_some_and(|a| !a.trim().is_empty()),
        "goal {id} uses autonomy ask; show this to the user and pass their words with --user-approval"
    );
    let events = store.events(id)?;
    let history = task_history(&events, assignment.task);
    let task = assignment.task.unwrap_or("this goal");
    if let Some(owner) = history.owner {
        ensure!(
            owner == assignment.to,
            "{task} belongs to {owner}; send its remaining work to {owner}, or split the work into separate tasks; a completed task can be revised (hgm revise) and then reassigned"
        );
    }
    let list = |items: &[String]| {
        if items.is_empty() {
            "(none)".to_string()
        } else {
            items.join("; ")
        }
    };
    match assignment.kind {
        Delivery::Correction => {
            if history.corrections >= policy.max_corrections {
                let reason = format!(
                    "{CORRECTION_BUDGET_SPENT} for {task} ({} of {}). The work keeps leaving the contract; decide whether to revise, re-scope, or stop.",
                    history.corrections, policy.max_corrections
                );
                return Err(spent(store, id, reason, wait_on_spent));
            }
            return Ok(format!(
                "Goal Manager correction for goal {id}, {task}: {text}\nStop the step that leaves the contract, fix it, then continue the assignment and report what changed. The contract of your assignment still binds you: {} show {id}.",
                hgm_for(store, assignment.to)?,
            ));
        }
        Delivery::Reanchor => {
            let hgm = hgm_for(store, assignment.to)?;
            let flag = assignment
                .task
                .map(|t| format!(" --task {t}"))
                .unwrap_or_default();
            return Ok(format!(
                "Goal check from the Goal Manager (goal {id}, {task}). When your current step is done, before the next one, restate in at most 5 lines: 1. the goal in your own words, 2. your current task, 3. acceptance items done and how you know, 4. your next step, 5. anything that drifted from the contract. Record it with {hgm} report {id}{flag} \"Goal restatement: ...\", then continue your work.{}\nGoal: {}\nAcceptance: {}\nInvariants: {}\nOut of scope: {}",
                if text.trim().is_empty() {
                    String::new()
                } else {
                    format!("\n{text}")
                },
                goal.spec.objective,
                list(&goal.spec.acceptance),
                list(&goal.spec.invariants),
                list(&goal.spec.out_of_scope),
            ));
        }
        Delivery::Assignment => {}
    }
    let approve = match assignment.task {
        Some(task) => format!("hgm approve-plan {id} --task {task} --evidence TEXT"),
        None => format!("hgm approve-plan {id} --evidence TEXT"),
    };
    ensure!(
        !policy.plan_first || assignment.plan || history.plan_approved,
        "goal {id} requires a plan first for {task}: send --plan to ask for one, review it, then record {approve}"
    );
    if history.assigned >= policy.max_assignments {
        let reason = format!(
            "{ASSIGNMENT_BUDGET_SPENT} for {task} ({} of {}). Decide with the user whether to stop, or to go on under a new goal with a larger loop.max_assignments (revise reopens only a completed task).",
            history.assigned, policy.max_assignments
        );
        return Err(spent(store, id, reason, wait_on_spent));
    }
    let request = if assignment.plan {
        "Write a short plan only (steps, files, assumptions, open questions); do not change files yet."
    } else {
        ""
    };
    let practice = crate::config::load(&store.workspace)?
        .assignment_append
        .unwrap_or_default();
    let place = goal
        .checkout
        .as_ref()
        .map(|checkout| {
            format!(
                "Work in {} (branch {}): edit, build and commit there.\n",
                checkout.path.display(),
                checkout.branch
            )
        })
        .unwrap_or_default();
    let scope = assignment
        .task
        .and_then(|task| goal.tasks.iter().find(|t| t.spec.id == task))
        .filter(|task| !task.spec.write_paths.is_empty())
        .map(|task| {
            format!(
                "Write only in: {}. hgm records changes elsewhere during this assignment and tells the Goal Manager; it does not block them.\n",
                task.spec.write_paths.join(", ")
            )
        })
        .unwrap_or_default();
    Ok(format!(
        "Goal {id}, {task}, assigned to {}.\n{place}{scope}{text}\n{request}\n{practice}\n\n{}\n{}",
        assignment.to,
        goal.spec.assignment_contract(),
        record_line(store, id, assignment.task, assignment.to)?
    ))
}

fn record_line(store: &Store, id: &str, task: Option<&str>, to: &str) -> Result<String> {
    let hgm = hgm_for(store, to)?;
    let task = task.map(|t| format!(" --task {t}")).unwrap_or_default();
    Ok(format!(
        "Record each choice you make inside the contract when you make it: {hgm} decision {id}{task} \"WHAT\" --why \"WHY\". Record a finding that should not block this work: {hgm} follow-up {id}{task} \"WHAT\". File your report when you finish or stop, in full, so the manager reads it from hgm: {hgm} report {id}{task} \"TEXT\" (or --file PATH)."
    ))
}
fn hgm_for(store: &Store, to: &str) -> Result<String> {
    Ok(if to == "main" {
        let quote = |path: &std::path::Path| crate::manager::quote(path.as_os_str());
        let executable = std::env::current_exe()?.canonicalize()?;
        let mut command = format!(
            "{} --workspace {}",
            quote(&executable),
            quote(&store.workspace)
        );
        if crate::cli::default_state_root().ok().as_ref() != Some(&store.root) {
            command += &format!(" --state-dir {}", quote(&store.root));
        }
        command
    } else {
        "hgm".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use serde_json::json;

    #[test]
    fn budget_line_reports_time_left_and_asks_to_finish_in_the_last_quarter() {
        let spec = serde_json::from_value(
            json!({"objective":"o","acceptance":["a"],"loop":{"max_hours":4}}),
        )
        .unwrap();
        let goal = Goal::new(spec, std::env::temp_dir()).unwrap();
        let events = vec![Event::new(
            &goal.id,
            "assigned",
            json!({"task":"t","text":"x"}),
        )];
        let start = events[0].at;
        let line = budget_line(&goal, &events, 3, 100, start + Duration::minutes(90));
        assert!(line.contains("Wake 3 of 100"), "{line}");
        assert!(line.contains("Assignments for t: 1 of 3"), "{line}");
        assert!(
            line.contains("Time used 1h30m of 4h (2h30m left)"),
            "{line}"
        );
        assert!(!line.contains("Finalize"), "{line}");
        let late = budget_line(&goal, &events, 4, 100, start + Duration::minutes(190));
        assert!(late.contains("Finalize window"), "{late}");
    }

    #[test]
    fn goal_checks_fire_periodically_and_after_compaction_restart_or_scope_change() {
        let spec = serde_json::from_value(json!({"objective":"o","acceptance":["a"]})).unwrap();
        let goal = Goal::new(spec, std::env::temp_dir()).unwrap();
        let at = |minutes: i64, kind: &str| {
            let mut event = Event::new(&goal.id, kind, json!({}));
            event.at = DateTime::<Utc>::UNIX_EPOCH + Duration::minutes(minutes);
            event
        };
        let now = |minutes: i64| DateTime::<Utc>::UNIX_EPOCH + Duration::minutes(minutes);
        let mut events = vec![at(0, "confirmed")];
        assert!(
            reanchor_due(&goal, &events, now(500)).is_none(),
            "no work yet"
        );
        events.push(at(10, "assigned"));
        assert!(reanchor_due(&goal, &events, now(54)).is_none());
        let (reason, when) = reanchor_due(&goal, &events, now(55)).unwrap();
        assert!(reason.contains("45 minutes"), "{reason}");
        assert_eq!(when, now(55));
        events.push(at(56, "reanchor"));
        assert!(reanchor_due(&goal, &events, now(60)).is_none());
        for (kind, expected) in [
            ("compaction_detected", "compacted"),
            ("restarted", "restarted"),
            ("task_reopened", "scope"),
            ("confirmed", "changed contract"),
        ] {
            let mut with = events.clone();
            with.push(at(58, kind));
            let (reason, when) = reanchor_due(&goal, &with, now(59)).unwrap();
            assert!(reason.contains(expected), "{kind}: {reason}");
            assert_eq!(when, now(58));
        }
    }

    fn assignment<'a>(goal: &'a str, user_approval: Option<&'a str>) -> Assignment<'a> {
        Assignment {
            goal: Some(goal),
            task: Some("work"),
            supervise: false,
            user_approval,
            plan: false,
            to: "main",
            kind: Delivery::Assignment,
            interrupt: false,
            via: None,
        }
    }

    #[test]
    fn check_refuses_like_admit_but_never_sets_the_goal_waiting() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        let add = |yaml: &str| -> Result<String> {
            Ok(store.add(serde_norway::from_str(yaml)?, None)?.id)
        };
        let confirm = |id: &str| store.update(id, |g| g.confirm("Yes".into(), String::new()));
        let unconfirmed =
            add("objective: o\nacceptance: [a]\nchecks: [{name: c, command: 'true'}]\n")?;
        let error = check(&store, &assignment(&unconfirmed, None), "Do it").unwrap_err();
        assert!(format!("{error:#}").contains("no confirmed loop contract"));
        confirm(&unconfirmed)?;
        let long = "word ".repeat(ASSIGNMENT_WORD_BUDGET + 1);
        assert!(
            format!(
                "{:#}",
                check(&store, &assignment(&unconfirmed, None), &long).unwrap_err()
            )
            .contains("budget 400")
        );
        let planned = add(
            "objective: o\nacceptance: [a]\nchecks: [{name: c, command: 'true'}]\nloop: {plan_first: true}\n",
        )?;
        confirm(&planned)?;
        let error = format!(
            "{:#}",
            check(&store, &assignment(&planned, None), "Do it").unwrap_err()
        );
        assert!(
            error.contains(&format!(
                "hgm approve-plan {planned} --task work --evidence TEXT"
            )),
            "{error}"
        );
        let ask = add("objective: o\nacceptance: [a]\nloop: {autonomy: ask}\n")?;
        confirm(&ask)?;
        assert!(
            format!(
                "{:#}",
                check(&store, &assignment(&ask, None), "Do it").unwrap_err()
            )
            .contains("--user-approval")
        );
        check(&store, &assignment(&ask, Some("User: yes")), "Do it")?;
        let spent = add(
            "objective: o\nacceptance: [a]\nchecks: [{name: c, command: 'true'}]\nloop: {max_assignments: 1}\n",
        )?;
        confirm(&spent)?;
        store.record(&spent, "assigned", json!({"task": "work", "to": "main"}))?;
        assert!(check(&store, &assignment(&spent, None), "Again").is_err());
        assert!(store.load(&spent)?.waiting.is_none());
        assert!(admit(&store, &assignment(&spent, None), "Again").is_err());
        assert!(store.load(&spent)?.waiting.is_some(), "admit asks the user");
        Ok(())
    }

    #[test]
    fn approve_plan_without_a_task_finds_the_one_plan_awaiting_approval() -> Result<()> {
        let event = |kind: &str, detail: serde_json::Value| Event::new("g-1", kind, detail);
        assert_eq!(plan_pending(&[], "g-1")?, None);
        let one = [event("assigned", json!({"task": "work", "plan": true}))];
        assert_eq!(plan_pending(&one, "g-1")?, Some("work".into()));
        let approved = [
            one[0].clone(),
            event("plan_approved", json!({"task": "work"})),
        ];
        assert_eq!(plan_pending(&approved, "g-1")?, None);
        let two = [
            one[0].clone(),
            event("assigned", json!({"task": "docs", "plan": true})),
        ];
        let error = plan_pending(&two, "g-1").unwrap_err().to_string();
        assert!(
            error.contains("hgm approve-plan g-1 --task work --evidence TEXT")
                && error.contains("--task docs"),
            "{error}"
        );
        Ok(())
    }

    fn words(text: &str) -> usize {
        text.split_whitespace().count()
    }

    #[test]
    fn every_text_hgm_types_stays_within_its_word_budget() -> Result<()> {
        const ASSIGNMENT_APPENDIX: usize = 160;
        const CORRECTION: usize = 45;
        const GOAL_CHECK: usize = 90;
        const HANDOFF_REQUEST: usize = 100;
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        let goal = store.add(
            serde_norway::from_str(
                "objective: o\nacceptance: [a]\nchecks: [{name: c, command: 'true'}]\n",
            )?,
            None,
        )?;
        store.update(&goal.id, |g| g.confirm("User: go".into(), String::new()))?;
        let assignment = |kind| Assignment {
            goal: Some(&goal.id),
            task: Some("work"),
            supervise: false,
            user_approval: None,
            plan: false,
            to: "main",
            kind,
            interrupt: false,
            via: None,
        };
        let sent = admit(&store, &assignment(Delivery::Assignment), "Do it")?;
        let appendix = words(&sent) - words("Do it");
        println!("assignment appendix: {appendix} words");
        assert!(appendix <= ASSIGNMENT_APPENDIX, "{appendix}: {sent}");
        let correction = admit(&store, &assignment(Delivery::Correction), "Fix it")?;
        println!("correction: {} words", words(&correction));
        assert!(words(&correction) <= CORRECTION, "{correction}");
        let check = admit(&store, &assignment(Delivery::Reanchor), "")?;
        println!("goal check: {} words", words(&check));
        assert!(words(&check) <= GOAL_CHECK, "{check}");
        assert!(check.contains("report ") && check.contains("\"Goal restatement: ...\""));
        let handoff = crate::handoff::handoff_prompt(
            std::path::Path::new("/tmp/handoff.md"),
            &json!({"transfer":"t-1","safe_to_restart":true}),
            std::path::Path::new("/tmp/ready.json"),
        );
        println!("handoff request: {} words", words(&handoff));
        assert!(words(&handoff) <= HANDOFF_REQUEST, "{handoff}");
        Ok(())
    }
}
