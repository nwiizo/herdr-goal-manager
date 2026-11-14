//! Durable, origin-bound handoff before a graceful native process restart.
use crate::{
    herdr_cli as herdr,
    store::Store,
    supervision::{self, Target},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Transfer {
    id: String,
    predecessor: Target,
    successor: Option<Target>,
    phase: String,
    arguments: Vec<String>,
    document: PathBuf,
    ready: PathBuf,
    acknowledged: PathBuf,
    digest: Option<String>,
    /// The original agent's process id at the request, so a successor without a
    /// session identity yet is known to be another process.
    #[serde(default)]
    predecessor_pid: Option<u64>,
}
fn record_path(store: &Store, context: &str) -> Result<PathBuf> {
    supervision::path(store, context, "-handoff.json")
}
fn persist(store: &Store, context: &str, transfer: &Transfer) -> Result<()> {
    store.write_private(&record_path(store, context)?, transfer)
}
pub(crate) fn inspect(store: &Store) -> Result<Value> {
    let context = supervision::session(store)?;
    let path = record_path(store, &context.key())?;
    if !path.exists() {
        return Ok(json!({"phase":"not_started"}));
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

/// Read only an unambiguous native footer, never percentages in generated prose.
pub(crate) fn context_used(agent: &str, screen: &str) -> Option<f64> {
    for line in screen.lines().rev().take(4) {
        let value = match agent {
            "codex" if line.contains("% context left") => {
                let left = line
                    .split_once("% context left")?
                    .0
                    .split_whitespace()
                    .last()?
                    .parse::<f64>()
                    .ok()?;
                100.0 - left
            }
            "codex" if line.contains("Context ") => {
                let (_, tail) = line.split_once("Context ")?;
                let left: f64 = tail.split_once("% left")?.0.parse().ok()?;
                100.0 - left
            }
            "claude" if line.trim_start().starts_with("╭─") && line.contains(" ctx ") => line
                .split_once(" ctx ")?
                .1
                .split_once("%")?
                .0
                .parse()
                .ok()?,
            "copilot" if line.contains("·") && line.trim_end().ends_with("%)") => line
                .rsplit_once('(')?
                .1
                .trim_end_matches(')')
                .trim_end_matches('%')
                .parse()
                .ok()?,
            _ => continue,
        };
        if (0.0..=100.0).contains(&value) {
            return Some(value);
        }
    }
    None
}

fn validate_arguments(provider: &str, arguments: &[String]) -> Result<()> {
    let valued: &[&str] = match provider {
        "codex" => &[
            "-c",
            "--config",
            "--model",
            "-m",
            "--profile",
            "-p",
            "--sandbox",
            "--ask-for-approval",
            "--add-dir",
            "--enable",
            "--disable",
        ],
        "claude" => &[
            "--model",
            "--permission-mode",
            "--settings",
            "--add-dir",
            "--allowedTools",
            "--allowed-tools",
            "--disallowedTools",
            "--disallowed-tools",
            "--effort",
        ],
        "copilot" => &[
            "--model",
            "--add-dir",
            "--allow-tool",
            "--deny-tool",
            "--reasoning-effort",
            "--context",
            "--mode",
        ],
        _ => anyhow::bail!("unsupported restart provider"),
    };
    let flags = [
        "--no-auto-update",
        "--disable-builtin-mcps",
        "--no-alt-screen",
        "--no-daemon",
        "--dangerously-skip-permissions",
        "--dangerously-bypass-approvals-and-sandbox",
        "--allow-all",
        "--allow-all-tools",
        "--allow-all-paths",
        "--allow-all-urls",
        "--yolo",
        "--no-custom-instructions",
        "--no-experimental",
        "--experimental",
        "--screen-reader",
    ];
    let mut index = 0;
    while index < arguments.len() {
        let arg = &arguments[index];
        if valued.contains(&arg.as_str()) {
            ensure!(index + 1 < arguments.len(), "missing launch option value");
            index += 2;
        } else if flags.contains(&arg.as_str())
            || arg
                .split_once('=')
                .is_some_and(|(key, _)| valued.contains(&key))
        {
            index += 1;
        } else {
            anyhow::bail!(
                "cannot safely reproduce launch argument {arg:?}; start the original agent with explicit supported options before enabling restart"
            );
        }
    }
    Ok(())
}
/// The one foreground process of `provider` in `pane` (its `argv` and `pid`).
async fn provider_process(pane: &str, provider: &str) -> Result<Value> {
    let info = herdr::json_query(&[
        "pane".into(),
        "process-info".into(),
        "--pane".into(),
        pane.into(),
    ])
    .await?;
    let processes = info["result"]["process_info"]["foreground_processes"]
        .as_array()
        .context("native process information missing")?;
    let candidates: Vec<_> = processes
        .iter()
        .filter(|p| {
            p["argv"]
                .as_array()
                .and_then(|argv| argv.first()?.as_str())
                .and_then(|program| std::path::Path::new(program).file_name()?.to_str())
                == Some(provider)
        })
        .collect();
    ensure!(
        candidates.len() == 1,
        "cannot identify one native launch command; original agent retained"
    );
    Ok(candidates[0].clone())
}
/// The original agent's launch options and process id, for a restart with
/// the same options and for telling its successor apart from it.
async fn recipe(origin: &Target) -> Result<(Vec<String>, Option<u64>)> {
    let provider = origin.agent.as_deref().context("no original agent")?;
    let process = provider_process(&origin.pane, provider).await?;
    let arguments = process["argv"]
        .as_array()
        .context("invalid launch argument")?
        .iter()
        .skip(1)
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .context("invalid launch argument")
        })
        .collect::<Result<Vec<_>>>()?;
    validate_arguments(provider, &arguments)?;
    Ok((arguments, process["pid"].as_u64()))
}
/// The shell variable through which a provider's agent can read its own session
/// identity, for providers whose session Herdr reports only after a first turn.
/// Others are added only after a native check.
fn session_variable(provider: &str) -> Option<&'static str> {
    match provider {
        "copilot" => Some("COPILOT_AGENT_SESSION_ID"),
        _ => None,
    }
}

