//! Scheduling and acceptance remain independent of the terminal and executor.

use crate::{model::*, process, store::Store};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use std::{collections::HashSet, future::Future, time::Duration};

#[async_trait::async_trait]
pub(crate) trait Executor: Sync {
    async fn execute(
        &self,
        store: &Store,
        dir: &std::path::Path,
        id: &str,
        command: &CommandSpec,
        prompt: &str,
        timeout: u64,
    ) -> Result<String>;
}
pub(crate) struct CommandExecutor;
#[async_trait::async_trait]
impl Executor for CommandExecutor {
    async fn execute(
        &self,
        store: &Store,
        dir: &std::path::Path,
        id: &str,
        command: &CommandSpec,
        prompt: &str,
        timeout: u64,
    ) -> Result<String> {
        process::execute(store, dir, id, command, prompt, timeout).await
    }
}

pub(crate) struct Options {
    pub resume: bool,
    pub timeout_secs: u64,
    pub max_attempts: usize,
    pub max_repairs: usize,
}
pub(crate) async fn run(
    store: &Store,
    id: &str,
    executor: &impl Executor,
    options: Options,
) -> Result<Goal> {
    ensure!(
        options.timeout_secs > 0 && options.max_attempts > 0,
        "timeout and attempt limit must be positive"
    );
    let _lease = store.execution_lease()?;
    let goal = store.update(id, |goal| goal.recover(options.resume))?;
    let result = run_inner(store, goal, executor, &options).await;
    if let Err(error) = &result {
        finish(store, id, GoalStatus::NeedsInput, &format!("{error:#}"))?;
    }
    result
}

