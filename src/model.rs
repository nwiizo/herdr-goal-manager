//! Persisted goal contract and deterministic state transitions.

use crate::contract::{Autonomy, LoopPolicy};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Deliver the prompt on stdin; otherwise commands receive a closed stdin.
    #[serde(default)]
    pub prompt_stdin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Check {
    pub name: String,
    /// Explicit user-authored shell command, run in the goal workspace.
    pub command: String,
    #[serde(default = "default_check_timeout")]
    pub timeout_secs: u64,
}
fn default_check_timeout() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskSpec {
    pub id: String,
    pub instruction: String,
    /// None means a human completes this task with evidence.
    #[serde(default)]
    pub runner: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GoalSpec {
    pub objective: String,
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub out_of_scope: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub runners: BTreeMap<String, CommandSpec>,
    #[serde(default)]
    pub planner: Option<String>,
    #[serde(default)]
    pub repair: Option<String>,
    #[serde(default)]
    pub checks: Vec<Check>,
    #[serde(default)]
    pub tasks: Vec<TaskSpec>,
    /// What must not change however the work is done.
    #[serde(default)]
    pub invariants: Vec<String>,
    /// What to do on reaching an out-of-scope boundary instead of crossing it.
    #[serde(default)]
    pub on_boundary: Option<String>,
    /// Prior attempts, similar code, and references worth reading first.
    #[serde(default)]
    pub notes: Vec<String>,
    /// Known unknowns, each with how to reduce the uncertainty.
    #[serde(default)]
    pub open_questions: Vec<OpenQuestion>,
    /// Workspace-relative files or directories the work must not modify.
    /// A change detected while the agent works stops the loop.
    #[serde(default)]
    pub protected_paths: Vec<String>,
    /// Why this contract may exceed the word budget.
    #[serde(default)]
    pub long_spec_reason: Option<String>,
    /// How the manager iterates: assignment budget, time budget, patrol, autonomy.
    #[serde(default, rename = "loop")]
    pub loop_policy: LoopPolicy,
}

/// Words, counting two CJK characters as one word so Japanese text is budgeted too.
pub(crate) fn word_units(text: &str) -> usize {
    let cjk = text
        .chars()
        .filter(|c| matches!(*c as u32, 0x3040..=0x30FF | 0x3400..=0x9FFF | 0xF900..=0xFAFF))
        .count();
    text.split_whitespace().count() + cjk / 2
}

/// An unresolved question stated with its resolution path, so agents do not
/// silently decide it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenQuestion {
    pub question: String,
    /// How to find out (read code, ask the user, measure).
    pub resolve_by: String,
    /// Behavior to use until it is resolved.
    #[serde(default)]
    pub default: Option<String>,
}

/// Spec length above which agents follow rules less reliably.
pub(crate) const SPEC_WORD_BUDGET: usize = 800;

