//! Shared human-readable goal views.
use crate::{
    model::{Attempt, Control, Goal},
    store::Store,
};
use anyhow::Result;
pub(crate) fn print_list(goals: &[Goal]) {
    if goals.is_empty() {
        println!("No goals. Use 'new' in the panel or 'hgm init' for a goal file.");
    }
    for (i, goal) in goals.iter().enumerate() {
        println!(
            "{}  {:?}  {}/{} tasks  {} [{}]",
            i + 1,
            goal.status,
            goal.tasks.iter().filter(|t| t.succeeded()).count(),
            goal.tasks.len(),
            goal.spec.objective,
            goal.id
        );
    }
}
pub(crate) fn print_goal(goal: &Goal, store: &Store, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(goal)?);
        return Ok(());
    }
    println!(
        "{}  {:?}\n{}\nWorkspace: {}",
        goal.id,
        goal.status,
        goal.spec.objective,
        goal.workspace.display()
    );
    for (i, criterion) in goal.spec.acceptance.iter().enumerate() {
        println!("  AC{}: {criterion}", i + 1);
    }
    for task in &goal.tasks {
        let state = if task.manual_evidence.is_some() {
            "manual completion".into()
        } else {
            task.attempts.last().map_or(
                if task.spec.runner.is_none() {
                    "human task".into()
                } else {
                    "pending".into()
                },
                |a| format!("{:?}", a.status),
            )
        };
        println!(
            "  {}: {} — {} (depends on: {})",
            task.spec.id,
            state,
            task.spec.instruction,
            task.spec.depends_on.join(", ")
        );
        if let Some(evidence) = &task.manual_evidence {
            println!("    Evidence: {}", evidence.note);
        }
        print_attempts(&task.attempts);
    }
    if !goal.plans.is_empty() {
        println!("Planning:");
        print_attempts(&goal.plans);
    }
    if !goal.repairs.is_empty() {
        println!("Repairs:");
        print_attempts(&goal.repairs);
    }
    for (i, verification) in goal.verifications.iter().enumerate() {
        println!(
            "Verification {}: {} checks, finished={}",
            i + 1,
            verification.results.len(),
            verification.finished_at.is_some()
        );
        for check in &verification.results {
            println!(
                "  {}: {:?} [{}]\n    {}",
                check.name, check.attempt.status, check.attempt.id, check.detail
            );
        }
    }
    for guidance in &goal.guidance {
        println!("Guidance (next attempt): {}", guidance.note);
    }
    if let Some(evidence) = &goal.acceptance {
        println!("Accepted: {}", evidence.note);
    }
    if goal.control != Control::Run {
        println!("Requested: {:?}", goal.control);
    }
    if let Some(message) = &goal.message {
        println!("{message}");
    }
    println!("Logs: {}", store.runs_directory().display());
    Ok(())
}
fn print_attempts(attempts: &[Attempt]) {
    for attempt in attempts {
        println!(
            "    {}: {:?}{}",
            attempt.id,
            attempt.status,
            attempt
                .error
                .as_ref()
                .map(|e| format!(" — {e}"))
                .unwrap_or_default()
        );
    }
}

/// One row per goal with its next useful action. A completed or cancelled
/// goal waits for nobody, whatever wait reason it kept as history.
pub(crate) fn status_rows(goals: &[Goal]) -> Vec<serde_json::Value> {
    use crate::model::GoalStatus;
    goals
        .iter()
        .map(|goal| {
            let finished = matches!(goal.status, GoalStatus::Completed | GoalStatus::Cancelled);
            let waiting = goal.waiting.as_ref().filter(|_| !finished);
            let next = match goal.status {
                GoalStatus::Completed | GoalStatus::Cancelled => "none",
                GoalStatus::Running => "inspect_execution",
                GoalStatus::ReadyForReview => "review_and_accept",
                GoalStatus::Paused if goal.waiting.is_some() => "answer_wait",
                GoalStatus::Paused => "resolve_wait_then_resume",
                _ if goal.tasks.iter().enumerate().any(|(i, t)| {
                    t.spec.runner.is_none() && !t.succeeded() && goal.dependencies_done(i)
                }) =>
                {
                    "complete_human_work"
                }
                GoalStatus::Ready => "start_work",
                _ => "inspect_and_resume",
            };
            serde_json::json!({"id":goal.id,"objective":goal.spec.objective,"status":goal.status,
            "completed_tasks":goal.tasks.iter().filter(|t|t.succeeded()).count(),
            "total_tasks":goal.tasks.len(),"next_action":next,"message":goal.message,
            "waiting_for":waiting.map(|w| &w.note)})
        })
        .collect()
}
/// Rows as JSON, an empty-state sentence, or one rendered entry per row.
fn print_rows(
    rows: &[serde_json::Value],
    json_output: bool,
    empty: &str,
    entry: impl Fn(&serde_json::Value) -> String,
) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(rows)?);
    } else if rows.is_empty() {
        println!("{empty}");
    } else {
        for row in rows {
            println!("{}", entry(row));
        }
    }
    Ok(())
}
fn text(row: &serde_json::Value, key: &str) -> String {
    row[key].as_str().unwrap_or_default().to_string()
}
pub(crate) fn print_status(goals: &[Goal], json_output: bool) -> Result<()> {
    print_rows(
        &status_rows(goals),
        json_output,
        "No tracked goals. Start hgm and talk about what you want to move forward.",
        |row| {
            let waiting = row["waiting_for"]
                .as_str()
                .map(|reason| format!("\n  Waiting for you: {reason}"))
                .unwrap_or_default();
            format!(
                "{} — {}\n  Next: {} · {}/{} tasks · {}{waiting}",
                text(row, "objective"),
                text(row, "status"),
                text(row, "next_action"),
                row["completed_tasks"],
                row["total_tasks"],
                text(row, "id"),
            )
        },
    )
}

