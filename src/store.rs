use crate::events::{self, Event};
use crate::model::{Goal, GoalSpec, validate_id};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) const LOCK_WAIT: Duration = Duration::from_secs(10);
pub(crate) const LOCK_RETRY: Duration = Duration::from_millis(50);
const EXECUTION_WAIT: Duration = Duration::from_secs(1);

pub(crate) struct Store {
    pub workspace: PathBuf,
    pub root: PathBuf,
    directory: PathBuf,
}
impl Store {
    pub fn new(workspace: &Path, root: &Path) -> Result<Self> {
        let workspace = workspace.canonicalize().context("resolve workspace")?;
        ensure!(workspace.is_dir(), "workspace must be a directory");
        let project = workspace
            .ancestors()
            .find(|p| p.join(".git").exists())
            .unwrap_or(&workspace);
        let resolved = resolved_root(root)?;
        let workspaces = resolved_root(&resolved.join("workspaces"))?;
        ensure!(
            !resolved.starts_with(project)
                && !workspaces.starts_with(project)
                && !project.starts_with(&workspaces),
            "state directory must be outside the workspace and its repository: {}",
            root.display()
        );
        let key = format!(
            "{:x}",
            Sha256::digest(workspace.as_os_str().as_encoded_bytes())
        );
        let root = if root
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            resolved
        } else {
            root.to_path_buf()
        };
        Ok(Self {
            directory: root.join("workspaces").join(key),
            workspace,
            root,
        })
    }
    fn initialize(&self) -> Result<()> {
        let mut root = std::fs::DirBuilder::new();
        root.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut root, 0o700);
        root.create(&self.root)?;
        for directory in [
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
    fn open_lock(&self, name: &str) -> Result<File> {
        self.initialize()?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Ok(options.open(self.directory.join(name))?)
    }
    fn transaction(&self) -> Result<File> {
        self.wait_lock("state.lock")
    }
    pub(crate) fn wait_lock(&self, name: &str) -> Result<File> {
        self.wait_lock_for(name, LOCK_WAIT)
    }
    fn wait_lock_for(&self, name: &str, wait: Duration) -> Result<File> {
        let file = self.open_lock(name)?;
        let deadline = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(LOCK_RETRY);
                }
                Err(TryLockError::WouldBlock) => bail!(
                    "workspace manager stayed busy for {}s ({name} is held); nothing was changed; retry after the active operation finishes",
                    wait.as_secs_f32()
                ),
                Err(TryLockError::Error(error)) => {
                    return Err(error).context(format!("lock {name}"));
                }
            }
        }
    }
    /// Acquires the execution lease, retrying briefly because a forked child
    /// can retain the previous runner's lock until exec.
    pub fn execution_lease(&self) -> Result<File> {
        self.wait_lock_for("execution.lock", EXECUTION_WAIT)
    }
    pub(crate) fn contract_lease(&self, id: &str) -> Result<File> {
        validate_id(id)?;
        self.wait_lock(&format!("{id}-contract.lock"))
    }
    /// Registers a goal. `owner` is the original Herdr terminal of the manager.
    pub fn add(&self, spec: GoalSpec, owner: Option<String>) -> Result<Goal> {
        self.register(Goal::new(spec, self.workspace.clone())?, owner)
    }
    /// Registers a goal under a chosen ID, refusing one that already exists.
    /// A caller that derives the ID from its own record (a queue claim) can
    /// later tell from the goal file alone whether registration happened.
    pub fn add_with_id(&self, spec: GoalSpec, owner: Option<String>, id: &str) -> Result<Goal> {
        validate_id(id)?;
        let mut goal = Goal::new(spec, self.workspace.clone())?;
        goal.id = id.into();
        self.register(goal, owner)
    }
    fn register(&self, mut goal: Goal, owner: Option<String>) -> Result<Goal> {
        goal.owner = owner;
        let events = events::transitions(None, &goal);
        {
            let _transaction = self.transaction()?;
            ensure!(
                !self.goal_exists(&goal.id)?,
                "goal {} already exists",
                goal.id
            );
            self.save(&goal)?;
            self.append(&goal.id, &events)?;
        }
        events::notify(&goal.spec.objective, &events);
        Ok(goal)
    }
    /// Whether a goal record with this ID exists in the workspace.
    pub fn goal_exists(&self, id: &str) -> Result<bool> {
        validate_id(id)?;
        Ok(self
            .directory
            .join("goals")
            .join(format!("{id}.json"))
            .try_exists()?)
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
            let _transaction = self.transaction()?;
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
    /// Replaces a draft contract while preserving the goal's ID, owner and timeline.
    pub fn replace_contract(&self, id: &str, spec: GoalSpec) -> Result<Goal> {
        let _contract = self.contract_lease(id)?;
        let words = spec.words();
        let goal = self.update(id, |goal| {
            ensure!(
                goal.confirmed.is_none(),
                "goal {id} is confirmed, so its contract stays as the user agreed it; nothing was replaced. If the user agrees to a new contract, cancel this goal and register a new one"
            );
            ensure!(
                !matches!(
                    goal.status,
                    crate::model::GoalStatus::Completed | crate::model::GoalStatus::Cancelled
                ),
                "goal {id} is {:?}; nothing was replaced, register a new goal instead",
                goal.status
            );
            let fresh = Goal::new(spec, goal.workspace.clone())?;
            ensure!(goal.status != crate::model::GoalStatus::Running
                && goal.plans.is_empty() && goal.verifications.is_empty() && goal.repairs.is_empty()
                && goal.tasks.iter().all(|t| t.attempts.is_empty() && t.manual_evidence.is_none() && t.superseded_evidence.is_empty()),
                "goal {id} has started work; register a new goal to preserve its history");
            goal.spec = fresh.spec;
            goal.tasks = fresh.tasks;
            Ok(())
        })?;
        self.record(
            id,
            "contract_replaced",
            serde_json::json!({"objective": goal.spec.objective, "words": words}),
        )?;
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
    fn append(&self, id: &str, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        self.initialize()?;
        append_lines(&self.events_path(id)?, events).context("append goal timeline")
    }
    pub(crate) fn append_private<T: serde::Serialize>(
        &self,
        path: &Path,
        values: &[T],
    ) -> Result<()> {
        ensure!(
            path.starts_with(&self.directory),
            "append-only state must stay in the private state directory"
        );
        append_lines(path, values).with_context(|| format!("append {}", path.display()))
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
    /// Where the run `attempt` keeps its files, without creating it.
    pub fn run_path(&self, attempt: &str) -> Result<PathBuf> {
        validate_id(attempt)?;
        Ok(self.directory.join("runs").join(attempt))
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
        let _transaction = self.transaction()?;
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

fn append_lines<T: serde::Serialize>(path: &Path, values: &[T]) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    let mut text = String::new();
    for value in values {
        text.push_str(&serde_json::to_string(value)?);
        text.push('\n');
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(text.as_bytes())?;
    Ok(())
}

fn resolved_root(root: &Path) -> Result<PathBuf> {
    let mut resolved = PathBuf::new();
    for component in std::path::absolute(root)?.components() {
        if component == std::path::Component::ParentDir {
            resolved.pop();
            continue;
        }
        resolved.push(component);
        match resolved.symlink_metadata() {
            Ok(_) => resolved = resolved.canonicalize()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("resolve state directory"),
        }
    }
    Ok(resolved)
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
    fn state_is_resolved_outside_the_repository_before_creation() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path().join("repo");
        let workspace = repo.join("project");
        std::fs::create_dir_all(&workspace)?;
        std::fs::create_dir(repo.join(".git"))?;
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&repo, &alias)?;
        for root in [&workspace, &repo.join("state"), &alias.join("new/state")] {
            assert!(Store::new(&workspace, root).is_err());
        }
        assert!(!repo.join("new").exists());
        Ok(())
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
    fn a_briefly_held_execution_lease_is_waited_for_and_a_held_one_still_excludes() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        let held = store.open_lock("execution.lock")?;
        held.try_lock()?;
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(held);
        });
        let start = Instant::now();
        let lease = store.execution_lease()?;
        assert!(start.elapsed() >= Duration::from_millis(250));
        release.join().expect("holder thread");
        let start = Instant::now();
        let other = Store::new(workspace.path(), state.path())?.execution_lease();
        assert!(format!("{:#}", other.unwrap_err()).contains("execution.lock is held"));
        assert!(start.elapsed() >= EXECUTION_WAIT);
        drop(lease);
        Ok(())
    }

    #[test]
    fn a_transaction_waits_for_a_short_one_in_another_process() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        let held = store.open_lock("state.lock")?;
        held.try_lock()?;
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(held);
        });
        let start = Instant::now();
        store.add(spec(), None)?;
        assert!(start.elapsed() >= Duration::from_millis(250));
        release.join().expect("holder thread");
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
