//! Herdr is the terminal layer; the manager owns goal state and acceptance.

use crate::binding::Existing;
use crate::{
    model::*,
    runner::{self, CommandExecutor, Options},
    store::Store,
    view::{print_goal, print_list},
};
use anyhow::{Context, Result, bail, ensure};
use std::{io::Write, path::Path};

use crate::herdr_cli::call;
/// Opens a plugin tab for the focused pane. Goal Manager opens once per tab:
/// from a tab that already has one, from that manager, or from a helper's tab
/// it switches to the running manager or refuses instead of starting another.
/// `state_root` holds every folder's saved managers.
pub(crate) async fn open_panel(entrypoint: &str, state_root: &Path) -> Result<()> {
    ensure!(
        std::env::var("HERDR_ENV").as_deref() == Ok("1"),
        "open this action inside Herdr"
    );
    let raw =
        std::env::var("HERDR_PLUGIN_CONTEXT_JSON").context("Herdr plugin context is missing")?;
    let context: serde_json::Value =
        serde_json::from_str(&raw).context("parse Herdr plugin context")?;
    let workspace = context["focused_pane_cwd"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| context["workspace_cwd"].as_str())
        .context("Herdr did not provide a working directory")?;
    let workspace_id = context["workspace_id"]
        .as_str()
        .context("Herdr did not provide a workspace ID")?;
    let origin_pane = context["focused_pane_id"]
        .as_str()
        .context("invoking pane is missing")?;
    let origin = crate::supervision::capture_origin(origin_pane).await?;
    let target: crate::supervision::Target = serde_json::from_str(&origin)?;
    let label = tab_label(&target.tab).await.unwrap_or_default();
    if entrypoint == "panel" {
        ensure!(
            label != WATCH_TAB,
            "this is the Goal watch tab; open Goal Manager from the main tab"
        );
        match crate::binding::existing_manager(state_root, &target).await? {
            Some(Existing::Manager(manager)) => {
                return call(&["tab".into(), "focus".into(), manager.tab]).await;
            }
            Some(Existing::Helper(role)) => {
                bail!("this tab runs the {role} helper; open Goal Manager from the main tab")
            }
            None => {}
        }
    }
    if unnamed(&label)
        && let Err(error) = call(&["tab".into(), "rename".into(), target.tab, "main".into()]).await
    {
        eprintln!("Could not name the original tab: {error:#}");
    }
    let args = vec![
        "plugin".into(),
        "pane".into(),
        "open".into(),
        "--plugin".into(),
        "herdr-goal-manager".into(),
        "--entrypoint".into(),
        entrypoint.into(),
        "--workspace".into(),
        workspace_id.into(),
        "--placement".into(),
        "tab".into(),
        "--focus".into(),
        "--env".into(),
        format!("HGM_WORKSPACE={workspace}"),
        "--env".into(),
        format!("HGM_ORIGIN_JSON={origin}"),
        // The new pane uses the state this command searched.
        "--env".into(),
        format!("HGM_STATE_DIR={}", state_root.display()),
    ];
    call(&args).await
}

/// The plugin manifest this build ships, for a plugin folder around an
/// installed binary (see [`plugin_folder`]).
const PLUGIN_MANIFEST: &str = include_str!("../herdr-plugin.toml");

/// Makes `dir` a Herdr plugin folder for this binary, so an `hgm` installed
/// without the plugin (for example from crates.io) can be added with
/// `herdr plugin link`: it writes the manifest and links `target/release/hgm`
/// to this executable, so upgrading the binary upgrades the plugin. A folder
/// that holds another plugin's manifest, or an hgm build as a regular file
/// (an archive or a checkout), is left alone.
pub(crate) fn plugin_folder(dir: &Path) -> Result<serde_json::Value> {
    let executable = std::env::current_exe()?.canonicalize()?;
    let manifest = dir.join("herdr-plugin.toml");
    if let Ok(existing) = std::fs::read_to_string(&manifest) {
        ensure!(
            existing.contains("id = \"herdr-goal-manager\""),
            "{} belongs to another plugin; choose another folder with --dir",
            manifest.display()
        );
    }
    let release = dir.join("target/release");
    let binary = release.join("hgm");
    if let Ok(metadata) = std::fs::symlink_metadata(&binary) {
        ensure!(
            metadata.file_type().is_symlink(),
            "{} is an hgm build, not a link; that folder is already a plugin (link it as it is) or choose another folder with --dir",
            binary.display()
        );
    }
    std::fs::create_dir_all(&release).with_context(|| format!("create {}", release.display()))?;
    // Replace any earlier link by rename, so the plugin never sees no binary.
    let temporary = release.join(format!(".hgm-{}", std::process::id()));
    let _ = std::fs::remove_file(&temporary);
    std::os::unix::fs::symlink(&executable, &temporary)?;
    std::fs::rename(&temporary, &binary)?;
    std::fs::write(&manifest, PLUGIN_MANIFEST)
        .with_context(|| format!("write {}", manifest.display()))?;
    Ok(serde_json::json!({
        "folder": dir,
        "binary": executable,
        "link": format!("herdr plugin link {}", crate::manager::quote(dir.as_os_str())),
    }))
}

