//! Another provider's check of the watched worker's work (`loop.cross_check`).
//! When the worker has settled after new work, the watch starts a one-shot,
//! read-only run of the other provider's CLI (Codex for a Claude Code worker,
//! Claude Code for the rest, or the one the contract names) on the goal's
//! acceptance, the task and the work dir's changes. The watch does not wait
//! for it: it reads the result at a later observation, records it and wakes
//! the manager. The verdict is an observation only: it completes and accepts
//! nothing.
use crate::{contract::CrossCheck, store::Store, watch::Watch};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::os::unix::process::CommandExt;

/// How long a check may run, collecting the changes included, before the
/// watch stops it.
const TIMEOUT_SECS: i64 = 900;
/// Findings a result keeps on the timeline and in the wake.
const MAX_FINDINGS: usize = 10;
/// The most of the assignment text the prompt quotes, in characters.
const MAX_ASSIGNMENT: usize = 2_000;

/// The shell around the reviewer's CLI. It collects the work dir's changes
/// itself, so a slow or misconfigured Git (an external diff or textconv
/// helper, an fsmonitor) runs under the check's timeout and process group, not
/// inside the watch's step: helpers are switched off and the output is cut
/// (60 KB of diff, 20 KB of untracked names). The exit status written is the
/// reviewer's.
const SHELL: &str = r#"{
  cat "$HGM_CROSS_IN"
  printf '\n'
  if git -C "$HGM_CROSS_DIR" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    printf 'git diff HEAD:\n'
    git -c core.fsmonitor=false -C "$HGM_CROSS_DIR" diff --no-ext-diff --no-textconv HEAD | head -c 60000
    printf '\nUntracked files:\n'
    git -c core.fsmonitor=false -C "$HGM_CROSS_DIR" ls-files --others --exclude-standard | head -c 20000
  else
    printf 'The work dir is not a Git repository: read the files it holds.\n'
  fi
} | "$@" > "$HGM_CROSS_OUT" 2> "$HGM_CROSS_ERR"
echo $? > "$HGM_CROSS_EXIT""#;

/// A check the watch started and has not read back yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CrossRun {
    pub id: String,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// `codex` or `claude`.
    pub reviewer: String,
    pub started_at: i64,
    /// The shell, which leads the run's process group.
    pub pid: u32,
}

/// The provider that checks a worker running `agent` under `mode`, if any.
pub(crate) fn reviewer(mode: CrossCheck, agent: &str) -> Option<&'static str> {
    match mode {
        CrossCheck::Off => None,
        CrossCheck::Auto if agent == "claude" => Some("codex"),
        CrossCheck::Auto | CrossCheck::Claude => Some("claude"),
        CrossCheck::Codex => Some("codex"),
    }
}

