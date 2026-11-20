use crate::{contract::CrossCheck, store::Store, watch::Watch};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::os::unix::process::CommandExt;

const TIMEOUT_SECS: i64 = 900;
const MAX_FINDINGS: usize = 10;
const MAX_ASSIGNMENT: usize = 2_000;

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
} | "$@" > "$HGM_CROSS_OUT" 2> "$HGM_CROSS_ERR""#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CrossRun {
    pub id: String,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// `codex` or `claude`.
    pub reviewer: String,
    pub started_at: i64,
    /// Historical metadata. Only the supervisor that spawned a child signals it.
    pub pid: u32,
    #[serde(default)]
    pub supervised: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stop_unconfirmed: bool,
}

pub(crate) fn reviewer(mode: CrossCheck, agent: &str) -> Option<&'static str> {
    match mode {
        CrossCheck::Off => None,
        CrossCheck::Auto if agent == "claude" => Some("codex"),
        CrossCheck::Auto | CrossCheck::Claude => Some("claude"),
        CrossCheck::Codex => Some("codex"),
    }
}

fn clean(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

fn stop_group(child: &mut std::process::Child) -> Result<()> {
    // The supervisor holds the unreaped child, so its PID cannot be reused.
    nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(i32::try_from(child.id())?),
        nix::sys::signal::Signal::SIGKILL,
    )
    .context("stop the cross-check process group")?;
    child.wait().context("wait for the stopped cross check")?;
    Ok(())
}

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
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("--workspace")
        .arg(&store.workspace)
        .arg("--state-dir")
        .arg(&store.root)
        .args(["cross-check-worker", &id, reviewer])
        .current_dir(&work_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(dir.join("supervisor.log"))?)
        .process_group(0)
        .spawn()
        .context("start cross-check supervisor")?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
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
        pid,
        supervised: true,
        stop_unconfirmed: false,
    });
    Ok(())
}

pub(crate) fn worker(store: &Store, id: &str, reviewer: &str) -> Result<()> {
    anyhow::ensure!(
        id.starts_with("cross-") && matches!(reviewer, "claude" | "codex"),
        "invalid cross check"
    );
    let dir = store.run_path(id)?;
    anyhow::ensure!(
        dir.join("prompt.txt").is_file(),
        "missing cross-check prompt"
    );
    std::fs::File::create_new(dir.join("supervisor.started"))
        .context("cross-check run already started or unavailable")?;
    let result = dir.join("result.txt");
    let work_dir = std::env::current_dir()?;
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
    let stdout = if reviewer == "codex" {
        dir.join("log.txt")
    } else {
        result.clone()
    };
    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg(SHELL)
        .arg("sh")
        .args(&args)
        .current_dir(&work_dir)
        .env("HGM_CROSS_IN", dir.join("prompt.txt"))
        .env("HGM_CROSS_DIR", &work_dir)
        .env("HGM_CROSS_OUT", &stdout)
        .env("HGM_CROSS_ERR", dir.join("stderr.txt"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            std::fs::write(dir.join("exit"), "launch_failed")?;
            return Err(error).with_context(|| format!("start the {reviewer} check"));
        }
    };
    let start = std::time::Instant::now();
    let outcome = loop {
        if let Some(status) = child.try_wait()? {
            break status.code().unwrap_or(-1).to_string();
        }
        let requested = std::fs::read_to_string(dir.join("stop")).ok();
        if requested.is_some() || start.elapsed().as_secs() >= TIMEOUT_SECS as u64 {
            stop_group(&mut child)?;
            break if requested.as_deref() == Some("stopped") {
                "stopped"
            } else {
                "timeout"
            }
            .into();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    std::fs::write(dir.join("exit.tmp"), outcome)?;
    std::fs::rename(dir.join("exit.tmp"), dir.join("exit"))?;
    Ok(())
}

pub(crate) async fn poll(
    store: &Store,
    watch: &mut Watch,
    now: i64,
    ended: bool,
) -> Result<Option<String>> {
    let Some(run) = watch.cross_check.clone() else {
        return Ok(None);
    };
    let dir = store.run_path(&run.id)?;
    let exit_path = dir.join("exit");
    let mut exit = std::fs::read_to_string(&exit_path).ok();
    if exit.is_none() && (ended || now - run.started_at >= TIMEOUT_SECS) {
        if run.supervised && !run.stop_unconfirmed {
            std::fs::write(dir.join("stop"), if ended { "stopped" } else { "timeout" })?;
            for _ in 0..40 {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                exit = std::fs::read_to_string(&exit_path).ok();
                if exit.is_some() {
                    break;
                }
            }
        }
        if exit.is_none() && !run.stop_unconfirmed {
            let reason = format!(
                "Termination of cross check {} is unconfirmed; inspect its run directory {}",
                run.id,
                dir.display()
            );
            store.record(
                &run.goal,
                "attention",
                json!({"cause":"cross_check_stop_unconfirmed","reason":reason,"run":run.id}),
            )?;
            if let Some(state) = &mut watch.cross_check {
                state.stop_unconfirmed = true;
            }
            return Ok(Some(reason));
        }
    }
    let (verdict, findings, status) = match exit {
        Some(status) => {
            let status = status.trim().to_string();
            let text = std::fs::read_to_string(dir.join("result.txt")).unwrap_or_default();
            let named = text
                .lines()
                .find_map(|line| line.trim().strip_prefix("VERDICT:"))
                .map(|v| v.trim().to_string())
                .filter(|v| v == "approve" || v == "changes_requested");
            let verdict = match named {
                _ if matches!(status.as_str(), "stopped" | "timeout") => status.clone(),
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
        stop_group(&mut shell).unwrap();
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