/// Herdr shows an unnamed tab by its number. A tab the user named keeps its name.
fn unnamed(label: &str) -> bool {
    let label = label.trim();
    label.is_empty() || label.chars().all(|c| c.is_ascii_digit())
}
/// The label hgm gives the watch tab.
pub(crate) const WATCH_TAB: &str = "Goal watch";
/// The tab's label. An unnamed original tab is named `main`, so the pinned
/// target is easy to find next to the Goal Manager tab.
async fn tab_label(tab: &str) -> Result<String> {
    let info = crate::herdr_cli::json_query(&["tab".into(), "get".into(), tab.into()]).await?;
    Ok(info["result"]["tab"]["label"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}

pub(crate) async fn panel(store: &Store) -> Result<()> {
    if std::env::var("HGM_PLUGIN_PANEL").as_deref() == Ok("1")
        && let Ok(tab) = std::env::var("HERDR_TAB_ID")
        && let Err(error) = present_tab(&tab).await
    {
        eprintln!("Could not label manager tab: {error:#}");
    }
    println!("Herdr Goal Manager — {}", store.workspace.display());
    loop {
        let goals = store.list()?;
        println!();
        print_list(&goals);
        println!("new | file <path> | show/run/resume/pause/cancel <number>");
        println!(
            "complete <number> <task> <evidence> | accept <number> <evidence> | note <number> <guidance>"
        );
        println!("Enter: refresh | quit: close panel (running Herdr tabs continue)");
        let Some(line) = input("hgm> ").await? else {
            return Ok(());
        };
        let line = line.trim();
        if matches!(line, "quit" | "q") {
            return Ok(());
        }
        if line.is_empty() {
            continue;
        }
        if let Err(error) = panel_command(store, &goals, line).await {
            eprintln!("{error:#}");
        }
    }
}

async fn present_tab(tab: &str) -> Result<()> {
    call(&[
        "tab".into(),
        "rename".into(),
        tab.into(),
        "Goal Manager".into(),
    ])
    .await?;
    call(&["tab".into(), "focus".into(), tab.into()]).await
}

async fn panel_command(store: &Store, goals: &[Goal], line: &str) -> Result<()> {
    if line == "new" {
        let Some(objective) = input("Desired outcome: ").await? else {
            return Ok(());
        };
        let Some(criterion) = input("Acceptance criterion: ").await? else {
            return Ok(());
        };
        let goal = store.add(
            crate::config::goal_spec(
                &store.workspace,
                serde_json::json!({"objective": objective, "acceptance": [criterion]}),
            )?,
            crate::supervision::origin_terminal(store)?,
        )?;
        print_goal(&goal, store, false)?;
        println!("Created a human task. Import a goal file to configure automatic runners.");
        return Ok(());
    }
    let (operation, rest) = line
        .split_once(char::is_whitespace)
        .context("use an operation and goal number")?;
    let rest = rest.trim();
    if operation == "file" {
        let raw = serde_norway::from_str(
            &std::fs::read_to_string(store.workspace.join(rest)).context("read goal file")?,
        )
        .context("parse goal YAML")?;
        let spec = crate::config::goal_spec(&store.workspace, raw)?;
        let owner = crate::supervision::origin_terminal(store)?;
        return print_goal(&store.add(spec, owner)?, store, false);
    }
    let (index, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let index: usize = index
        .parse()
        .context("goal number must be a positive integer")?;
    let goal = index
        .checked_sub(1)
        .and_then(|i| goals.get(i))
        .context("goal number is not in the displayed list")?;
    let result = match operation {
        "show" => store.load(&goal.id)?,
        "pause" | "cancel" => {
            let control = if operation == "pause" {
                Control::Pause
            } else {
                Control::Cancel
            };
            let goal = store.update(&goal.id, |g| g.request(control))?;
            crate::watch::release_watches(store, &goal.id, "stopped_by_goal")?;
            goal
        }
        "note" => store.update(&goal.id, |g| g.note(rest.trim().into()))?,
        "complete" => {
            let (task, evidence) = rest
                .trim()
                .split_once(char::is_whitespace)
                .context("complete needs a task ID and evidence")?;
            store.update(&goal.id, |g| {
                g.complete_manual(task, evidence.trim().into())
            })?
        }
        "accept" => runner::accept(store, &goal.id, rest.trim(), &CommandExecutor).await?,
        "run" | "resume" => {
            ensure!(
                std::env::var("HERDR_ENV").as_deref() == Ok("1"),
                "run outside Herdr with: hgm --workspace '{}' {operation} {}",
                store.workspace.display(),
                goal.id
            );
            ensure!(
                !matches!(goal.status, GoalStatus::Completed | GoalStatus::Cancelled),
                "terminal goals cannot run"
            );
            ensure!(
                operation == "resume" || goal.status == GoalStatus::Ready,
                "inspect partial work, then use resume"
            );
            let workspace =
                std::env::var("HERDR_WORKSPACE_ID").context("Herdr workspace ID is missing")?;
            call(&run_pane_args(
                &workspace,
                &store.workspace,
                &store.root,
                &goal.id,
                operation,
            ))
            .await?;
            println!(
                "Requested {operation} in a background Herdr tab. Refresh to read persisted progress."
            );
            return Ok(());
        }
        _ => bail!("unknown operation: {operation}"),
    };
    print_goal(&result, store, false)
}

fn run_pane_args(
    workspace: &str,
    directory: &Path,
    state: &Path,
    id: &str,
    operation: &str,
) -> Vec<String> {
    vec![
        "plugin".into(),
        "pane".into(),
        "open".into(),
        "--plugin".into(),
        "herdr-goal-manager".into(),
        "--entrypoint".into(),
        "runner".into(),
        "--placement".into(),
        "tab".into(),
        "--workspace".into(),
        workspace.into(),
        "--env".into(),
        format!("HGM_WORKSPACE={}", directory.display()),
        "--env".into(),
        format!("HGM_STATE_DIR={}", state.display()),
        "--env".into(),
        format!("HGM_GOAL_ID={id}"),
        "--env".into(),
        format!("HGM_OPERATION={operation}"),
        "--no-focus".into(),
    ]
}

pub(crate) async fn run_pane(store: &Store) -> Result<()> {
    let id = std::env::var("HGM_GOAL_ID").context("runner pane is missing goal ID")?;
    validate_id(&id)?;
    let operation = std::env::var("HGM_OPERATION").context("runner pane is missing operation")?;
    ensure!(
        matches!(operation.as_str(), "run" | "resume"),
        "invalid runner pane operation"
    );
    let result = runner::run(
        store,
        &id,
        &CommandExecutor,
        Options {
            resume: operation == "resume",
            timeout_secs: 600,
            max_attempts: 3,
            max_repairs: 2,
        },
    )
    .await;
    if let Ok(goal) = store.load(&id) {
        print_goal(&goal, store, false)?;
    }
    if let Err(error) = &result {
        eprintln!("{error:#}");
    }
    input("Press Enter to close this execution tab.").await?;
    result.map(|_| ())
}

async fn input(prompt: &str) -> Result<Option<String>> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
    })
    .await
    .context("read panel input")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_uses_opaque_context_and_does_not_move_focus_or_change_plugin_cwd() {
        let args = run_pane_args(
            "w99",
            Path::new("/work 'quoted'"),
            Path::new("/state"),
            "g-123",
            "resume",
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--env", "HGM_WORKSPACE=/work 'quoted'"])
        );
        assert!(args.windows(2).any(|pair| pair == ["--workspace", "w99"]));
        assert!(args.windows(2).any(|pair| pair == ["--placement", "tab"]));
        assert_eq!(args.last().map(String::as_str), Some("--no-focus"));
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--cwd" || arg == "--target-pane")
        );
    }
}
