//! A task's write scope: the paths its `write_paths` name. When such a task
//! is assigned, hgm keeps a snapshot of the goal's work dir; while the task is
//! open it compares the work dir with that snapshot, records each changed path
//! outside the scope once and tells the manager. Like protected paths, this
//! observes and reports: hgm never blocks a write.
use crate::{
    events::Event,
    fingerprint::Snapshot,
    model::{Goal, within},
    store::Store,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Timeline kind of changes outside a task's write scope.
pub(crate) const SCOPE_EXCEEDED: &str = "scope_exceeded";
/// Paths named in one wake; the timeline keeps them all.
const NAMED: usize = 10;

/// The work dir when a task's latest linked assignment went out.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Baseline {
    pub at: DateTime<Utc>,
    pub snapshot: Snapshot,
}

fn baseline_path(store: &Store, goal: &str, task: &str) -> Result<std::path::PathBuf> {
    crate::model::validate_id(goal)?;
    crate::model::validate_id(task)?;
    Ok(store
        .private_directory("scope")?
        .join(format!("{goal}-{task}.json")))
}

/// Starts a new scope window for a linked assignment of `task` when the task
/// names write_paths; returns whether it did. A new assignment of the same
/// task starts a new window.
pub(crate) fn begin(store: &Store, goal: &Goal, task: &str) -> Result<bool> {
    let Some(task) = goal.tasks.iter().find(|t| t.spec.id == task) else {
        return Ok(false);
    };
    if task.spec.write_paths.is_empty() {
        return Ok(false);
    }
    let baseline = Baseline {
        at: Utc::now(),
        snapshot: Snapshot::take(goal.work_dir()?)?,
    };
    store.write_private(&baseline_path(store, &goal.id, &task.spec.id)?, &baseline)?;
    Ok(true)
}

