//! Next steps the manager prepares while its worker works: a short queue of
//! linked assignments per role, each checked like any send when it is
//! stored, handed over by the watch one at a time as the worker settles (see
//! `watch`). Each manager context keeps its own queues next to its watch record.
use crate::{
    admission::{self, Assignment, Delivery},
    binding::{path, session, wait_operation_lease},
    model::validate_id,
    store::Store,
    watch::{load_watch, save_watch},
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Steps a role's queue holds at most: on 2026-11-15 the manager wanted two to
/// four in a row, sent ahead with `--interrupt` for lack of a queue.
pub(crate) const MAX_PREPARED: usize = 5;

/// The step the watch delivers to `to` when it settles.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Prepared {
    pub version: u32,
    pub goal: String,
    pub task: Option<String>,
    pub to: String,
    pub text: String,
    pub plan: bool,
    pub user_approval: Option<String>,
    pub prepared_at: DateTime<Utc>,
}
impl Prepared {
    /// The send the watch makes: normal admission, recorded as by the watch.
    pub fn assignment(&self) -> Assignment<'_> {
        Assignment {
            goal: Some(&self.goal),
            task: self.task.as_deref(),
            supervise: false,
            user_approval: self.user_approval.as_deref(),
            plan: self.plan,
            to: &self.to,
            kind: Delivery::Assignment,
            interrupt: false,
            via: Some("watch"),
        }
    }
}

fn file(store: &Store, context: &str, role: &str) -> Result<PathBuf> {
    validate_id(role)?;
    path(store, context, &format!("-prepared-{role}.json"))
}
/// The steps queued for `role` in manager context `context`, next first. A
/// single step stored by an earlier build reads as a queue of one.
pub(crate) fn queue(store: &Store, context: &str, role: &str) -> Result<Vec<Prepared>> {
    let bytes = match std::fs::read(file(store, context, role)?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error).context("read prepared steps"),
    };
    let value: Value = serde_json::from_slice(&bytes).context("parse prepared steps")?;
    Ok(if value.is_array() {
        serde_json::from_value(value).context("parse prepared steps")?
    } else {
        vec![serde_json::from_value(value).context("parse prepared step")?]
    })
}
fn save(store: &Store, context: &str, role: &str, steps: &[Prepared]) -> Result<()> {
    if steps.is_empty() {
        return match std::fs::remove_file(file(store, context, role)?) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(error).context("remove prepared steps")
            }
            _ => Ok(()),
        };
    }
    store.write_private(&file(store, context, role)?, &steps)
}
/// The next step for `role` in manager context `context`, if any.
pub(crate) fn load(store: &Store, context: &str, role: &str) -> Result<Option<Prepared>> {
    Ok(queue(store, context, role)?.into_iter().next())
}
/// Takes the next step for `role` off its queue without a timeline record
/// (it was delivered, or its delivery is recorded as uncertain).
pub(crate) fn remove(store: &Store, context: &str, role: &str) -> Result<()> {
    let mut steps = queue(store, context, role)?;
    if !steps.is_empty() {
        steps.remove(0);
    }
    save(store, context, role, &steps)
}
/// Removes the queued steps for `role` (one, by its 1-based place, or all)
/// and records why on each step's goal. Returns the steps removed.
pub(crate) fn drop_steps(
    store: &Store,
    context: &str,
    role: &str,
    place: Option<usize>,
    reason: &str,
) -> Result<Vec<Prepared>> {
    let mut steps = queue(store, context, role)?;
    let dropped = match place {
        None => std::mem::take(&mut steps),
        Some(n) => {
            ensure!(
                (1..=steps.len()).contains(&n),
                "{role} has {} prepared step(s); --step takes 1 to {}",
                steps.len(),
                steps.len().max(1)
            );
            vec![steps.remove(n - 1)]
        }
    };
    save(store, context, role, &steps)?;
    for step in &dropped {
        store.record(
            &step.goal,
            "prepared_withdrawn",
            json!({"task": step.task, "to": step.to, "reason": reason}),
        )?;
    }
    Ok(dropped)
}