/// What needs a human now, most blocking first: an agent dialog or stopped
/// monitoring, then results to review, manager questions, and missing input.
/// Completed and cancelled goals need nobody: cancelling keeps the saved wait
/// reason and the last event as history, so neither may surface them here.
pub(crate) fn inbox_rows(store: &Store, goals: &[Goal]) -> Result<Vec<serde_json::Value>> {
    use crate::model::GoalStatus;
    let mut rows = vec![];
    for goal in goals {
        if matches!(goal.status, GoalStatus::Completed | GoalStatus::Cancelled) {
            continue;
        }
        let blocking = store.events(&goal.id)?.pop().filter(|event| {
            event.kind == "attention"
                || (event.kind == "watch_ended" && event.detail["notify"] == true)
        });
        let message = || goal.message.clone().unwrap_or_default();
        let waiting = goal.waiting.as_ref().map(|w| w.note.clone());
        let (kind, reason, since) = if let Some(event) = &blocking {
            let kind = if event.kind == "attention" {
                "agent_needs_you"
            } else {
                "monitoring_stopped"
            };
            (
                kind,
                event.detail["reason"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                event.at,
            )
        } else if goal.status == GoalStatus::ReadyForReview {
            (
                "review_result",
                waiting.unwrap_or_else(message),
                goal.updated_at,
            )
        } else if let Some(reason) = waiting {
            ("answer_manager", reason, goal.updated_at)
        } else if goal.status == GoalStatus::NeedsInput {
            ("provide_input", message(), goal.updated_at)
        } else {
            continue;
        };
        rows.push(serde_json::json!({
            "id": goal.id, "objective": goal.spec.objective,
            "kind": kind, "reason": reason, "since": since,
        }));
    }
    Ok(rows)
}
pub(crate) fn print_inbox(store: &Store, goals: &[Goal], json_output: bool) -> Result<()> {
    print_rows(
        &inbox_rows(store, goals)?,
        json_output,
        "Nothing needs you right now.",
        |row| {
            format!(
                "[{}] {} — {}\n  {}",
                text(row, "kind"),
                text(row, "objective"),
                text(row, "reason"),
                text(row, "id"),
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Evidence, GoalStatus};

    #[test]
    fn finished_goals_need_nobody_whatever_they_kept() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        let spec = || serde_norway::from_str("objective: Review\nacceptance: [Reviewed]\n");
        let mut ids = vec![];
        for status in [
            GoalStatus::Completed,
            GoalStatus::Cancelled,
            GoalStatus::Paused,
        ] {
            let id = store.add(spec()?, None)?.id;
            store.update(&id, |goal| {
                goal.status = status;
                goal.waiting = Some(Evidence {
                    note: "Ask the user".into(),
                    recorded_at: chrono::Utc::now(),
                });
                Ok(())
            })?;
            store.record(&id, "attention", serde_json::json!({"reason": "dialog"}))?;
            ids.push(id);
        }
        let rows = inbox_rows(&store, &store.list()?)?;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["id"], ids[2]);
        let waits: Vec<_> = status_rows(&store.list()?)
            .iter()
            .map(|row| row["waiting_for"].clone())
            .collect();
        assert_eq!(
            waits,
            [
                serde_json::Value::Null,
                serde_json::Value::Null,
                "Ask the user".into()
            ]
        );
        Ok(())
    }
}
