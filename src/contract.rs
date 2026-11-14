//! The human-designed loop: how far the manager may iterate before a person
//! decides. Pure types with no dependency on goal records or storage.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Whether the manager may send each assignment itself or must show it first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Autonomy {
    /// Assign, inspect and retry within budget; humans review the result.
    #[default]
    Auto,
    /// Every assignment needs the user's recorded approval.
    Ask,
}

/// When to stop iterating and escalate to a person.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LoopPolicy {
    /// Linked assignments per task (or per goal without a task) before escalating.
    /// A human revision starts a new budget for the reopened task.
    pub max_assignments: u32,
    /// Hours after the first assignment before monitoring escalates.
    pub max_hours: Option<u32>,
    /// Minutes between drift checks while the original agent works; 0 disables.
    pub patrol_minutes: u32,
    pub autonomy: Autonomy,
    /// Human-written abort conditions checked at every wake (e.g. "the fix
    /// requires changing packages/auth").
    pub stop_when: Vec<String>,
    /// Original-agent context use that triggers handoff and restart.
    pub handoff_percent: u8,
    /// Each task needs an approved plan before implementation assignments.
    pub plan_first: bool,
    /// Machine-checked abort conditions run before each wake; exit 0 stops.
    pub stop_checks: Vec<StopCheck>,
    /// Corrections per task (since its last reopen) before the user is asked.
    pub max_corrections: u32,
    /// Run the goal's command checks when the worker settles after new work,
    /// so a missed acceptance is corrected instead of waiting for review.
    pub check_on_settle: bool,
    /// Minutes of work between goal checks (the worker restates the goal so
    /// long work does not drift); 30..=60.
    pub reanchor_minutes: u32,
}

/// A user-authored shell command whose success means the loop must stop.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StopCheck {
    pub name: String,
    pub command: String,
    #[serde(default = "stop_check_timeout")]
    pub timeout_secs: u64,
}
fn stop_check_timeout() -> u64 {
    60
}
impl Default for LoopPolicy {
    fn default() -> Self {
        Self {
            max_assignments: 3,
            max_hours: None,
            // User decision (2026-11-13): check main every 5 minutes.
            patrol_minutes: 5,
            autonomy: Autonomy::Auto,
            stop_when: vec![],
            // User decision (2026-11-13): hand off at 85%, never compact.
            handoff_percent: 85,
            plan_first: false,
            stop_checks: vec![],
            max_corrections: 5,
            check_on_settle: true,
            // User decision (2026-11-13): re-read the goal every 30-60 minutes.
            reanchor_minutes: 45,
        }
    }
}
impl LoopPolicy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=100).contains(&self.max_assignments),
            "loop.max_assignments must be 1..=100"
        );
        ensure!(
            self.max_hours
                .is_none_or(|hours| (1..=720).contains(&hours)),
            "loop.max_hours must be 1..=720"
        );
        ensure!(
            self.patrol_minutes <= 1440,
            "loop.patrol_minutes must be 0..=1440"
        );
        ensure!(
            (30..=95).contains(&self.handoff_percent),
            "loop.handoff_percent must be 30..=95"
        );
        ensure!(
            self.stop_when.iter().all(|s| !s.trim().is_empty()),
            "loop.stop_when entries must not be empty"
        );
        ensure!(
            self.stop_checks.iter().all(|check| {
                !check.name.trim().is_empty()
                    && !check.command.trim().is_empty()
                    && check.timeout_secs > 0
            }),
            "loop.stop_checks need a name, a command, and a positive timeout"
        );
        ensure!(
            (1..=50).contains(&self.max_corrections),
            "loop.max_corrections must be 1..=50"
        );
        ensure!(
            (30..=60).contains(&self.reanchor_minutes),
            "loop.reanchor_minutes must be 30..=60"
        );
        Ok(())
    }
}

/// Content digest of workspace-relative protected paths, taken at confirmation
/// and compared while the agent works. A path created later also changes it.
pub(crate) fn protected_digest(workspace: &Path, paths: &[String]) -> Result<String> {
    let mut hasher = Sha256::new();
    for prefix in paths {
        let mut files = vec![];
        collect_files(&workspace.join(prefix), &mut files)?;
        files.sort();
        hasher.update(prefix.as_bytes());
        hasher.update([0]);
        for file in files {
            let relative = file.strip_prefix(workspace).unwrap_or(&file);
            hasher.update(relative.as_os_str().as_encoded_bytes());
            hasher.update([0]);
            hasher
                .update(std::fs::read(&file).with_context(|| format!("read {}", file.display()))?);
            hasher.update([0]);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            collect_files(&entry?.path(), files)?;
        }
    } else if metadata.is_file() {
        files.push(path.to_path_buf());
    }
    Ok(())
}