/// Queues `text` as a next step for `assignment.to`, after any already
/// queued, up to [`MAX_PREPARED`]. Every refusal of a send applies now
/// (confirmation, word limit, plan approval, `ask` approval, budget, one owner
/// per task), so a step that could never be admitted is not kept. The watch of
/// this tab must observe that role; a goal it does not serve yet joins it,
/// since the step goes to the pane it watches.
pub(crate) async fn prepare(
    store: &Store,
    assignment: &Assignment<'_>,
    text: &str,
) -> Result<Value> {
    let goal = assignment
        .goal
        .context("a prepared step belongs to a goal; pass --goal")?;
    admission::check(store, assignment, text)?;
    let context = session(store)?;
    let key = context.key();
    let _operation = wait_operation_lease(store, &key).await?;
    let mut watch = load_watch(store, &key)
        .ok()
        .filter(|watch| watch.active)
        .context("no watch runs for this tab, so nothing would deliver the step; send it now, or supervise the goal first")?;
    ensure!(
        watch.role == assignment.to,
        "the watch observes {}, not {}; a step for {} would never be delivered",
        watch.role,
        assignment.to,
        assignment.to
    );
    let mut steps = queue(store, &key, assignment.to)?;
    ensure!(
        steps.len() < MAX_PREPARED,
        "{} already has {MAX_PREPARED} prepared steps; let the watch deliver some, or withdraw one with hgm send --withdraw --to {} --step N",
        assignment.to,
        assignment.to
    );
    if !watch.serves(goal) {
        watch.join(goal);
        save_watch(store, &watch)?;
        store.record(
            goal,
            "watch_joined",
            json!({"pane": watch.origin.pane, "goals": watch.members(), "via": "prepare"}),
        )?;
    }
    let step = Prepared {
        version: 1,
        goal: goal.into(),
        task: assignment.task.map(String::from),
        to: assignment.to.into(),
        text: text.into(),
        plan: assignment.plan,
        user_approval: assignment.user_approval.map(String::from),
        prepared_at: Utc::now(),
    };
    steps.push(step.clone());
    save(store, &key, assignment.to, &steps)?;
    // The watch tells its next reason to keep the next step afresh.
    watch.prepared_held = None;
    save_watch(store, &watch)?;
    store.record(
        goal,
        "prepared",
        json!({
            "task": step.task, "to": step.to, "text": step.text, "plan": step.plan,
            "replaced": null, "place": steps.len(),
        }),
    )?;
    Ok(json!({
        "status": "prepared",
        "goal": goal,
        "task": step.task,
        "to": step.to,
        "replaced": false,
        "place": steps.len(),
        "queued": steps.len(),
        "message": format!(
            "Step {} of {} for {}: the watch delivers one each time {} settles, in order, unless a regressed check, stop condition, protected path, budget or busy input holds it; withdraw one with hgm send --withdraw --to {} --step N, or all without --step",
            steps.len(), steps.len(), step.to, step.to, step.to
        ),
    }))
}
/// Withdraws the steps queued for `role`: the one at 1-based `place`, or all.
pub(crate) async fn withdraw(store: &Store, role: &str, place: Option<usize>) -> Result<Value> {
    let key = session(store)?.key();
    let _operation = wait_operation_lease(store, &key).await?;
    let dropped = drop_steps(store, &key, role, place, "withdrawn by the manager")?;
    ensure!(!dropped.is_empty(), "no step is prepared for {role}");
    let left = queue(store, &key, role)?.len();
    let first = &dropped[0];
    Ok(
        json!({"status": "withdrawn", "goal": first.goal, "task": first.task, "to": first.to,
              "withdrawn": dropped.len(), "left": left}),
    )
}