fn baseline(store: &Store, goal: &str, task: &str) -> Result<Option<Baseline>> {
    match std::fs::read(baseline_path(store, goal, task)?) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Compares the work dir with the scope window of the task `role` was last
/// assigned. Paths changed outside its write_paths that no earlier record of
/// this window named are recorded as [`SCOPE_EXCEEDED`]; the returned note
/// names them for the manager's wake. A finished task, a task without
/// write_paths or one without a snapshot is not compared.
pub(crate) fn check(
    store: &Store,
    goal: &Goal,
    events: &[Event],
    role: &str,
) -> Result<Option<String>> {
    let Some(task) = events
        .iter()
        .rev()
        .find(|e| e.kind == "assigned" && e.detail["to"].as_str().unwrap_or("main") == role)
        .and_then(|e| e.detail["task"].as_str())
        .and_then(|id| goal.tasks.iter().find(|t| t.spec.id == id))
    else {
        return Ok(None);
    };
    let scope = &task.spec.write_paths;
    if scope.is_empty() || task.succeeded() {
        return Ok(None);
    }
    let Some(baseline) = baseline(store, &goal.id, &task.spec.id)? else {
        return Ok(None);
    };
    let dir = goal.work_dir()?;
    let reported: std::collections::HashSet<&str> = events
        .iter()
        .filter(|e| {
            e.kind == SCOPE_EXCEEDED && e.detail["task"] == task.spec.id && e.at >= baseline.at
        })
        .flat_map(|e| e.detail["paths"].as_array().into_iter().flatten())
        .filter_map(|p| p.as_str())
        .collect();
    let outside: Vec<String> = Snapshot::take(dir)?
        .changed_since(&baseline.snapshot, dir)
        .into_iter()
        .filter(|path| !within(path, scope) && !reported.contains(path.as_str()))
        .collect();
    if outside.is_empty() {
        return Ok(None);
    }
    store.record(
        &goal.id,
        SCOPE_EXCEEDED,
        json!({"task": task.spec.id, "role": role, "paths": outside, "write_paths": scope}),
    )?;
    let mut named = outside
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if outside.len() > NAMED {
        named += &format!(" and {} more (hgm log {})", outside.len() - NAMED, goal.id);
    }
    Ok(Some(format!(
        "{role} changed files outside task {}'s write_paths ({}): {named}. hgm does not block writes; check whether they belong to this task",
        task.spec.id,
        scope.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_outside_write_paths_are_recorded_once_per_window() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        std::fs::create_dir_all(workspace.path().join("src"))?;
        std::fs::write(workspace.path().join("src/a.rs"), "a")?;
        std::fs::write(workspace.path().join("README.md"), "r")?;
        let goal = store.add(
            serde_norway::from_str(
                "objective: o\nacceptance: [a]\ntasks: [{id: work, instruction: w, write_paths: [src]}, {id: free, instruction: f}]\n",
            )?,
            None,
        )?;
        for bad in ["../elsewhere", "/abs", ""] {
            let spec = format!(
                "objective: o\nacceptance: [a]\ntasks: [{{id: w, instruction: w, write_paths: ['{bad}']}}]\n"
            );
            let error = store.add(serde_norway::from_str(&spec)?, None).unwrap_err();
            assert!(format!("{error:#}").contains("write_paths must be workspace-relative"));
        }
        let assign = |task: &str| -> Result<Vec<Event>> {
            store.record(&goal.id, "assigned", json!({"task": task, "to": "main"}))?;
            store.events(&goal.id)
        };
        assert!(!begin(&store, &goal, "free")?, "no write_paths, no window");
        assert!(begin(&store, &goal, "work")?);
        let events = assign("work")?;
        // Inside the scope: nothing to report.
        std::fs::write(workspace.path().join("src/a.rs"), "changed")?;
        std::fs::write(workspace.path().join("src/b.rs"), "new")?;
        assert_eq!(check(&store, &goal, &events, "main")?, None);
        // Outside: recorded and named once.
        std::fs::write(workspace.path().join("README.md"), "rewritten")?;
        std::fs::write(workspace.path().join("notes.txt"), "n")?;
        let note = check(&store, &goal, &events, "main")?.unwrap();
        assert!(
            note.contains(
                "main changed files outside task work's write_paths (src): README.md, notes.txt"
            ) && note.contains("does not block writes"),
            "{note}"
        );
        let events = store.events(&goal.id)?;
        let recorded: Vec<_> = events.iter().filter(|e| e.kind == SCOPE_EXCEEDED).collect();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].detail["paths"],
            json!(["README.md", "notes.txt"])
        );
        assert_eq!(check(&store, &goal, &events, "main")?, None, "named once");
        // Another role's assignment is not this window; a finished task stops it.
        assert_eq!(check(&store, &goal, &events, "impl")?, None);
        std::fs::write(workspace.path().join("other.txt"), "o")?;
        let done = store.update(&goal.id, |g| g.complete_manual("work", "Done".into()))?;
        assert_eq!(check(&store, &done, &events, "main")?, None);
        Ok(())
    }

    #[test]
    fn a_repository_window_counts_edits_untracked_files_and_commits() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let run = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(workspace.path())
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}");
        };
        run(&["init", "-q"]);
        std::fs::create_dir(workspace.path().join("src"))?;
        std::fs::write(workspace.path().join("src/a.rs"), "a")?;
        std::fs::write(workspace.path().join("Cargo.toml"), "c")?;
        std::fs::write(workspace.path().join(".gitignore"), "target/\n")?;
        let commit = |message: &str| {
            run(&["add", "-A"]);
            run(&[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-qm",
                message,
            ]);
        };
        commit("init");
        // Another goal's edit already in the tree is not this task's change.
        std::fs::write(workspace.path().join("Cargo.toml"), "before")?;
        let store = Store::new(workspace.path(), state.path())?;
        let goal = store.add(
            serde_norway::from_str(
                "objective: o\nacceptance: [a]\ntasks: [{id: work, instruction: w, write_paths: [src]}]\n",
            )?,
            None,
        )?;
        assert!(begin(&store, &goal, "work")?);
        store.record(&goal.id, "assigned", json!({"task": "work", "to": "main"}))?;
        std::fs::create_dir(workspace.path().join("target"))?;
        std::fs::write(workspace.path().join("target/out"), "built")?;
        std::fs::write(workspace.path().join("src/a.rs"), "edited")?;
        assert_eq!(
            check(&store, &goal, &store.events(&goal.id)?, "main")?,
            None
        );
        // A commit of files outside the scope is a change, including the edit
        // that was already there.
        std::fs::write(workspace.path().join("docs.md"), "d")?;
        commit("docs");
        let note = check(&store, &goal, &store.events(&goal.id)?, "main")?.unwrap();
        assert!(note.contains(": Cargo.toml, docs.md."), "{note}");
        Ok(())
    }
}