async fn run_inner(
    store: &Store,
    mut goal: Goal,
    executor: &impl Executor,
    options: &Options,
) -> Result<Goal> {
    if goal.tasks.is_empty() {
        ensure!(
            goal.plans.len() < options.max_attempts,
            "planning attempt limit reached"
        );
        let planner = goal
            .spec
            .planner
            .as_ref()
            .context("goal has no tasks or planner")?;
        let command = goal.spec.runners[planner].clone();
        let prompt = format!("{}\n{}", goal.spec.planning_prompt()?, goal.prompt()?);
        let attempt = Attempt::running();
        goal = store.update(&goal.id, |goal| {
            goal.plans.push(attempt.clone());
            Ok(())
        })?;
        let Some(result) = controlled(
            store,
            &goal.id,
            executor.execute(
                store,
                goal.work_dir(),
                &attempt.id,
                &command,
                &prompt,
                options.timeout_secs,
            ),
        )
        .await?
        else {
            return stop(store, &goal.id);
        };
        let parsed = result.and_then(|reply| goal.spec.parse_plan(&reply));
        goal = store.update(&goal.id, |goal| {
            goal.plans
                .last_mut()
                .context("missing planning attempt")?
                .finish(
                    &parsed
                        .as_ref()
                        .map(|_| String::new())
                        .map_err(|e| anyhow::anyhow!("{e:#}")),
                );
            if let Ok(tasks) = &parsed {
                goal.tasks = tasks.iter().cloned().map(Task::new).collect();
            }
            Ok(())
        })?;
        parsed?;
    }
    let mut tried = HashSet::new();
    loop {
        goal = store.load(&goal.id)?;
        if goal.control != Control::Run {
            return stop(store, &goal.id);
        }
        let Some(index) = goal.ready_task(&tried, options.max_attempts) else {
            break;
        };
        let task = &goal.tasks[index];
        tried.insert(task.spec.id.clone());
        let command = goal.spec.runners[task
            .spec
            .runner
            .as_ref()
            .context("automatic task needs runner")?]
        .clone();
        let prompt = format!(
            "{}\n# Assigned task: {}\n{}\nDependencies: {}. Preserve completed and partial work in this workspace. Prior attempt records:\n{}",
            goal.prompt()?,
            task.spec.id,
            task.spec.instruction,
            task.spec.depends_on.join(", "),
            serde_json::to_string_pretty(&task.attempts)?
        );
        let attempt = Attempt::running();
        eprintln!(
            "Goal {}: task {} (attempt {})",
            goal.id,
            task.spec.id,
            task.attempts.len() + 1
        );
        goal = store.update(&goal.id, |goal| {
            goal.tasks[index].attempts.push(attempt.clone());
            Ok(())
        })?;
        let Some(result) = controlled(
            store,
            &goal.id,
            executor.execute(
                store,
                goal.work_dir(),
                &attempt.id,
                &command,
                &prompt,
                options.timeout_secs,
            ),
        )
        .await?
        else {
            return stop(store, &goal.id);
        };
        store.update(&goal.id, |goal| {
            goal.tasks[index]
                .attempts
                .last_mut()
                .context("missing task attempt")?
                .finish(&result);
            Ok(())
        })?;
    }
    if goal.tasks.iter().any(|task| !task.succeeded()) {
        return finish(
            store,
            &goal.id,
            GoalStatus::NeedsInput,
            "Manual, failed, or dependency-blocked tasks remain. Inspect with show; complete manual work with evidence, or resume unfinished automatic work.",
        );
    }
    loop {
        let Some(passed) = verify(store, &goal.id, executor).await? else {
            return stop(store, &goal.id);
        };
        if passed {
            let message = if goal.spec.checks.is_empty() {
                "Tasks finished. No command checks are registered, so acceptance rests on the recorded evidence. Review it against the acceptance criteria, then accept with evidence or request a revision."
            } else {
                "Tasks finished and registered checks passed. Review the acceptance criteria, then accept with evidence or request a revision."
            };
            return finish(store, &goal.id, GoalStatus::ReadyForReview, message);
        }
        goal = store.load(&goal.id)?;
        let Some(repair) = &goal.spec.repair else {
            return finish(
                store,
                &goal.id,
                GoalStatus::NeedsInput,
                &failure_summary(
                    &goal,
                    "Acceptance checks failed. No repair runner is configured; inspect evidence, fix the issue, then resume.",
                ),
            );
        };
        if goal.repairs.len() >= options.max_repairs {
            return finish(
                store,
                &goal.id,
                GoalStatus::NeedsInput,
                &failure_summary(
                    &goal,
                    "Acceptance checks failed and the repair limit is exhausted.",
                ),
            );
        }
        let command = goal.spec.runners[repair].clone();
        let prompt = format!(
            "{}\n# Acceptance checks failed\n{}\nFix only goal-related failures. Preserve criteria and check commands. Every check will run again.",
            goal.prompt()?,
            serde_json::to_string_pretty(&goal.verifications.last())?
        );
        let attempt = Attempt::running();
        goal = store.update(&goal.id, |goal| {
            goal.repairs.push(attempt.clone());
            Ok(())
        })?;
        let Some(result) = controlled(
            store,
            &goal.id,
            executor.execute(
                store,
                goal.work_dir(),
                &attempt.id,
                &command,
                &prompt,
                options.timeout_secs,
            ),
        )
        .await?
        else {
            return stop(store, &goal.id);
        };
        store.update(&goal.id, |goal| {
            goal.repairs
                .last_mut()
                .context("missing repair attempt")?
                .finish(&result);
            Ok(())
        })?;
        result.context("acceptance repair failed")?;
    }
}

async fn verify(store: &Store, id: &str, executor: &impl Executor) -> Result<Option<bool>> {
    let goal = store.update(id, |goal| {
        goal.verifications.push(Verification {
            started_at: Utc::now(),
            finished_at: None,
            results: vec![],
        });
        Ok(())
    })?;
    for check in &goal.spec.checks {
        let command = CommandSpec {
            program: "sh".into(),
            args: vec!["-c".into(), check.command.clone()],
            prompt_stdin: false,
        };
        let mut attempt = Attempt::running();
        store.update(id, |goal| {
            goal.verifications
                .last_mut()
                .context("missing verification")?
                .results
                .push(CheckResult {
                    name: check.name.clone(),
                    attempt: attempt.clone(),
                    detail: String::new(),
                });
            Ok(())
        })?;
        let Some(result) = controlled(
            store,
            id,
            executor.execute(
                store,
                goal.work_dir(),
                &attempt.id,
                &command,
                "",
                check.timeout_secs,
            ),
        )
        .await?
        else {
            return Ok(None);
        };
        attempt.finish(&result);
        store.update(id, |goal| {
            let check = goal
                .verifications
                .last_mut()
                .and_then(|v| v.results.last_mut())
                .context("missing check result")?;
            check.attempt = attempt;
            check.detail = match result {
                Ok(output) => output.chars().take(8192).collect(),
                Err(error) => format!("{error:#}"),
            };
            Ok(())
        })?;
    }
    let goal = store.update(id, |goal| {
        goal.verifications
            .last_mut()
            .context("missing verification")?
            .finished_at = Some(Utc::now());
        Ok(())
    })?;
    Ok(Some(
        goal.verifications
            .last()
            .context("missing verification")?
            .results
            .iter()
            .all(|check| check.attempt.status == AttemptStatus::Succeeded),
    ))
}

