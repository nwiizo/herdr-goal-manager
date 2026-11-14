//! The manager's binding to Herdr: the pinned target, the saved manager
//! context, private state paths and leases, and the pane reads that dispatch
//! and the watch both build on.
use crate::{
    herdr_cli as herdr,
    store::{LOCK_RETRY, LOCK_WAIT, Store},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Target {
    pub pane: String,
    pub tab: String,
    pub terminal: String,
    pub agent: Option<String>,
    pub session: Option<Value>,
}
impl Target {
    pub(crate) fn from_value(value: &Value) -> Result<Self> {
        let required = |key: &str| -> Result<String> {
            Ok(value[key]
                .as_str()
                .with_context(|| format!("Herdr missing {key}"))?
                .into())
        };
        Ok(Self {
            pane: required("pane_id")?,
            tab: required("tab_id")?,
            terminal: required("terminal_id")?,
            agent: value["agent"].as_str().map(str::to_owned),
            session: value.get("agent_session").cloned(),
        })
    }
    pub(crate) fn check(&self, live: &Value) -> Result<()> {
        ensure!(
            live["terminal_id"].as_str() == Some(&self.terminal)
                && live["tab_id"].as_str() == Some(&self.tab),
            "bound terminal moved or was replaced; reopen Goal Manager from the intended tab"
        );
        if let Some(agent) = &self.agent {
            ensure!(
                live["agent"].as_str() == Some(agent),
                "bound agent was replaced"
            );
        }
        if let Some(session) = &self.session {
            ensure!(live.get("agent_session") == Some(session), SESSION_CHANGED);
        }
        Ok(())
    }
}
/// The pane is no longer in Herdr's inventory (it exited or was closed).
pub(crate) const PANE_GONE: &str = "bound pane no longer exists";
/// The pinned tab runs another agent session than the bound one.
pub(crate) const SESSION_CHANGED: &str = "bound agent session changed";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Session {
    pub id: String,
    pub workspace: PathBuf,
    pub socket: Option<String>,
    pub origin: Option<Target>,
}
impl Session {
    /// Where the watch, the handoff transfer, roles and their leases are kept:
    /// the pinned terminal, so a manager reopened for the same tab finds them.
    /// A manager without an original pane keeps them under its own context.
    pub(crate) fn key(&self) -> String {
        self.origin.as_ref().map_or_else(
            || self.id.clone(),
            |origin| format!("t-{}", origin.terminal),
        )
    }
}
/// State kept per pinned terminal (see [`Session::key`]).
const TERMINAL_STATE: [&str; 3] = ["-watch.json", "-handoff.json", "-roles.json"];
/// Moves state an earlier build saved under the manager context to the
/// terminal key, unless the terminal already has its own.
fn adopt_context_state(store: &Store, session: &Session) -> Result<()> {
    let key = session.key();
    if key == session.id {
        return Ok(());
    }
    for suffix in TERMINAL_STATE {
        let current = path(store, &key, suffix)?;
        let legacy = path(store, &session.id, suffix)?;
        if !current.exists() && legacy.exists() {
            std::fs::rename(&legacy, &current)
                .with_context(|| format!("move {} to {}", legacy.display(), current.display()))?;
        }
    }
    Ok(())
}
pub(crate) fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).into()).collect()
}
pub(crate) async fn live_pane(pane: &str) -> Result<Value> {
    let panes = herdr::json_query(&args(&["pane", "list"])).await?;
    let mut live = panes["result"]["panes"]
        .as_array()
        .context("Herdr pane list is missing")?
        .iter()
        .find(|p| p["pane_id"].as_str() == Some(pane))
        .cloned()
        .context(PANE_GONE)?;
    let agents = herdr::json_query(&args(&["agent", "list"])).await?;
    if let Some(agent) = agents["result"]["agents"]
        .as_array()
        .and_then(|list| list.iter().find(|p| p["pane_id"].as_str() == Some(pane)))
    {
        live = agent.clone();
    }
    Ok(live)
}
pub(crate) fn path(store: &Store, id: &str, suffix: &str) -> Result<PathBuf> {
    crate::model::validate_id(id)?;
    Ok(store
        .private_directory("manager")?
        .join(format!("{id}{suffix}")))
}
/// A new manager context for this launch; [`save_context`] keeps it once the
/// launch is allowed, so a refused launch leaves no file behind.
pub(crate) fn prepare_context(store: &Store) -> Result<Session> {
    let origin = match std::env::var("HGM_ORIGIN_JSON") {
        Ok(text) => Some(serde_json::from_str(&text).context("parse original pane binding")?),
        Err(_) => None,
    };
    Ok(Session {
        id: format!("m-{}", uuid::Uuid::new_v4()),
        workspace: store.workspace.clone(),
        socket: std::env::var("HERDR_SOCKET_PATH").ok(),
        origin,
    })
}
pub(crate) fn save_context(store: &Store, session: &Session) -> Result<()> {
    store.write_private(&path(store, &session.id, ".json")?, session)
}
pub(crate) async fn capture_origin(pane: &str) -> Result<String> {
    Ok(serde_json::to_string(&Target::from_value(
        &live_pane(pane).await?,
    )?)?)
}
fn read_session(store: &Store, id: &str) -> Result<Session> {
    let bytes = std::fs::read(path(store, id, ".json")?)
        .with_context(|| format!("manager context {id} is not saved for this workspace"))?;
    Ok(serde_json::from_slice(&bytes)?)
}
/// The current manager's original terminal: it scopes briefs and goal ownership.
/// Without a Herdr-bound manager context the workspace is shared (`None`).
pub(crate) fn origin_terminal(store: &Store) -> Result<Option<String>> {
    let Ok(id) = std::env::var("HGM_CONTEXT") else {
        return Ok(None);
    };
    Ok(read_session(store, &id)?
        .origin
        .map(|origin| origin.terminal))
}
pub(crate) fn session(store: &Store) -> Result<Session> {
    let id =
        std::env::var("HGM_CONTEXT").context("open Goal Manager from its Herdr shortcut first")?;
    let session = read_session(store, &id)?;
    ensure!(
        session.workspace == store.workspace,
        "manager belongs to another workspace"
    );
    ensure!(
        std::env::var("HERDR_ENV").as_deref() == Ok("1"),
        "this operation requires Herdr"
    );
    ensure!(
        session.socket.is_some() && std::env::var("HERDR_SOCKET_PATH").ok() == session.socket,
        "Herdr session changed; reopen the manager in the intended session"
    );
    adopt_context_state(store, &session)?;
    Ok(session)
}
/// The Goal Manager open for a pinned tab: one per original terminal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ManagerRecord {
    pub context: String,
    pub manager: Target,
    /// When the manager registered (Unix seconds; 0 in older records).
    #[serde(default)]
    pub registered_at: i64,
}
/// Records `manager` as the Goal Manager open for `session`'s tab.
pub(crate) fn register_manager(store: &Store, session: &Session, manager: &Target) -> Result<()> {
    store.write_private(
        &path(store, &session.key(), "-manager.json")?,
        &ManagerRecord {
            context: session.id.clone(),
            manager: manager.clone(),
            registered_at: chrono::Utc::now().timestamp(),
        },
    )
}
/// How long a registered manager counts as running before Herdr shows its
/// agent (the provider is still starting in the pane).
const LAUNCH_GRACE_SECS: i64 = 30;
/// Whether a Goal Manager still runs in `manager`'s pane: the same terminal,
/// with an agent in it, or registered so recently that its agent may not show
/// yet. The manager pane closes when its process exits.
async fn manager_runs(manager: &Target, registered_at: i64) -> bool {
    let starting = chrono::Utc::now().timestamp() - registered_at < LAUNCH_GRACE_SECS;
    live_pane(&manager.pane).await.is_ok_and(|live| {
        live["terminal_id"].as_str() == Some(&manager.terminal)
            && (starting || live.get("agent").is_some())
    })
}
/// Serializes Goal Manager launches for the tab of `terminal` across every
/// folder under `root`, so checking for a running manager and registering the
/// new one cannot interleave with another launch. Waits up to 10 seconds.
pub(crate) async fn launch_lease(root: &Path, terminal: &str) -> Result<std::fs::File> {
    let name = format!("t-{terminal}");
    crate::model::validate_id(&name)?;
    let directory = root.join("launch");
    std::fs::create_dir_all(&directory)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(format!("{name}.lock")))?;
    for _ in 0..50 {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    anyhow::bail!("another Goal Manager is starting for this tab; retry in a moment")
}
/// What already serves the tab `origin` when Goal Manager is opened from it.
#[derive(Debug)]
pub(crate) enum Existing {
    /// The running Goal Manager of this tab, or `origin` is that manager.
    Manager(Target),
    /// `origin` is a helper role's tab (impl or reviewer) of some manager.
    Helper(String),
}
/// Finds what already serves `origin`, in every folder's state under `root`:
/// a running manager recorded for its terminal (or named by the terminal's
/// watch, for managers of earlier builds), the manager itself, or a helper.
pub(crate) async fn existing_manager(root: &Path, origin: &Target) -> Result<Option<Existing>> {
    let tab = format!("t-{}", origin.terminal);
    let Ok(workspaces) = std::fs::read_dir(root.join("workspaces")) else {
        return Ok(None);
    };
    for workspace in workspaces.flatten() {
        let Ok(files) = std::fs::read_dir(workspace.path().join("manager")) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let Some((key, kind)) = name
                .strip_suffix(".json")
                .and_then(|stem| stem.rsplit_once('-'))
                .filter(|(_, kind)| matches!(*kind, "manager" | "watch" | "roles"))
            else {
                continue;
            };
            let Ok(value) = std::fs::read(file.path())
                .map_err(anyhow::Error::from)
                .and_then(|bytes| Ok(serde_json::from_slice::<Value>(&bytes)?))
            else {
                continue;
            };
            if kind == "roles" {
                let helper = value.as_object().and_then(|roles| {
                    roles
                        .iter()
                        .find(|(_, role)| role["target"]["terminal"] == origin.terminal.as_str())
                });
                if let Some((role, _)) = helper {
                    return Ok(Some(Existing::Helper(role.clone())));
                }
                continue;
            }
            let Ok(manager) = serde_json::from_value::<Target>(value["manager"].clone()) else {
                continue;
            };
            // Watch records (managers of earlier builds) carry no registration time.
            let registered_at = value["registered_at"].as_i64().unwrap_or(0);
            if (key == tab || manager.terminal == origin.terminal)
                && manager_runs(&manager, registered_at).await
            {
                return Ok(Some(Existing::Manager(manager)));
            }
        }
    }
    Ok(None)
}
fn operation_lock(store: &Store, context: &str) -> Result<std::fs::File> {
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path(store, context, "-operation.lock")?)?)
}
/// The supervision lock, taken at once or not at all: the watch skips a step
/// when a manager command holds it.
pub(crate) fn operation_lease(store: &Store, context: &str) -> Result<std::fs::File> {
    let file = operation_lock(store, context)?;
    file.try_lock()
        .context("supervision state is busy; retry the operation")?;
    Ok(file)
}
/// The supervision lock for a manager command. A watch step holds it for a
/// few seconds, so a busy lock is retried for up to [`LOCK_WAIT`] before the
/// command fails with nothing delivered or changed.
pub(crate) async fn wait_operation_lease(store: &Store, context: &str) -> Result<std::fs::File> {
    let file = operation_lock(store, context)?;
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(LOCK_RETRY).await;
            }
            Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!(
                "supervision state stayed busy for {}s ({context}-operation.lock is held, usually by a watch step); nothing was delivered or changed; retry the operation",
                LOCK_WAIT.as_secs()
            ),
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).context("lock the supervision state");
            }
        }
    }
}