/// Reserve the request before delivery; a transport error never causes a replay.
pub(crate) async fn request(store: &Store) -> Result<Value> {
    let context = supervision::session(store)?;
    let _lease = supervision::operation_lease(store, &context.key())?;
    if record_path(store, &context.key())?.exists() {
        let previous = inspect(store)?;
        ensure!(
            previous["phase"] == "complete",
            "handoff already exists; use restart to reconcile it"
        );
    }
    let observation = supervision::target(store, None, None).await?;
    ensure!(
        supervision::can_prompt(
            &observation["live"],
            observation["screen"].as_str().unwrap_or("")
        ),
        "wait for the original agent to finish its turn and clear its input before requesting handoff"
    );
    let origin: Target = serde_json::from_value(observation["binding"].clone())?;
    ensure!(
        origin
            .session
            .as_ref()
            .and_then(|s| s["value"].as_str())
            .is_some(),
        "native session identity is required for restart"
    );
    let (arguments, predecessor_pid) = recipe(&origin).await?;
    let id = format!("transfer-{}", uuid::Uuid::new_v4());
    let directory = store.private_directory("manager")?;
    let mut transfer = Transfer {
        document: directory.join(format!("{id}.md")),
        ready: directory.join(format!("{id}-ready.json")),
        acknowledged: directory.join(format!("{id}-ack.json")),
        id,
        predecessor: origin,
        successor: None,
        phase: "request_delivery_uncertain".into(),
        arguments,
        digest: None,
        predecessor_pid,
    };
    persist(store, &context.key(), &transfer)?;
    let ready = json!({"transfer":transfer.id,"session":transfer.predecessor.session.as_ref().and_then(|s|s["value"].as_str()),"safe_to_restart":true});
    let prompt = format!(
        "Goal Manager handoff request. Do not compact. Stop new assignments and reach a safe boundary. Save a complete handoff at {} containing the goal, current instructions and permissions, scope, verified results and paths, pending work, failed approaches, live workers and the next action. Finish any process/subagent that depends on this CLI, or report why restart is unsafe. Only when the document is saved and no work depends on this CLI, atomically write {} to {}. Then finish this turn; Goal Manager will restart this same tab. Do not exit yourself.",
        transfer.document.display(),
        ready,
        transfer.ready.display()
    );
    herdr::query(&[
        "agent".into(),
        "prompt".into(),
        transfer.predecessor.pane.clone(),
        prompt,
    ])
    .await
    .context("handoff request delivery uncertain; inspect target, do not resend")?;
    transfer.phase = "awaiting_handoff".into();
    persist(store, &context.key(), &transfer)?;
    // The watch advances the transfer. When none runs for this tab (its goal
    // ended before the handoff), open it again; without a watch record the
    // manager advances the transfer with `hgm restart`.
    if let Ok(watch) = crate::watch::load_watch(store, &context.key())
        && !crate::watch::watch_running(store, &watch, chrono::Utc::now().timestamp())?
        && let Err(error) = supervision::open_monitor(store, &context).await
    {
        eprintln!(
            "Could not open the watch that advances this handoff ({error:#}); run hgm restart until it completes."
        );
    }
    Ok(serde_json::to_value(transfer)?)
}
fn document_digest(transfer: &Transfer) -> Result<String> {
    let metadata =
        std::fs::symlink_metadata(&transfer.document).context("handoff document not saved yet")?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() > 0 && metadata.len() <= 131_072,
        "handoff must be a regular, nonempty file up to 128 KiB"
    );
    let text = std::fs::read_to_string(&transfer.document)?;
    ensure!(!text.trim().is_empty(), "handoff is empty");
    Ok(format!("{:x}", Sha256::digest(text.as_bytes())))
}
fn read_receipt(path: &std::path::Path) -> Result<Value> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() <= 8192,
        "invalid handoff receipt"
    );
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
/// One bounded transition. The monitor retries observations, never uncertain mutations.
pub(crate) async fn advance(store: &Store) -> Result<Value> {
    let mut context = supervision::session(store)?;
    let _lease = supervision::operation_lease(store, &context.key())?;
    let mut transfer: Transfer =
        serde_json::from_slice(&std::fs::read(record_path(store, &context.key())?)?)?;
    let live = supervision::live_pane(&transfer.predecessor.pane).await?;
    ensure!(
        live["terminal_id"] == transfer.predecessor.terminal
            && live["tab_id"] == transfer.predecessor.tab,
        "original terminal changed; restart stopped"
    );
    match transfer.phase.as_str() {
        "request_delivery_uncertain" | "awaiting_handoff" => {
            transfer.predecessor.check(&live)?;
            if !transfer.ready.exists() {
                return Ok(serde_json::to_value(transfer)?);
            }
            let receipt = read_receipt(&transfer.ready)?;
            ensure!(
                receipt["transfer"] == transfer.id
                    && receipt["session"]
                        == transfer
                            .predecessor
                            .session
                            .as_ref()
                            .context("missing predecessor session")?["value"]
                    && receipt["safe_to_restart"] == true,
                "handoff receipt does not release this predecessor"
            );
            let screen = supervision::screen(&transfer.predecessor.pane).await?;
            if !supervision::can_prompt(&live, &screen) {
                return Ok(serde_json::to_value(transfer)?);
            }
            transfer.digest = Some(document_digest(&transfer)?);
            transfer.phase = "exit_delivery_uncertain".into();
            persist(store, &context.key(), &transfer)?;
            herdr::query(&[
                "agent".into(),
                "prompt".into(),
                transfer.predecessor.pane.clone(),
                "/exit".into(),
            ])
            .await
            .context("exit admission uncertain; observe the original tab before retrying")?;
            transfer.phase = "awaiting_exit".into();
        }
        "exit_delivery_uncertain" | "awaiting_exit" => {
            if live.get("agent").is_some() {
                transfer.predecessor.check(&live)?;
                return Ok(serde_json::to_value(transfer)?);
            }
            ensure!(
                Some(document_digest(&transfer)?) == transfer.digest,
                "saved handoff changed after release"
            );
            transfer.phase = "start_delivery_uncertain".into();
            persist(store, &context.key(), &transfer)?;
            let mut args = vec![
                "agent".into(),
                "start".into(),
                format!("hgm-{}", &transfer.id[9..17]),
                "--kind".into(),
                transfer
                    .predecessor
                    .agent
                    .clone()
                    .context("missing provider")?,
                "--pane".into(),
                transfer.predecessor.pane.clone(),
                "--".into(),
            ];
            args.extend(transfer.arguments.clone());
            herdr::query(&args).await.context("successor launch uncertain; inspect the same tab, never start a second replacement")?;
            transfer.phase = "awaiting_successor".into();
        }
        "start_delivery_uncertain" | "awaiting_successor" => {
            if live.get("agent").is_none() {
                return Ok(serde_json::to_value(transfer)?);
            }
            ensure!(
                live["agent"].as_str() == transfer.predecessor.agent.as_deref(),
                "unexpected successor provider"
            );
            let successor = Target::from_value(&live)?;
            let provider = successor.agent.clone().context("missing provider")?;
            // Some providers get a session identity only with their first turn
            // (Copilot). Such a successor is adopted before it has one only as
            // another process in the predecessor's terminal, started after the
            // predecessor exited (this phase follows the observed exit); its
            // session is checked against Herdr's report after that turn.
            let variable = session_variable(&provider).filter(|_| successor.session.is_none());
            if variable.is_some() {
                let pid = provider_process(&successor.pane, &provider).await?["pid"].as_u64();
                let another_process = transfer
                    .predecessor_pid
                    .is_none_or(|before| pid.is_some_and(|now| now != before));
                if !another_process {
                    return Ok(serde_json::to_value(transfer)?);
                }
            } else if successor.session.is_none()
                || successor.session == transfer.predecessor.session
            {
                return Ok(serde_json::to_value(transfer)?);
            }
            let screen = supervision::screen(&successor.pane).await?;
            if !supervision::can_prompt(&live, &screen) {
                return Ok(serde_json::to_value(transfer)?);
            }
            ensure!(
                Some(document_digest(&transfer)?) == transfer.digest,
                "handoff changed after restart"
            );
            let (session, identity) = match (&successor.session, variable) {
                (Some(session), _) => (session["value"].clone(), String::new()),
                (None, Some(variable)) => (
                    json!("SESSION_ID"),
                    format!(
                        " Replace SESSION_ID with your own session id: the output of printf %s \"${variable}\" in a shell command."
                    ),
                ),
                (None, None) => anyhow::bail!("missing successor session"),
            };
            let receipt =
                json!({"transfer":transfer.id,"session":session,"sha256":transfer.digest});
            let prompt = format!(
                "Goal Manager fresh-session handoff. Read current workspace instructions and {}. Verify its scope, permissions, evidence and next action; do not expand authority or replay completed work. After reading and accepting ownership, atomically write {} to {}.{} Then, as a fresh reader, briefly note what worked, what failed and which expectations missed, and choose the step that best closes the gap to the acceptance; the recorded next action is a candidate, not an order. Continue with that step. Do not compact; prepare another handoff near 85% context.",
                transfer.document.display(),
                receipt,
                transfer.acknowledged.display(),
                identity
            );
            transfer.successor = Some(successor);
            transfer.phase = "adoption_delivery_uncertain".into();
            persist(store, &context.key(), &transfer)?;
            herdr::query(&[
                "agent".into(),
                "prompt".into(),
                transfer.predecessor.pane.clone(),
                prompt,
            ])
            .await
            .context("handoff admission uncertain; inspect successor before resending")?;
            transfer.phase = "awaiting_acknowledgement".into();
        }
        "adoption_delivery_uncertain" | "awaiting_acknowledgement" => {
            transfer
                .successor
                .as_ref()
                .context("missing successor")?
                .check(&live)?;
            if !transfer.acknowledged.exists() {
                return Ok(serde_json::to_value(transfer)?);
            }
            // The session Herdr reports now. A successor adopted before its
            // first turn must have one by the time it acknowledges, distinct
            // from the predecessor's and equal to the one it wrote.
            let Some(reported) = live
                .get("agent_session")
                .filter(|session| session["value"].is_string())
                .cloned()
            else {
                return Ok(serde_json::to_value(transfer)?);
            };
            ensure!(
                transfer.predecessor.session.as_ref() != Some(&reported),
                "the successor reports the predecessor's session; restart stopped"
            );
            let receipt = read_receipt(&transfer.acknowledged)?;
            ensure!(
                receipt["transfer"] == transfer.id
                    && receipt["session"] == reported["value"]
                    && receipt["sha256"].as_str() == transfer.digest.as_deref(),
                "successor acknowledgement mismatch"
            );
            let successor = Target::from_value(&live)?;
            context.origin = Some(successor.clone());
            store.write_private(&supervision::path(store, &context.id, ".json")?, &context)?;
            crate::watch::rebind_watch(
                store,
                &context.key(),
                &successor,
                json!({"via": "handoff", "transfer": transfer.id}),
            )?;
            transfer.successor = Some(successor);
            transfer.phase = "complete".into();
        }
        "complete" => {}
        _ => anyhow::bail!("unknown transfer phase; inspect saved handoff"),
    }
    persist(store, &context.key(), &transfer)?;
    Ok(serde_json::to_value(transfer)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn percentages_are_context_only_and_boundary_is_85() {
        assert_eq!(
            context_used("codex", "report 90%\n 15% context left"),
            Some(85.0)
        );
        assert_eq!(context_used("copilot", "model · (85%)"), Some(85.0));
        assert_eq!(context_used("claude", "task 95% complete"), None);
        assert_eq!(context_used("copilot", "used 99% of weekly limit"), None);
    }
    #[test]
    fn launch_options_preserved_but_history_replay_rejected() {
        assert!(
            validate_arguments("codex", &["--sandbox".into(), "workspace-write".into()]).is_ok()
        );
        assert!(validate_arguments("codex", &["resume".into(), "previous".into()]).is_err());
        assert!(validate_arguments("copilot", &["--session-id=previous".into()]).is_err());
        assert!(validate_arguments("claude", &["--model".into()]).is_err());
    }
}