/// Human acceptance rechecks the current workspace; an earlier pass cannot
/// accept a now-failing outcome. Manual-only goals still require explicit evidence.
pub(crate) async fn accept(
    store: &Store,
    id: &str,
    evidence: &str,
    executor: &impl Executor,
) -> Result<Goal> {
    ensure!(
        !evidence.trim().is_empty(),
        "acceptance evidence must not be empty"
    );
    let _lease = store.execution_lease()?;
    let goal = store.load(id)?;
    ensure!(
        goal.status == GoalStatus::ReadyForReview,
        "goal must be ready_for_review before acceptance"
    );
    store.update(id, |goal| {
        goal.status = GoalStatus::Running;
        Ok(())
    })?;
    let result = verify(store, id, executor).await;
    match result {
        Ok(Some(true)) => store.update(id, |goal| {
            if goal.control != Control::Run {
                apply_stop(goal);
                return Ok(());
            }
            ensure!(
                goal.tasks.iter().all(Task::succeeded),
                "tasks are no longer complete"
            );
            goal.acceptance = Some(Evidence {
                note: evidence.into(),
                recorded_at: Utc::now(),
            });
            goal.status = GoalStatus::Completed;
            goal.message = Some("Accepted with human evidence and current check results.".into());
            Ok(())
        }),
        Ok(None) => stop(store, id),
        Ok(Some(false)) => finish(
            store,
            id,
            GoalStatus::NeedsInput,
            "Checks failed during acceptance. The goal was not accepted.",
        ),
        Err(error) => {
            finish(store, id, GoalStatus::NeedsInput, &format!("{error:#}"))?;
            Err(error)
        }
    }
}

async fn controlled<T>(
    store: &Store,
    id: &str,
    future: impl Future<Output = T>,
) -> Result<Option<T>> {
    if store.load(id)?.control != Control::Run {
        return Ok(None);
    }
    tokio::pin!(future);
    let mut poll = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            result = &mut future => return Ok(Some(result)),
            _ = poll.tick() => if store.load(id)?.control != Control::Run { return Ok(None); },
        }
    }
}
fn apply_stop(goal: &mut Goal) {
    goal.interrupt_attempts();
    goal.status = if goal.control == Control::Cancel {
        GoalStatus::Cancelled
    } else {
        GoalStatus::Paused
    };
    goal.message = Some("Active work was interrupted; partial effects and evidence remain. Inspect them before resuming.".into());
}
fn stop(store: &Store, id: &str) -> Result<Goal> {
    store.update(id, |goal| {
        apply_stop(goal);
        Ok(())
    })
}
/// The summary of a failed verification. A check whose environment denied it
/// access (a sandbox, for example) produced no result about the work, so it is
/// named apart from checks that failed on their own, which `failure` describes.
fn failure_summary(goal: &Goal, failure: &str) -> String {
    const DENIED: &str = "Operation not permitted";
    let (denied, failed): (Vec<&CheckResult>, Vec<&CheckResult>) = goal
        .verifications
        .last()
        .map(|v| v.results.iter())
        .into_iter()
        .flatten()
        .filter(|check| check.attempt.status == AttemptStatus::Failed)
        .partition(|check| check.detail.contains(DENIED));
    if denied.is_empty() {
        return failure.into();
    }
    let names = |checks: &[&CheckResult]| {
        checks
            .iter()
            .map(|check| check.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut summary = format!(
        "The environment denied check {} access ({DENIED}): this is not a check result. Run the checks again where they can write, for example outside a sandbox, then resume.",
        names(&denied)
    );
    if !failed.is_empty() {
        summary.push_str(&format!(
            " Check {} failed on its own: {failure}",
            names(&failed)
        ));
    }
    summary
}
fn finish(store: &Store, id: &str, status: GoalStatus, message: &str) -> Result<Goal> {
    store.update(id, |goal| {
        if goal.control != Control::Run {
            apply_stop(goal);
        } else {
            goal.status = status;
            goal.message = Some(message.into());
        }
        Ok(())
    })
}