pub(crate) async fn screen(pane: &str) -> Result<String> {
    screen_lines(pane, 60).await
}
/// The last `lines` visible rows of `pane` as text. Read with styles, so a
/// faint input suggestion is not taken for a draft (see `plain_screen`).
pub(crate) async fn screen_lines(pane: &str, lines: u32) -> Result<String> {
    let ansi = herdr::query(&args(&[
        "agent",
        "read",
        pane,
        "--source",
        "visible",
        "--lines",
        &lines.to_string(),
        "--format",
        "ansi",
    ]))
    .await?;
    Ok(crate::signals::plain_screen(&ansi))
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn bound() -> Target {
        Target {
            pane: "w1:p1".into(),
            tab: "w1:t1".into(),
            terminal: "term1".into(),
            agent: Some("codex".into()),
            session: Some(json!({"value":"session1"})),
        }
    }
    #[test]
    fn binding_rejects_replacement_and_moved_tab() {
        let live = json!({"pane_id":"w1:p1","tab_id":"w1:t1","terminal_id":"term1","agent":"codex","agent_session":{"value":"session1"}});
        assert!(bound().check(&live).is_ok());
        let mut changed = live.clone();
        changed["agent_session"]["value"] = json!("new");
        assert!(bound().check(&changed).is_err());
        let mut changed = live;
        changed["tab_id"] = json!("w2:t1");
        assert!(bound().check(&changed).is_err());
    }
}
