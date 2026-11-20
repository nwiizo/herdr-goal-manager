use crate::{
    admission::{ASSIGNMENT_BUDGET_SPENT, CORRECTION_BUDGET_SPENT},
    config::Config,
    events::Event,
    model::{Goal, GoalStatus},
    queue::{Queue, State},
    store::Store,
    watch::{TIME_BUDGET_SPENT, WAKE_BUDGET_SPENT},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OwnCheckout {
    Off,
    Skipped(String),
    On,
}
impl OwnCheckout {
    pub fn new(config: &Config, workspace: &std::path::Path) -> Self {
        let git = crate::fingerprint::is_git(workspace);
        match (config.factory.checkout, git) {
            (Some(false), _) | (None, false) => Self::Off,
            (Some(true), false) => Self::Skipped(format!(
                "factory.checkout is on, but {} is not a Git repository; the goal works in the workspace",
                workspace.display()
            )),
            (_, true) => Self::On,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Gate {
    /// Results waiting for the person's review.
    pub awaiting: Vec<String>,
    /// Confirmed goals still in progress.
    pub active: Vec<String>,
    max_awaiting: u32,
    max_active: u32,
}
impl Gate {
    pub fn new(goals: &[Goal], config: &Config) -> Self {
        let ids = |keep: &dyn Fn(&Goal) -> bool| {
            goals
                .iter()
                .filter(|g| keep(g))
                .map(|g| g.id.clone())
                .collect()
        };
        Self {
            awaiting: ids(&|g| g.status == GoalStatus::ReadyForReview),
            active: ids(&|g| {
                g.confirmed.is_some()
                    && !matches!(
                        g.status,
                        GoalStatus::ReadyForReview | GoalStatus::Completed | GoalStatus::Cancelled
                    )
            }),
            max_awaiting: config.factory.max_awaiting_review(),
            max_active: config.factory.max_active(),
        }
    }
    /// Why the queue may not hand out work now, and the goals involved.
    pub fn held(&self) -> Option<(String, Vec<String>)> {
        let mut reasons = vec![];
        let mut goals = vec![];
        if self.awaiting.len() >= self.max_awaiting as usize {
            reasons.push(format!(
                "factory.max_awaiting_review ({}) reached: {} await review (hgm accept or hgm revise)",
                self.max_awaiting,
                self.awaiting.join(", ")
            ));
            goals.extend(self.awaiting.iter().cloned());
        }
        if self.active.len() >= self.max_active as usize {
            reasons.push(format!(
                "factory.max_active ({}) reached: {} in progress (finish or cancel one first; a waiting goal still counts)",
                self.max_active,
                self.active.join(", ")
            ));
            goals.extend(self.active.iter().cloned());
        }
        (!reasons.is_empty()).then(|| (reasons.join("; "), goals))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RunClass {
    pub class: Option<&'static str>,
    pub reasons: Vec<String>,
}

const NEEDS_INPUT: [&str; 5] = [
    "dialog",
    "login",
    "limit",
    "provider_errors",
    "manager_blocked",
];

fn stop(event: &Event) -> Option<String> {
    let text = |key: &str| event.detail[key].as_str().unwrap_or_default();
    match event.kind.as_str() {
        "boundary_crossed" => Some("protected paths changed".into()),
        "stop_condition" => Some(format!("stop condition {} holds", text("check"))),
        "watch_ended"
            if text("reason").starts_with(TIME_BUDGET_SPENT)
                || text("reason").starts_with(WAKE_BUDGET_SPENT) =>
        {
            Some(
                text("reason")
                    .split(':')
                    .next()
                    .unwrap_or_default()
                    .replace('_', " "),
            )
        }
        "waiting"
            if text("reason").starts_with(ASSIGNMENT_BUDGET_SPENT)
                || text("reason").starts_with(CORRECTION_BUDGET_SPENT) =>
        {
            Some(text("reason").split(" (").next().unwrap_or_default().into())
        }
        "attention" if NEEDS_INPUT.contains(&text("cause")) => {
            Some(format!("needs input: {}", text("reason")))
        }
        _ => None,
    }
}
fn resumes(event: &Event) -> bool {
    match event.kind.as_str() {
        "assigned" | "correction" | "confirmed" | "task_reopened" | "wake" | "patrol"
        | "watch_joined" => true,
        "status" => matches!(event.detail["to"].as_str(), Some("ready" | "running")),
        _ => false,
    }
}

pub(crate) fn run_class(events: &[Event]) -> RunClass {
    let at = |event: &Event| crate::events::local_clock(event.at);
    let mut flaws = vec![];
    let mut passed_settles: Vec<&str> = vec![];
    for event in events {
        match event.kind.as_str() {
            "checks" if event.detail["passed"] == false => {
                flaws.push(format!("checks failed {}", at(event)));
            }
            "settle_checks" => {
                let failed = event.detail["failed"].as_array().into_iter().flatten();
                for name in failed.filter_map(|check| check["name"].as_str()) {
                    if passed_settles.contains(&name) {
                        flaws.push(format!("check {name} regressed {}", at(event)));
                    }
                }
                let passed = event.detail["passed"].as_array().into_iter().flatten();
                passed_settles.extend(passed.filter_map(Value::as_str));
            }
            "task_reopened" => flaws.push(format!("revised {}", at(event))),
            _ => {}
        }
    }
    let accepted = events.iter().any(|e| e.kind == "accepted");
    let worked = events.iter().any(|e| e.kind == "assigned");
    let blocked = events
        .iter()
        .rposition(|e| stop(e).is_some())
        .filter(|&index| !events[index + 1..].iter().any(resumes))
        .and_then(|index| stop(&events[index]));
    let cancelled = events
        .iter()
        .rev()
        .find(|e| e.kind == "status")
        .is_some_and(|e| e.detail["to"] == "cancelled");
    let (class, reasons) = if accepted && !worked {
        ("manual", vec!["accepted without agent work".to_string()])
    } else if accepted && !flaws.is_empty() {
        ("flawed", flaws)
    } else if accepted {
        ("success", vec![])
    } else if let Some(reason) = blocked {
        ("blocked", vec![reason])
    } else if cancelled {
        ("manual", vec!["cancelled by a person".to_string()])
    } else if !flaws.is_empty() {
        ("flawed", flaws)
    } else {
        return RunClass {
            class: None,
            reasons: vec![],
        };
    };
    RunClass {
        class: Some(class),
        reasons,
    }
}

pub(crate) struct Factory<'a> {
    store: &'a Store,
    config: Config,
    items: BTreeMap<String, String>,
    queued: usize,
}
impl<'a> Factory<'a> {
    /// Reads the queue when factory mode is on; `None` leaves every view as it
    /// was. Views that never read the config before (status, show, inbox,
    /// overview) keep working with an unreadable one; the queue commands
    /// report its error.
    pub fn load(store: &'a Store) -> Result<Option<Self>> {
        let Ok(config) = crate::config::load(&store.workspace) else {
            return Ok(None);
        };
        if config.require_factory().is_err() {
            return Ok(None);
        }
        let statuses = Queue::open(store)?.statuses()?;
        let items = statuses
            .iter()
            .filter(|s| s.state == State::Dispatched)
            .filter_map(|s| Some((s.goal.clone()?, s.id.clone())))
            .collect();
        let queued = statuses.iter().filter(|s| s.state == State::Queued).count();
        Ok(Some(Self {
            store,
            config,
            items,
            queued,
        }))
    }
    /// Queue item and run class of a goal taken from the queue.
    pub fn describe(&self, goal: &str) -> Result<Option<Value>> {
        let Some(item) = self.items.get(goal) else {
            return Ok(None);
        };
        let class = run_class(&self.store.events(goal)?);
        Ok(Some(json!({
            "item": item,
            "run_class": class.class,
            "run_class_reasons": class.reasons,
        })))
    }
    /// Adds `run_class` to status rows of goals taken from the queue.
    pub fn annotate(&self, rows: &mut [Value]) -> Result<()> {
        for row in rows {
            let id = row["id"].as_str().unwrap_or_default().to_string();
            if let Some(described) = self.describe(&id)? {
                row["run_class"] = described["run_class"].clone();
            }
        }
        Ok(())
    }
    fn gate(&self) -> Result<Gate> {
        Ok(Gate::new(&self.store.list()?, &self.config))
    }
    /// The inbox row while queued work waits behind the gate.
    pub fn held_row(&self) -> Result<Option<Value>> {
        if self.queued == 0 {
            return Ok(None);
        }
        Ok(self.gate()?.held().map(|(reason, goals)| {
            json!({
                "id": "queue",
                "objective": format!("Factory queue ({} waiting)", self.queued),
                "kind": "queue_held",
                "reason": reason,
                "since": null,
                "goals": goals,
            })
        }))
    }
    /// The overview line while queued work waits and the gate is open.
    pub fn ready_line(&self) -> Result<Option<String>> {
        if self.queued == 0 || self.gate()?.held().is_some() {
            return Ok(None);
        }
        Ok(Some(format!(
            "{} agreed contract{} queued; dispatch the next with hgm queue next",
            self.queued,
            if self.queued == 1 { "" } else { "s" }
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::GoalSpec,
        runner::{self, CommandExecutor, Options},
    };
    use chrono::{Duration, Utc};

    #[test]
    fn factory_checkout_is_on_by_default_only_in_a_git_repository() -> Result<()> {
        let plain = tempfile::tempdir()?;
        let repository = tempfile::tempdir()?;
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(repository.path())
            .output()?;
        assert!(init.status.success());
        let with = |checkout: Option<bool>| {
            let mut config = Config::default();
            config.factory.checkout = checkout;
            config
        };
        use OwnCheckout as O;
        assert_eq!(O::new(&with(None), repository.path()), O::On);
        assert_eq!(O::new(&with(Some(true)), repository.path()), O::On);
        assert_eq!(O::new(&with(Some(false)), repository.path()), O::Off);
        assert_eq!(O::new(&with(None), plain.path()), O::Off);
        assert_eq!(O::new(&with(Some(false)), plain.path()), O::Off);
        let O::Skipped(reason) = O::new(&with(Some(true)), plain.path()) else {
            panic!("an explicit checkout outside Git is skipped with a reason");
        };
        assert!(reason.contains("is not a Git repository"), "{reason}");
        assert_eq!(
            crate::queue::branch_for("q-1a2b3c4d-5e6f-7a8b-9c0d-112233445566"),
            "hgm/q-1a2b3c4d"
        );
        Ok(())
    }

    fn event(minute: i64, kind: &str, detail: Value) -> Event {
        let mut event = Event::new("g-1", kind, detail);
        event.at = chrono::DateTime::<Utc>::UNIX_EPOCH + Duration::minutes(minute);
        event
    }
    fn class(events: &[Event]) -> Option<&'static str> {
        run_class(events).class
    }

    #[test]
    fn a_settle_failure_is_a_flaw_only_when_the_check_passed_before() {
        let unfinished = event(
            2,
            "settle_checks",
            json!({"passed": [], "failed": [{"name": "tests"}]}),
        );
        let passing = event(
            3,
            "settle_checks",
            json!({"passed": ["tests"], "failed": []}),
        );
        let regressed = event(
            4,
            "settle_checks",
            json!({"passed": [], "failed": [{"name": "tests"}]}),
        );
        let assigned = event(1, "assigned", json!({"task": "work"}));
        let accepted = event(9, "accepted", json!({"evidence": "ok"}));
        assert_eq!(
            class(&[
                assigned.clone(),
                unfinished.clone(),
                passing.clone(),
                accepted.clone()
            ]),
            Some("success"),
            "a check that never passed was unfinished work"
        );
        let at = crate::events::local_clock(regressed.at);
        let flawed = run_class(&[assigned, unfinished, passing, regressed, accepted]);
        assert_eq!(flawed.class, Some("flawed"));
        assert_eq!(flawed.reasons, [format!("check tests regressed {at}")]);
    }

    #[test]
    fn blocked_means_stopped_now_and_never_after_acceptance() {
        let assigned = event(1, "assigned", json!({}));
        let dialog = event(
            2,
            "attention",
            json!({"cause": "dialog", "reason": "approval shown"}),
        );
        let stalled = event(
            2,
            "attention",
            json!({"cause": "stalled", "reason": "no change"}),
        );
        assert_eq!(class(&[assigned.clone(), dialog.clone()]), Some("blocked"));
        assert_eq!(
            class(&[assigned.clone(), stalled]),
            None,
            "not needed input"
        );
        let wake = event(3, "wake", json!({}));
        assert_eq!(
            class(&[assigned.clone(), dialog.clone(), wake]),
            None,
            "watch resumed"
        );
        let later = event(3, "assigned", json!({}));
        assert_eq!(class(&[assigned.clone(), dialog.clone(), later]), None);
        let accepted = event(4, "accepted", json!({}));
        assert_eq!(
            class(&[assigned.clone(), dialog.clone(), accepted]),
            Some("success"),
            "R7: accepted after a dialog"
        );
        for stop in [
            event(2, "boundary_crossed", json!({})),
            event(2, "stop_condition", json!({"check": "auth"})),
            event(2, "watch_ended", json!({"reason": "time_budget_exhausted"})),
            event(
                2,
                "watch_ended",
                json!({"reason": "wake_budget_exhausted: the watch spent"}),
            ),
            event(
                2,
                "waiting",
                json!({"reason": "Assignment budget exhausted for work (3 of 3)."}),
            ),
            event(
                2,
                "waiting",
                json!({"reason": "Correction budget exhausted for work (5 of 5)."}),
            ),
        ] {
            let found = run_class(&[assigned.clone(), stop.clone()]);
            assert_eq!(found.class, Some("blocked"), "{stop:?}");
            assert!(!found.reasons[0].is_empty());
            let resumed = event(5, "status", json!({"from": "paused", "to": "ready"}));
            assert_eq!(class(&[assigned.clone(), stop, resumed]), None);
        }
        let cancelled = event(6, "status", json!({"from": "paused", "to": "cancelled"}));
        assert_eq!(
            class(&[assigned.clone(), dialog, cancelled]),
            Some("blocked")
        );
    }

    #[test]
    fn manual_work_and_agent_reports_never_make_a_success() {
        let accepted = event(4, "accepted", json!({}));
        assert_eq!(class(std::slice::from_ref(&accepted)), Some("manual"));
        let cancelled = event(4, "status", json!({"from": "ready", "to": "cancelled"}));
        assert_eq!(
            class(&[event(1, "assigned", json!({})), cancelled]),
            Some("manual"),
            "R8"
        );
        let reports = [
            event(1, "assigned", json!({})),
            event(2, "report", json!({"text": "all done, tests pass"})),
            event(3, "reviewed", json!({"verdict": "approve", "findings": []})),
        ];
        assert_eq!(class(&reports), None, "reports and verdicts decide nothing");
        let failed = event(2, "checks", json!({"passed": false}));
        assert_eq!(
            class(&[event(1, "assigned", json!({})), failed]),
            Some("flawed")
        );
    }

    #[tokio::test]
    async fn the_normal_path_is_a_success_and_a_revision_makes_it_flawed() -> Result<()> {
        async fn walk(revise: bool) -> Result<RunClass> {
            let workspace = tempfile::tempdir()?;
            let state = tempfile::tempdir()?;
            let store = Store::new(workspace.path(), state.path())?;
            let spec: GoalSpec = serde_norway::from_str(
                "objective: Summary\nacceptance: [Done]\nchecks: [{name: ok, command: 'true'}]\n",
            )?;
            let id = store.add(spec, None)?.id;
            runner::confirm_goal(&store, &id, "Yes").await?;
            store.record(&id, "assigned", json!({"task": "work", "text": "Do it"}))?;
            let options = || Options {
                resume: false,
                timeout_secs: 30,
                max_attempts: 1,
                max_repairs: 0,
            };
            let goal = runner::run(&store, &id, &CommandExecutor, options()).await?;
            assert_eq!(goal.status, GoalStatus::NeedsInput);
            assert_eq!(
                run_class(&store.events(&id)?).class,
                None,
                "needs_input is not blocked"
            );
            store.update(&id, |g| g.complete_manual("work", "Written".into()))?;
            runner::run(&store, &id, &CommandExecutor, options()).await?;
            if revise {
                store.update(&id, |g| g.revise("Add the tax".into(), &[]).map(|_| ()))?;
                store.update(&id, |g| g.complete_manual("work", "Fixed".into()))?;
                runner::run(&store, &id, &CommandExecutor, options()).await?;
            }
            runner::accept(&store, &id, "Reviewed", &CommandExecutor).await?;
            Ok(run_class(&store.events(&id)?))
        }
        assert_eq!(walk(false).await?.class, Some("success"));
        let flawed = walk(true).await?;
        assert_eq!(flawed.class, Some("flawed"));
        assert!(
            flawed.reasons[0].starts_with("revised"),
            "{:?}",
            flawed.reasons
        );
        Ok(())
    }

    #[test]
    fn the_gate_names_the_goals_that_hold_it() -> Result<()> {
        let goal = |status: GoalStatus, confirmed: bool| -> Result<Goal> {
            let spec: GoalSpec = serde_norway::from_str("objective: o\nacceptance: [a]\n")?;
            let mut goal = Goal::new(spec, std::env::temp_dir())?;
            goal.status = status;
            goal.confirmed = confirmed.then(|| crate::model::Evidence {
                note: "yes".into(),
                recorded_at: Utc::now(),
            });
            Ok(goal)
        };
        let config = Config::default();
        let draft = goal(GoalStatus::Ready, false)?;
        let done = goal(GoalStatus::Completed, true)?;
        assert!(
            Gate::new(&[draft, done], &config).held().is_none(),
            "unconfirmed and finished goals do not count"
        );
        let working = goal(GoalStatus::Running, true)?;
        let (reason, goals) = Gate::new(std::slice::from_ref(&working), &config)
            .held()
            .unwrap();
        assert!(
            reason.contains("factory.max_active (1)") && reason.contains(&working.id),
            "{reason}"
        );
        assert_eq!(goals, std::slice::from_ref(&working.id));
        let review = [
            goal(GoalStatus::ReadyForReview, true)?,
            goal(GoalStatus::ReadyForReview, true)?,
        ];
        let (reason, goals) = Gate::new(&review, &config).held().unwrap();
        assert!(
            reason.contains("factory.max_awaiting_review (2)"),
            "{reason}"
        );
        assert!(!reason.contains("max_active"), "{reason}");
        assert_eq!(goals.len(), 2);
        Ok(())
    }
}