impl GoalSpec {
    pub fn validate(&self) -> Result<()> {
        self.loop_policy.validate()?;
        let words = self.words();
        ensure!(
            words <= SPEC_WORD_BUDGET
                || self
                    .long_spec_reason
                    .as_ref()
                    .is_some_and(|r| !r.trim().is_empty()),
            "the contract is about {words} words (budget {SPEC_WORD_BUDGET}); long specs are followed less reliably. Link background material, split the goal, or state long_spec_reason"
        );
        ensure!(
            self.protected_paths.iter().all(|p| {
                let path = std::path::Path::new(p);
                !p.trim().is_empty()
                    && path.is_relative()
                    && path
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_)))
            }),
            "protected_paths must be workspace-relative paths without '..'"
        );
        ensure!(
            self.invariants.iter().all(|s| !s.trim().is_empty())
                && self
                    .open_questions
                    .iter()
                    .all(|q| { !q.question.trim().is_empty() && !q.resolve_by.trim().is_empty() }),
            "stop conditions, invariants, and open questions must not be empty"
        );
        ensure!(
            !self.objective.trim().is_empty(),
            "objective must not be empty"
        );
        ensure!(
            !self.acceptance.is_empty() && self.acceptance.iter().all(|s| !s.trim().is_empty()),
            "at least one nonempty acceptance criterion is required"
        );
        for (name, command) in &self.runners {
            validate_id(name)?;
            ensure!(
                !command.program.trim().is_empty(),
                "runner {name} needs a program"
            );
        }
        for name in self.planner.iter().chain(self.repair.iter()) {
            ensure!(self.runners.contains_key(name), "unknown runner: {name}");
        }
        let mut names = HashSet::new();
        for check in &self.checks {
            validate_id(&check.name)?;
            ensure!(names.insert(&check.name), "duplicate check: {}", check.name);
            ensure!(
                !check.command.trim().is_empty() && check.timeout_secs > 0,
                "check {} needs a command and positive timeout",
                check.name
            );
        }
        self.validate_tasks(&self.tasks)
    }

    pub fn validate_tasks(&self, tasks: &[TaskSpec]) -> Result<()> {
        ensure!(tasks.len() <= 32, "at most 32 tasks are supported per goal");
        let mut ids = HashSet::new();
        for task in tasks {
            validate_id(&task.id)?;
            ensure!(
                ids.insert(task.id.as_str()),
                "duplicate task ID: {}",
                task.id
            );
            ensure!(
                !task.instruction.trim().is_empty(),
                "task {} has no instruction",
                task.id
            );
            if let Some(runner) = &task.runner {
                ensure!(
                    self.runners.contains_key(runner),
                    "task {} uses unknown runner {runner}",
                    task.id
                );
            }
        }
        for task in tasks {
            let mut dependencies = HashSet::new();
            for dependency in &task.depends_on {
                ensure!(
                    ids.contains(dependency.as_str()),
                    "task {} has unknown dependency {dependency}",
                    task.id
                );
                ensure!(
                    dependencies.insert(dependency),
                    "task {} repeats dependency {dependency}",
                    task.id
                );
            }
        }
        let mut resolved = HashSet::new();
        loop {
            let before = resolved.len();
            for task in tasks {
                if task
                    .depends_on
                    .iter()
                    .all(|id| resolved.contains(id.as_str()))
                {
                    resolved.insert(task.id.as_str());
                }
            }
            if resolved.len() == tasks.len() {
                return Ok(());
            }
            ensure!(
                resolved.len() > before,
                "task dependency graph contains a cycle"
            );
        }
    }

    pub fn context(&self) -> Result<String> {
        Ok(format!(
            "# Goal contract\n{}\n\nPreserve existing work. Work only toward this goal. Report unresolved criteria. Do not commit, push, publish, merge, or perform unrelated actions unless the goal explicitly calls for them.\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "objective": self.objective, "acceptance": self.acceptance,
                "invariants": self.invariants,
                "out_of_scope": self.out_of_scope, "on_boundary": self.boundary_rule(),
                "constraints": self.constraints, "notes": self.notes,
                "open_questions": self.open_questions,
                "checks": self.checks, "available_runners": self.runners.keys().collect::<Vec<_>>(),
                "loop": self.loop_policy,
            }))?
        ))
    }

    /// What an agent does at an out-of-scope boundary; defaults to stopping.
    pub fn boundary_rule(&self) -> &str {
        self.on_boundary
            .as_deref()
            .unwrap_or("Stop and report the boundary and why it seems necessary; do not cross it.")
    }
    /// Approximate length of the human-written contract in words.
    pub fn words(&self) -> usize {
        let questions = self.open_questions.iter().flat_map(|q| {
            [Some(&q.question), Some(&q.resolve_by), q.default.as_ref()]
                .into_iter()
                .flatten()
        });
        std::iter::once(&self.objective)
            .chain(&self.acceptance)
            .chain(&self.invariants)
            .chain(&self.out_of_scope)
            .chain(&self.constraints)
            .chain(&self.notes)
            .chain(&self.loop_policy.stop_when)
            .chain(self.on_boundary.as_ref())
            .chain(questions)
            .chain(self.tasks.iter().map(|t| &t.instruction))
            .map(|text| word_units(text))
            .sum()
    }
    /// The contract every linked assignment carries, so the original agent
    /// works from the agreed spec whatever the manager's wording.
    pub fn assignment_contract(&self) -> String {
        let section = |title: &str, items: &[String]| {
            if items.is_empty() {
                String::new()
            } else {
                format!(
                    "{title}:\n{}\n",
                    items
                        .iter()
                        .map(|i| format!("- {i}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            }
        };
        let questions: Vec<String> = self
            .open_questions
            .iter()
            .map(|q| {
                format!(
                    "{} (resolve by: {}{})",
                    q.question,
                    q.resolve_by,
                    q.default
                        .as_ref()
                        .map(|d| format!("; until then: {d}"))
                        .unwrap_or_default()
                )
            })
            .collect();
        let protected: Vec<String> = self
            .protected_paths
            .iter()
            .map(|p| format!("{p} (protected)"))
            .collect();
        format!(
            "--- Goal contract (from hgm; binding) ---\nGoal: {}\n{}{}{}{}At a boundary: {}\n{}Report: what changed, how each acceptance item was verified, and anything unresolved. Do not claim completion without evidence.",
            self.objective,
            section("Acceptance", &self.acceptance),
            section("Invariants", &self.invariants),
            section(
                "Out of scope",
                &[self.out_of_scope.clone(), protected].concat()
            ),
            section("Constraints", &self.constraints),
            self.boundary_rule(),
            section("Open questions", &questions),
        )
    }

    pub fn planning_prompt(&self) -> Result<String> {
        Ok(format!(
            "{}\nInspect the workspace and return ONLY a JSON array of 1 to 32 bounded tasks. Each task has id, instruction, runner (a listed runner name, or null for human work), and depends_on (task IDs). Do not execute tasks, add commands, change the goal contract, or invent runners. IDs use ASCII letters/digits/dashes/underscores. Tasks run sequentially; specify real dependencies. Example: [{{\"id\":\"review\",\"instruction\":\"Review the outcome\",\"runner\":null,\"depends_on\":[]}}].",
            self.context()?
        ))
    }

    pub fn parse_plan(&self, reply: &str) -> Result<Vec<TaskSpec>> {
        let text = reply.trim();
        let text = if let Some(fenced) = text
            .strip_prefix("\x60\x60\x60json")
            .or_else(|| text.strip_prefix("\x60\x60\x60"))
        {
            fenced
                .trim()
                .strip_suffix("\x60\x60\x60")
                .context("unclosed plan code fence")?
                .trim()
        } else {
            text
        };
        let tasks: Vec<TaskSpec> =
            serde_json::from_str(text).context("planner must return only a JSON task array")?;
        ensure!(!tasks.is_empty(), "planner returned no tasks");
        self.validate_tasks(&tasks)?;
        Ok(tasks)
    }
}

pub(crate) fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 100
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "invalid ID: {id}"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoalStatus {
    Ready,
    Running,
    Paused,
    NeedsInput,
    ReadyForReview,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Control {
    Run,
    Pause,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttemptStatus {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Attempt {
    pub id: String,
    pub status: AttemptStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}
impl Attempt {
    pub fn running() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            status: AttemptStatus::Running,
            started_at: Utc::now(),
            finished_at: None,
            error: None,
        }
    }
    pub fn finish(&mut self, result: &Result<String>) {
        self.status = if result.is_ok() {
            AttemptStatus::Succeeded
        } else {
            AttemptStatus::Failed
        };
        self.finished_at = Some(Utc::now());
        self.error = result.as_ref().err().map(|e| format!("{e:#}"));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Evidence {
    pub note: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Task {
    pub spec: TaskSpec,
    pub attempts: Vec<Attempt>,
    pub manual_evidence: Option<Evidence>,
    /// Evidence from completions that a human revision later reopened.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub superseded_evidence: Vec<Evidence>,
}
impl Task {
    pub fn new(spec: TaskSpec) -> Self {
        Self {
            spec,
            attempts: vec![],
            manual_evidence: None,
            superseded_evidence: vec![],
        }
    }
    pub fn succeeded(&self) -> bool {
        self.manual_evidence.is_some()
            || self
                .attempts
                .last()
                .is_some_and(|a| a.status == AttemptStatus::Succeeded)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckResult {
    pub name: String,
    pub attempt: Attempt,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Verification {
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub results: Vec<CheckResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Goal {
    pub version: u32,
    pub id: String,
    pub workspace: PathBuf,
    pub spec: GoalSpec,
    pub status: GoalStatus,
    pub control: Control,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub plans: Vec<Attempt>,
    pub tasks: Vec<Task>,
    pub verifications: Vec<Verification>,
    pub repairs: Vec<Attempt>,
    pub guidance: Vec<Evidence>,
    pub acceptance: Option<Evidence>,
    pub message: Option<String>,
    /// Original Herdr terminal of the manager that registered the goal.
    #[serde(default)]
    pub owner: Option<String>,
    /// Why automation waits for a human; separate from user guidance.
    #[serde(default)]
    pub waiting: Option<Evidence>,
    /// The user's words agreeing to this loop contract; agent assignments need it.
    #[serde(default)]
    pub confirmed: Option<Evidence>,
    /// Digest of `spec.protected_paths` when the contract was confirmed.
    #[serde(default)]
    pub protected_digest: Option<String>,
}
impl Goal {
    pub fn new(mut spec: GoalSpec, workspace: PathBuf) -> Result<Self> {
        spec.validate()?;
        if spec.tasks.is_empty() && spec.planner.is_none() {
            spec.tasks.push(TaskSpec {
                id: "work".into(),
                instruction: spec.objective.clone(),
                runner: None,
                depends_on: vec![],
            });
        }
        let tasks = spec.tasks.iter().cloned().map(Task::new).collect();
        Ok(Self {
            version: 1,
            id: format!("g-{}", uuid::Uuid::new_v4()),
            workspace,
            spec,
            status: GoalStatus::Ready,
            control: Control::Run,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            plans: vec![],
            tasks,
            verifications: vec![],
            repairs: vec![],
            guidance: vec![],
            acceptance: None,
            message: None,
            owner: None,
            waiting: None,
            confirmed: None,
            protected_digest: None,
        })
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1,
            "unsupported goal version {}",
            self.version
        );
        validate_id(&self.id)?;
        self.spec.validate()?;
        self.spec.validate_tasks(
            &self
                .tasks
                .iter()
                .map(|t| t.spec.clone())
                .collect::<Vec<_>>(),
        )
    }
    pub fn request(&mut self, control: Control) -> Result<()> {
        ensure!(
            !matches!(self.status, GoalStatus::Completed | GoalStatus::Cancelled),
            "goal is terminal: {:?}",
            self.status
        );
        self.control = control;
        if control == Control::Run {
            self.waiting = None;
        }
        // Control steers automation. Review and input states describe the outcome,
        // so pausing or resuming wakeups must not hide a result awaiting a human.
        self.status = match (control, self.status) {
            (_, GoalStatus::Running) => GoalStatus::Running,
            (Control::Cancel, _) => GoalStatus::Cancelled,
            (_, GoalStatus::ReadyForReview) => GoalStatus::ReadyForReview,
            (Control::Pause, GoalStatus::NeedsInput) => GoalStatus::NeedsInput,
            (Control::Pause, _) => GoalStatus::Paused,
            (Control::Run, _) => GoalStatus::Ready,
        };
        Ok(())
    }
    /// Records the user's agreement to the loop contract (outcome, checks, budget)
    /// and the protected-path baseline. Autonomy may not exceed what a command
    /// check can verify: `auto` without one must be registered as `ask` instead.
    pub fn confirm(&mut self, evidence: String, protected_digest: String) -> Result<()> {
        ensure!(
            !evidence.trim().is_empty(),
            "confirmation must quote the user's agreement"
        );
        ensure!(
            !matches!(self.status, GoalStatus::Completed | GoalStatus::Cancelled),
            "goal is terminal"
        );
        ensure!(
            self.spec.loop_policy.autonomy == Autonomy::Ask || !self.spec.checks.is_empty(),
            "autonomy auto needs at least one command check: autonomy must not exceed what can be verified. Register the goal again with a check, or with autonomy ask"
        );
        self.confirmed = Some(Evidence {
            note: evidence,
            recorded_at: Utc::now(),
        });
        self.protected_digest = Some(protected_digest);
        Ok(())
    }
    /// Pauses automatic wakeups for a human decision without changing the outcome state.
    pub fn wait(&mut self, reason: String) -> Result<()> {
        ensure!(
            !reason.trim().is_empty(),
            "waiting reason must not be empty"
        );
        self.request(Control::Pause)?;
        self.waiting = Some(Evidence {
            note: reason,
            recorded_at: Utc::now(),
        });
        Ok(())
    }
    /// Reopens completed manual tasks after a human rejects the result. Earlier
    /// evidence moves to history and the feedback guides the next assignment.
    pub fn revise(&mut self, feedback: String, tasks: &[String]) -> Result<Vec<String>> {
        ensure!(
            !feedback.trim().is_empty(),
            "revision feedback must not be empty"
        );
        ensure!(
            !matches!(self.status, GoalStatus::Completed | GoalStatus::Cancelled),
            "terminal goals cannot be revised; register a new goal"
        );
        ensure!(
            self.status != GoalStatus::Running,
            "pause active execution before requesting a revision"
        );
        let selected: Vec<usize> = if tasks.is_empty() {
            (0..self.tasks.len())
                .filter(|&i| self.tasks[i].spec.runner.is_none() && self.tasks[i].succeeded())
                .collect()
        } else {
            tasks
                .iter()
                .map(|id| {
                    let index = self
                        .tasks
                        .iter()
                        .position(|t| &t.spec.id == id)
                        .with_context(|| format!("unknown task ID {id}"))?;
                    let task = &self.tasks[index];
                    ensure!(
                        task.spec.runner.is_none(),
                        "task {id} runs a configured command; add guidance and resume instead"
                    );
                    ensure!(task.succeeded(), "task {id} is not complete");
                    Ok(index)
                })
                .collect::<Result<_>>()?
        };
        ensure!(!selected.is_empty(), "no completed manual task to reopen");
        let now = Utc::now();
        let mut reopened = vec![];
        for index in selected {
            let task = &mut self.tasks[index];
            if let Some(evidence) = task.manual_evidence.take() {
                task.superseded_evidence.push(evidence);
            }
            reopened.push(task.spec.id.clone());
        }
        self.guidance.push(Evidence {
            note: format!("Revision requested: {feedback}"),
            recorded_at: now,
        });
        self.control = Control::Run;
        self.waiting = None;
        self.acceptance = None;
        self.status = GoalStatus::Ready;
        self.message = Some(format!(
            "Revision requested for {}; reassign the reopened work with the feedback.",
            reopened.join(", ")
        ));
        Ok(reopened)
    }
    pub fn note(&mut self, text: String) -> Result<()> {
        ensure!(!text.trim().is_empty(), "guidance must not be empty");
        ensure!(
            !matches!(self.status, GoalStatus::Completed | GoalStatus::Cancelled),
            "terminal goals cannot receive guidance"
        );
        self.guidance.push(Evidence {
            note: text,
            recorded_at: Utc::now(),
        });
        Ok(())
    }
    pub fn complete_manual(&mut self, id: &str, evidence: String) -> Result<()> {
        ensure!(
            !evidence.trim().is_empty(),
            "manual completion requires evidence"
        );
        ensure!(
            !matches!(self.status, GoalStatus::Completed | GoalStatus::Cancelled),
            "goal is terminal"
        );
        let index = self
            .tasks
            .iter()
            .position(|t| t.spec.id == id)
            .context("unknown task ID")?;
        ensure!(
            self.tasks[index].spec.runner.is_none(),
            "automatic tasks must succeed through their configured runner"
        );
        ensure!(
            self.dependencies_done(index),
            "task dependencies are unfinished"
        );
        ensure!(!self.tasks[index].succeeded(), "task is already complete");
        self.tasks[index].manual_evidence = Some(Evidence {
            note: evidence,
            recorded_at: Utc::now(),
        });
        if self.status == GoalStatus::NeedsInput {
            self.status = GoalStatus::Ready;
            self.message = None;
        }
        Ok(())
    }
    pub fn dependencies_done(&self, index: usize) -> bool {
        self.tasks[index]
            .spec
            .depends_on
            .iter()
            .all(|id| self.tasks.iter().any(|t| &t.spec.id == id && t.succeeded()))
    }
    pub fn ready_task(&self, tried: &HashSet<String>, max_attempts: usize) -> Option<usize> {
        self.tasks
            .iter()
            .enumerate()
            .find(|(i, task)| {
                task.spec.runner.is_some()
                    && !task.succeeded()
                    && !tried.contains(&task.spec.id)
                    && task.attempts.len() < max_attempts
                    && self.dependencies_done(*i)
            })
            .map(|(i, _)| i)
    }
    pub fn interrupt_attempts(&mut self) {
        for attempt in self
            .plans
            .iter_mut()
            .chain(self.repairs.iter_mut())
            .chain(self.tasks.iter_mut().flat_map(|t| t.attempts.iter_mut()))
            .chain(
                self.verifications
                    .iter_mut()
                    .flat_map(|v| v.results.iter_mut().map(|r| &mut r.attempt)),
            )
        {
            if attempt.status == AttemptStatus::Running {
                attempt.status = AttemptStatus::Interrupted;
                attempt.finished_at = Some(Utc::now());
                attempt.error = Some("Execution ended before recording its result. Inspect partial effects before resuming.".into());
            }
        }
    }
    pub fn recover(&mut self, resume: bool) -> Result<()> {
        ensure!(
            !matches!(self.status, GoalStatus::Completed | GoalStatus::Cancelled),
            "terminal goals cannot resume"
        );
        ensure!(
            resume || self.status == GoalStatus::Ready,
            "goal is {:?}; inspect it, then use resume",
            self.status
        );
        self.interrupt_attempts();
        self.control = Control::Run;
        self.status = GoalStatus::Running;
        self.message = None;
        self.acceptance = None;
        self.waiting = None;
        Ok(())
    }
    pub fn prompt(&self) -> Result<String> {
        Ok(format!(
            "{}\n# User guidance (applies from the next attempt)\n{}",
            self.spec.context()?,
            serde_json::to_string_pretty(&self.guidance)?
        ))
    }
}