/// `text` as one line of plain characters, at most `max` of them: the
/// reviewer's output is model output, and control characters (escape
/// sequences, carriage returns) must not reach the manager's terminal.
fn clean(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Stops a check's process group: the shell leads its own group, so TERM
/// reaches the CLI and Git it started, not only the shell around them.
fn stop_group(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .output();
}

/// What the reviewer reads before the changes the shell appends: the
/// contract fields that say what done means and what this task covers.
fn prompt(
    goal: &crate::model::Goal,
    task: Option<&str>,
    assignment: Option<&str>,
    work_dir: &std::path::Path,
) -> String {
    let spec = &goal.spec;
    let list = |items: &[String]| {
        if items.is_empty() {
            "- (none)".to_string()
        } else {
            items
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    let task_spec = task.and_then(|id| spec.tasks.iter().find(|t| t.id == id));
    let assignment: String = assignment
        .unwrap_or("(none)")
        .chars()
        .take(MAX_ASSIGNMENT)
        .collect();
    format!(
        "You are checking another agent's work for hgm goal {id} task {task}, read-only: do not modify any file or run anything that writes.\n\
         Work dir: {dir}\nObjective: {objective}\nAcceptance:\n{acceptance}\nOut of scope:\n{scope}\n\
         Task instruction: {instruction}\nThe task may write only under:\n{paths}\n\
         Latest assignment for it:\n{assignment}\n\n\
         Check the changes against the acceptance and this task: correctness bugs, acceptance items missed or only claimed, and changes outside the task or its write paths.\n\
         Answer with a first line exactly `VERDICT: approve` or `VERDICT: changes_requested`, then at most {max} findings, one per line, each starting with `- `.\n",
        id = goal.id,
        task = task.unwrap_or("(goal)"),
        dir = work_dir.display(),
        objective = spec.objective,
        acceptance = list(&spec.acceptance),
        scope = list(&spec.out_of_scope),
        instruction = task_spec.map_or("(none)", |t| t.instruction.as_str()),
        paths = list(task_spec.map_or(&[][..], |t| &t.write_paths[..])),
        max = MAX_FINDINGS,
    )
}

/// Starts the check for the latest linked work once the worker has settled
/// after it: at most one check per assignment or correction, one at a time.
/// Work that settles while a check runs is checked after it, at a later
/// observation, because only a started check counts it as covered.
pub(crate) fn request(
    store: &Store,
    watch: &mut Watch,
    goal: &crate::model::Goal,
    events: &[crate::events::Event],
    settled: bool,
) -> Result<()> {
    let agent = watch.origin.agent.as_deref().unwrap_or("");
    let Some(reviewer) = reviewer(goal.spec.loop_policy.cross_check, agent) else {
        return Ok(());
    };
    let Some(work) = events
        .iter()
        .rev()
        .find(|e| matches!(e.kind.as_str(), "assigned" | "correction"))
    else {
        return Ok(());
    };
    let covered = watch.cross_through.is_some_and(|done| done >= work.at);
    if covered {
        watch.cross_due = false;
    }
    // Due once a settle after a change was seen (the watch's settle
    // detection, which also runs the settle checks); kept while a check runs.
    if !settled
        || !watch.cross_due
        || covered
        || watch.held.is_some()
        || watch.cross_check.is_some()
    {
        return Ok(());
    }
    let task = work.detail["task"].as_str();
    let assignment = events
        .iter()
        .rev()
        .find(|e| e.kind == "assigned" && e.detail["task"].as_str() == task)
        .and_then(|e| e.detail["text"].as_str());
    start(store, watch, goal, task, assignment, reviewer)?;
    watch.cross_through = Some(work.at);
    watch.cross_due = false;
    Ok(())
}

fn start(
    store: &Store,
    watch: &mut Watch,
    goal: &crate::model::Goal,
    task: Option<&str>,
    assignment: Option<&str>,
    reviewer: &str,
) -> Result<()> {
    let work_dir = goal.work_dir()?.to_path_buf();
    let id = format!("cross-{}", uuid::Uuid::new_v4());
    let dir = store.run_directory(&id)?;
    std::fs::write(
        dir.join("prompt.txt"),
        prompt(goal, task, assignment, &work_dir),
    )
    .context("write the cross check prompt")?;
    let result = dir.join("result.txt");
    let mut args: Vec<String> = match reviewer {
        "codex" => vec![
            "exec".into(),
            "--ephemeral".into(),
            "--sandbox".into(),
            "read-only".into(),
            "--skip-git-repo-check".into(),
            "-C".into(),
            work_dir.display().to_string(),
            "-o".into(),
            result.display().to_string(),
            "-".into(),
        ],
        // Read, Grep and Glob only, and no hooks: a user's hook could write.
        _ => vec![
            "-p".into(),
            "--permission-mode".into(),
            "dontAsk".into(),
            "--tools".into(),
            "Read,Grep,Glob".into(),
            "--settings".into(),
            r#"{"disableAllHooks":true}"#.into(),
            "--no-session-persistence".into(),
            "--disable-slash-commands".into(),
        ],
    };
    args.insert(0, reviewer.into());
    // Claude Code answers on stdout; Codex writes its last message to -o.
    let stdout = if reviewer == "codex" {
        dir.join("log.txt")
    } else {
        result.clone()
    };
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(SHELL)
        .arg("sh")
        .args(&args)
        .current_dir(&work_dir)
        .env("HGM_CROSS_IN", dir.join("prompt.txt"))
        .env("HGM_CROSS_DIR", &work_dir)
        .env("HGM_CROSS_OUT", &stdout)
        .env("HGM_CROSS_ERR", dir.join("stderr.txt"))
        .env("HGM_CROSS_EXIT", dir.join("exit"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .with_context(|| format!("start the {reviewer} check"))?;
    store.record(
        &goal.id,
        "cross_check_started",
        json!({"reviewer": reviewer, "task": task, "run": id}),
    )?;
    watch.cross_check = Some(CrossRun {
        id,
        goal: goal.id.clone(),
        task: task.map(String::from),
        reviewer: reviewer.into(),
        started_at: Utc::now().timestamp(),
        pid: child.id(),
    });
    // Reaped when it ends, so a long-running watch leaves no zombie behind.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Reads a finished check back: records its verdict and findings and returns
/// the note for the next wake. One still running past [`TIMEOUT_SECS`] is
/// stopped and recorded as timed out; with `ended` (the watch has ended) a
/// running one is stopped at once.
pub(crate) fn poll(
    store: &Store,
    watch: &mut Watch,
    now: i64,
    ended: bool,
) -> Result<Option<String>> {
    let Some(run) = watch.cross_check.clone() else {
        return Ok(None);
    };
    let dir = store.run_path(&run.id)?;
    let exit = std::fs::read_to_string(dir.join("exit")).ok();
    let (verdict, findings, status) = match exit {
        Some(status) => {
            let status = status.trim().to_string();
            let text = std::fs::read_to_string(dir.join("result.txt")).unwrap_or_default();
            let named = text
                .lines()
                .find_map(|line| line.trim().strip_prefix("VERDICT:"))
                .map(|v| v.trim().to_string())
                .filter(|v| v == "approve" || v == "changes_requested");
            // A reviewer that failed has no verdict, whatever it printed.
            let verdict = match named {
                _ if status != "0" => "failed".to_string(),
                Some(verdict) => verdict,
                None => "no_verdict".into(),
            };
            let findings: Vec<String> = text
                .lines()
                .map(str::trim)
                .filter(|line| line.starts_with("- "))
                .take(MAX_FINDINGS)
                .map(|line| clean(line, 200))
                .collect();
            (verdict, findings, status)
        }
        None if ended || now - run.started_at >= TIMEOUT_SECS => {
            stop_group(run.pid);
            let why = if ended { "stopped" } else { "timeout" };
            (why.to_string(), vec![], String::new())
        }
        None => return Ok(None),
    };
    store.record(
        &run.goal,
        "cross_checked",
        json!({"reviewer": run.reviewer, "task": run.task, "verdict": verdict,
               "findings": findings, "exit": status, "run": run.id}),
    )?;
    watch.cross_check = None;
    // Model output, quoted as data: never raw text in the manager's input.
    let detail = if findings.is_empty() {
        String::new()
    } else {
        format!(": {}", serde_json::to_string(&findings).unwrap_or_default())
    };
    Ok(Some(format!(
        "the {} check of {}'s work for goal {} task {} says {verdict}{detail}; it is an observation, not evidence: weigh it against the contract",
        run.reviewer,
        watch.role,
        run.goal,
        run.task.as_deref().unwrap_or("(goal)")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timed_out_check_stops_the_cli_the_shell_started() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        let mut shell = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30 & echo $! > \"$1\"; wait")
            .arg("sh")
            .arg(&child)
            .process_group(0)
            .spawn()
            .unwrap();
        let alive = |pid: &str| {
            std::process::Command::new("kill")
                .args(["-0", pid])
                .status()
                .unwrap()
                .success()
        };
        let mut sleeper = String::new();
        for _ in 0..50 {
            sleeper = std::fs::read_to_string(&child)
                .unwrap_or_default()
                .trim()
                .to_string();
            if !sleeper.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(alive(&sleeper));
        stop_group(shell.id());
        let _ = shell.wait();
        let mut gone = false;
        for _ in 0..50 {
            if !alive(&sleeper) {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(gone, "the CLI under the shell was stopped too");
    }

    #[test]
    fn reviewer_output_reaches_the_manager_as_plain_text() {
        // An escape sequence and a carriage return in a finding are dropped.
        assert_eq!(
            clean("- \u{1b}[2Jcleared\rscreen", 200),
            "- [2Jclearedscreen"
        );
        assert_eq!(clean("abcdef", 3), "abc");
    }

    #[test]
    fn the_other_provider_checks_unless_the_contract_names_one() {
        assert_eq!(reviewer(CrossCheck::Off, "claude"), None);
        assert_eq!(reviewer(CrossCheck::Auto, "claude"), Some("codex"));
        assert_eq!(reviewer(CrossCheck::Auto, "codex"), Some("claude"));
        assert_eq!(reviewer(CrossCheck::Codex, "codex"), Some("codex"));
        assert_eq!(reviewer(CrossCheck::Claude, "claude"), Some("claude"));
        let read = |text: &str| -> Result<CrossCheck, _> { serde_json::from_str(text) };
        assert_eq!(read("false").unwrap(), CrossCheck::Off);
        assert_eq!(read("true").unwrap(), CrossCheck::Auto);
        assert_eq!(read("\"auto\"").unwrap(), CrossCheck::Auto);
        assert_eq!(read("\"codex\"").unwrap(), CrossCheck::Codex);
        assert_eq!(read("\"claude\"").unwrap(), CrossCheck::Claude);
        assert!(
            read("\"gemini\"")
                .unwrap_err()
                .to_string()
                .contains("false, true, auto, codex or claude")
        );
    }
}
