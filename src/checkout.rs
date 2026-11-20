use crate::{
    binding::args,
    herdr_cli as herdr,
    model::{Checkout, Goal, GoalStatus},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde_json::json;
use std::path::{Path, PathBuf};

fn git(directory: &Path, arguments: &[&str]) -> Result<std::process::Output> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .with_context(|| format!("run git in {}", directory.display()))
}

fn worktree_of(repository: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let output = git(repository, &["worktree", "list", "--porcelain"])?;
    ensure!(
        output.status.success(),
        "git worktree list failed in {}",
        repository.display()
    );
    let head = format!("branch refs/heads/{branch}");
    let mut path = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(found) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(found));
        } else if line == head {
            return Ok(path);
        }
    }
    Ok(None)
}

pub(crate) async fn prepare(goal: &Goal) -> Result<Checkout> {
    let spec = goal
        .spec
        .checkout
        .as_ref()
        .context("the contract asks for no checkout")?;
    let repository = spec
        .repository
        .clone()
        .unwrap_or_else(|| goal.workspace.clone());
    ensure!(
        git(&repository, &["rev-parse", "--git-dir"])?
            .status
            .success(),
        "checkout repository {} is not a Git repository",
        repository.display()
    );
    let repo = repository.to_string_lossy().into_owned();
    let label = format!("goal {}", &goal.id[..goal.id.len().min(10)]);
    let reply = match worktree_of(&repository, &spec.branch)? {
        Some(path) => {
            let path = path.to_string_lossy().into_owned();
            herdr::json_query(&args(&[
                "worktree",
                "open",
                "--cwd",
                &repo,
                "--path",
                &path,
                "--label",
                &label,
                "--no-focus",
            ]))
            .await
            .context("herdr worktree open")?
        }
        None => {
            let branch = format!("refs/heads/{}", spec.branch);
            ensure!(
                !git(&repository, &["rev-parse", "--verify", "--quiet", &branch])?
                    .status
                    .success(),
                "branch {} already exists in {} without a worktree; check it out yourself, or ask for another branch",
                spec.branch,
                repository.display()
            );
            let path = spec
                .path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned());
            let mut arguments = vec![
                "worktree",
                "create",
                "--cwd",
                &repo,
                "--branch",
                &spec.branch,
                "--base",
                &spec.base,
                "--label",
                &label,
                "--no-focus",
            ];
            if let Some(path) = &path {
                arguments.extend(["--path", path]);
            }
            herdr::json_query(&args(&arguments))
                .await
                .context("herdr worktree create")?
        }
    };
    let result = &reply["result"];
    let workspace_id = result["workspace"]["workspace_id"]
        .as_str()
        .context("Herdr returned no workspace for the checkout")?;
    let path = result["worktree"]["path"]
        .as_str()
        .or(result["workspace"]["worktree"]["checkout_path"].as_str())
        .context("Herdr returned no checkout path")?;
    Ok(Checkout {
        path: path.into(),
        branch: spec.branch.clone(),
        base: spec.base.clone(),
        workspace_id: workspace_id.into(),
        removed_at: None,
    })
}

pub(crate) async fn remove(store: &Store, id: &str) -> Result<Goal> {
    let goal = store.load(id)?;
    let checkout = goal
        .checkout
        .clone()
        .filter(|checkout| checkout.removed_at.is_none())
        .with_context(|| format!("goal {id} has no checkout to remove"))?;
    ensure!(
        goal.status != GoalStatus::Running,
        "goal {id} is running; pause it before removing its checkout (nothing was removed)"
    );
    ensure!(
        !crate::watch::watched(store, id)?,
        "a running watch serves goal {id}; wait or pause the goal first (nothing was removed)"
    );
    let status = git(&checkout.path, &["status", "--porcelain"])?;
    ensure!(
        status.status.success(),
        "git status failed in {}",
        checkout.path.display()
    );
    let changes = String::from_utf8_lossy(&status.stdout).lines().count();
    ensure!(
        changes == 0,
        "{} has {changes} uncommitted change(s); commit or discard them first (nothing was removed)",
        checkout.path.display()
    );
    herdr::json_query(&args(&[
        "worktree",
        "remove",
        "--workspace",
        &checkout.workspace_id,
    ]))
    .await
    .context("herdr worktree remove")?;
    let goal = store.update(id, |goal| {
        if let Some(checkout) = &mut goal.checkout {
            checkout.removed_at = Some(Utc::now());
        }
        Ok(())
    })?;
    store.record(
        id,
        "checkout_removed",
        json!({"path": checkout.path, "branch": checkout.branch, "workspace": checkout.workspace_id}),
    )?;
    Ok(goal)
}
