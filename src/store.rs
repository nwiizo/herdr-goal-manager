//! Private atomic records outside the managed workspace.

use crate::events::{self, Event};
use crate::model::{Goal, GoalSpec, validate_id};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub(crate) struct Store {
    pub workspace: PathBuf,
    pub root: PathBuf,
    directory: PathBuf,
}
impl Store {
    pub fn new(workspace: &Path, root: &Path) -> Result<Self> {
        let workspace = workspace.canonicalize().context("resolve workspace")?;
        ensure!(workspace.is_dir(), "workspace must be a directory");
        let key = format!(
            "{:x}",
            Sha256::digest(workspace.as_os_str().as_encoded_bytes())
        );
        Ok(Self {
            directory: root.join("workspaces").join(key),
            workspace,
            root: root.to_path_buf(),
        })
    }
    fn initialize(&self) -> Result<()> {
        for directory in [
            &self.root,
            &self.root.join("workspaces"),
            &self.directory,
            &self.directory.join("goals"),
            &self.directory.join("runs"),
        ] {
            std::fs::create_dir_all(directory)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }
    fn lock(&self, name: &str) -> Result<File> {
        self.initialize()?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(self.directory.join(name))?;
        file.try_lock().with_context(|| {
            format!("workspace manager is busy ({name}); retry after the active operation finishes")
        })?;
        Ok(file)
    }
    /// Exclusive execution ownership across processes. This file lease guards
    /// no shared memory; individual state transactions are synchronous and short.
    pub fn execution_lease(&self) -> Result<File> {
        self.lock("execution.lock")
    }
    /// Registers a goal. `owner` is the original Herdr terminal of the manager.
    pub fn add(&self, spec: GoalSpec, owner: Option<String>) -> Result<Goal> {
        let mut goal = Goal::new(spec, self.workspace.clone())?;
        goal.owner = owner;
        let events = events::transitions(None, &goal);
        {
            let _transaction = self.lock("state.lock")?;
            self.save(&goal)?;
            self.append(&goal.id, &events)?;
        }
        events::notify(&goal.spec.objective, &events);
        Ok(goal)
    }
    pub fn load(&self, id: &str) -> Result<Goal> {
        validate_id(id)?;
        let body = std::fs::read(self.directory.join("goals").join(format!("{id}.json")))
            .with_context(|| format!("read goal {id}"))?;
        let goal: Goal = serde_json::from_slice(&body).context("parse goal record")?;
        goal.validate()?;
        ensure!(
            goal.id == id && goal.workspace == self.workspace,
            "goal identity or workspace does not match"
        );
        Ok(goal)
    }
    pub fn list(&self) -> Result<Vec<Goal>> {
        let entries = match std::fs::read_dir(self.directory.join("goals")) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e).context("list goals"),
        };
        let mut goals = vec![];
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                goals.push(
                    self.load(
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .context("invalid goal filename")?,
                    )?,
                );
            }
        }
        goals.sort_by_key(|goal| goal.created_at);
        Ok(goals)
    }
    pub fn update(&self, id: &str, mutate: impl FnOnce(&mut Goal) -> Result<()>) -> Result<Goal> {
        let (goal, events) = {
            let _transaction = self.lock("state.lock")?;
            let mut goal = self.load(id)?;
            let before = goal.clone();
            mutate(&mut goal)?;
            goal.updated_at = chrono::Utc::now();
            goal.validate()?;
            self.save(&goal)?;
            let events = events::transitions(Some(&before), &goal);
            self.append(&goal.id, &events)?;
            (goal, events)
        };
        events::notify(&goal.spec.objective, &events);
        Ok(goal)
    }
    /// Records an assignment or observation that does not change the goal record.
    pub fn record(&self, id: &str, kind: &str, detail: serde_json::Value) -> Result<()> {
        let goal = self.load(id)?;
        let events = [Event::new(id, kind, detail)];
        self.append(id, &events)?;
        events::notify(&goal.spec.objective, &events);
        Ok(())
    }
    /// Reads a goal's timeline in append order.
    pub fn events(&self, id: &str) -> Result<Vec<Event>> {
        let text = match std::fs::read_to_string(self.events_path(id)?) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(error).context("read goal timeline"),
        };
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).context("parse goal timeline entry"))
            .collect()
    }
    fn events_path(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self
            .directory
            .join("goals")
            .join(format!("{id}.events.ndjson")))
    }
    /// One `write` per batch with O_APPEND keeps concurrent appends whole.
    fn append(&self, id: &str, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let mut text = String::new();
        for event in events {
            text.push_str(&serde_json::to_string(event)?);
            text.push('\n');
        }
        self.initialize()?;
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(self.events_path(id)?)?
            .write_all(text.as_bytes())
            .context("append goal timeline")
    }
    fn save(&self, goal: &Goal) -> Result<()> {
        let directory = self.directory.join("goals");
        let mut file = tempfile::NamedTempFile::new_in(&directory)?;
        serde_json::to_writer_pretty(&mut file, goal)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(directory.join(format!("{}.json", goal.id)))
            .context("replace goal record atomically")?;
        #[cfg(unix)]
        File::open(directory)?.sync_all()?;
        Ok(())
    }
    pub fn run_directory(&self, attempt: &str) -> Result<PathBuf> {
        validate_id(attempt)?;
        self.initialize()?;
        let path = self.directory.join("runs").join(attempt);
        std::fs::create_dir(&path).context("create unique execution directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(path)
    }
    pub fn runs_directory(&self) -> PathBuf {
        self.directory.join("runs")
    }
}

