//! Another provider's check of the watched worker's work (`loop.cross_check`).
//! When the worker settles after new work, the watch starts a one-shot,
//! read-only run of the other provider's CLI (Codex for a Claude Code worker,
//! Claude Code for the rest, or the one the contract names) on the goal's
//! acceptance and the work dir's changes. The watch does not wait for it: it
//! reads the result at a later observation, records it and wakes the manager.
//! The verdict is an observation only: it completes and accepts nothing.
use crate::{contract::CrossCheck, store::Store, watch::Watch};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{os::unix::process::CommandExt, path::Path};

/// How long a check may run before the watch stops it.
const TIMEOUT_SECS: i64 = 900;
/// The most of the work dir's diff the prompt carries, in bytes.
const MAX_DIFF: usize = 60_000;
/// Findings a result keeps on the timeline and in the wake.
const MAX_FINDINGS: usize = 10;

/// `text` as one line of plain characters, at most `max` of them: the reviewer's
/// output is model output, and control characters (escape sequences, carriage
/// returns) must not reach the manager's terminal through a wake.
fn clean(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Stops a check's process group: the shell leads its own group, so TERM
/// reaches the CLI it started, not only the shell around it.
fn stop_group(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .output();
}

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

/// What the reviewer reads: the contract fields that say what done means, and
/// the work dir's uncommitted changes (`git diff HEAD` and untracked files).
fn prompt(goal: &crate::model::Goal, task: Option<&str>, work_dir: &Path) -> String {
    let spec = &goal.spec;
    let list = |items: &[String]| {
        items
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(work_dir)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let changes = match (
        git(&["diff", "HEAD"]),
        git(&["ls-files", "--others", "--exclude-standard"]),
    ) {
        (Some(diff), Some(untracked)) => {
            let mut diff: String = diff;
            if diff.len() > MAX_DIFF {
                let mut end = MAX_DIFF;
                while !diff.is_char_boundary(end) {
                    end -= 1;
                }
                diff.truncate(end);
                diff.push_str("\n[diff cut at 60 KB; read the files for the rest]");
            }
            format!("git diff HEAD:\n{diff}\nUntracked files:\n{untracked}")
        }
        _ => "The work dir is not a Git repository: read the files it holds.".into(),
    };
    format!(
        "You are checking another agent's work for hgm goal {id} task {task}, read-only: do not modify any file or run anything that writes.\n\
         Work dir: {dir}\nObjective: {objective}\nAcceptance:\n{acceptance}\nOut of scope:\n{scope}\n\n\
         Check the changes against the acceptance: correctness bugs, acceptance items missed or only claimed, and work outside the task.\n\
         Answer with a first line exactly `VERDICT: approve` or `VERDICT: changes_requested`, then at most {max} findings, one per line, each starting with `- `.\n\n{changes}",
        id = goal.id,
        task = task.unwrap_or("(goal)"),
        dir = work_dir.display(),
        objective = spec.objective,
        acceptance = list(&spec.acceptance),
        scope = list(&spec.out_of_scope),
        max = MAX_FINDINGS,
    )
}

/// Starts the check for the work the watched worker just settled on, unless
/// the contract has none or one still runs. The run's files live in the
/// goal's run directory; a shell writes the exit status when it ends.
pub(crate) fn start(
    store: &Store,
    watch: &mut Watch,
    goal: &crate::model::Goal,
    task: Option<&str>,
) -> Result<()> {
    let agent = watch.origin.agent.as_deref().unwrap_or("");
    let Some(reviewer) = reviewer(goal.spec.loop_policy.cross_check, agent) else {
        return Ok(());
    };
    if watch.cross_check.is_some() {
        return Ok(());
    }
    let work_dir = goal.work_dir()?.to_path_buf();
    let id = format!("cross-{}", uuid::Uuid::new_v4());
    let dir = store.run_directory(&id)?;
    std::fs::write(dir.join("prompt.txt"), prompt(goal, task, &work_dir))
        .context("write the cross check prompt")?;
    let result = dir.join("result.txt");
    let mut args: Vec<String> = match reviewer {
        "codex" => vec![
            "exec".into(),
            "--ephemeral".into(),
            "--sandbox".into(),
            "read-only".into(),
            "-C".into(),
            work_dir.display().to_string(),
            "-o".into(),
            result.display().to_string(),
            "-".into(),
        ],
        _ => vec![
            "-p".into(),
            "--permission-mode".into(),
            "dontAsk".into(),
            "--tools".into(),
            "Read,Grep,Glob".into(),
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
        .arg("\"$@\" < \"$HGM_CROSS_IN\" > \"$HGM_CROSS_OUT\" 2> \"$HGM_CROSS_ERR\"; echo $? > \"$HGM_CROSS_EXIT\"")
        .arg("sh")
        .args(&args)
        .current_dir(&work_dir)
        .env("HGM_CROSS_IN", dir.join("prompt.txt"))
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

/// Reads a finished check back: records its verdict and findings and adds them
/// to the next wake. One still running past [`TIMEOUT_SECS`] is stopped and
/// recorded as timed out. Returns the note it added, if any.
pub(crate) fn poll(store: &Store, watch: &mut Watch, now: i64) -> Result<Option<String>> {
    let Some(run) = watch.cross_check.clone() else {
        return Ok(None);
    };
    let dir = store.run_path(&run.id)?;
    let exit = std::fs::read_to_string(dir.join("exit")).ok();
    let (verdict, findings, status) = match exit {
        Some(status) => {
            let text = std::fs::read_to_string(dir.join("result.txt")).unwrap_or_default();
            let verdict = text
                .lines()
                .find_map(|line| line.trim().strip_prefix("VERDICT:"))
                .map(|v| v.trim().to_string())
                .filter(|v| v == "approve" || v == "changes_requested")
                .unwrap_or_else(|| "no_verdict".into());
            let findings: Vec<String> = text
                .lines()
                .map(str::trim)
                .filter(|line| line.starts_with("- "))
                .take(MAX_FINDINGS)
                .map(|line| clean(line, 200))
                .collect();
            (verdict, findings, status.trim().to_string())
        }
        None if now - run.started_at >= TIMEOUT_SECS => {
            stop_group(run.pid);
            ("timeout".to_string(), vec![], String::new())
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
    let detail = if findings.is_empty() {
        String::new()
    } else {
        // Model output, quoted as data: never raw text in the manager's input.
        format!(": {}", serde_json::to_string(&findings).unwrap_or_default())
    };
    let note = format!(
        "the {} check of {}'s work for {} says {verdict}{detail}; it is an observation, not evidence: weigh it against the contract",
        run.reviewer,
        watch.role,
        run.task.as_deref().unwrap_or("the goal")
    );
    Ok(Some(note))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timed_out_check_stops_the_cli_the_shell_started() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        let shell = std::process::Command::new("sh")
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
        let mut shell = shell;
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