impl Store {
    pub fn private_directory(&self, name: &str) -> Result<PathBuf> {
        validate_id(name)?;
        self.initialize()?;
        let directory = self.directory.join(name);
        std::fs::create_dir_all(&directory)?;
        Ok(directory)
    }
    pub fn write_private(&self, path: &Path, value: &impl serde::Serialize) -> Result<()> {
        let _transaction = self.lock("state.lock")?;
        let directory = path.parent().context("state file needs a directory")?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.as_file().sync_all()?;
        file.persist(path).context("save private state")?;
        Ok(())
    }
    /// Saves the conversation brief. `scope` is the original Herdr terminal, so
    /// managers opened from different tabs of one workspace keep separate briefs.
    pub fn save_brief(&self, scope: Option<&str>, text: &str) -> Result<()> {
        self.write_private(
            &self.private_directory("manager")?.join(brief_file(scope)?),
            &text,
        )
    }
    pub fn brief(&self, scope: Option<&str>) -> Result<String> {
        let path = self.directory.join("manager").join(brief_file(scope)?);
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(error.into()),
        }
    }
}

fn brief_file(scope: Option<&str>) -> Result<String> {
    match scope {
        Some(terminal) => {
            validate_id(terminal)?;
            Ok(format!("brief-{terminal}.json"))
        }
        None => Ok("brief.json".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Attempt, AttemptStatus, GoalStatus};

    fn spec() -> GoalSpec {
        serde_norway::from_str("objective: Review\nacceptance: [Reviewed]\n").unwrap()
    }

    #[test]
    fn transaction_failure_does_not_replace_state_and_other_runners_are_excluded() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        let goal = store.add(spec(), None)?;
        let lease = store.execution_lease()?;
        assert!(
            Store::new(workspace.path(), state.path())?
                .execution_lease()
                .is_err()
        );
        drop(lease);
        let _lease = store.execution_lease()?;
        assert!(
            store
                .update(&goal.id, |g| {
                    g.version = 99;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(store.load(&goal.id)?.version, 1);
        assert!(store.load("../escape").is_err());
        Ok(())
    }

    #[test]
    fn recovery_preserves_attempt_identity_instead_of_silently_replaying() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        let goal = store.add(spec(), None)?;
        let attempt = Attempt::running();
        store.update(&goal.id, |g| {
            g.status = GoalStatus::Running;
            g.plans.push(attempt.clone());
            Ok(())
        })?;
        let fresh_process = Store::new(workspace.path(), state.path())?;
        assert!(
            fresh_process
                .update(&goal.id, |g| g.recover(false))
                .is_err()
        );
        let recovered = fresh_process.update(&goal.id, |g| g.recover(true))?;
        assert_eq!(recovered.plans[0].id, attempt.id);
        assert_eq!(recovered.plans[0].status, AttemptStatus::Interrupted);
        Ok(())
    }
}
