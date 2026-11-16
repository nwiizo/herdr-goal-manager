//! The optional factory queue: user-authored contracts the user agreed to,
//! kept with that agreement until one is turned into a confirmed goal.
//!
//! Everything lives in the private state directory: one immutable file per
//! item (`queue/items/<id>.json`) and an append-only ledger
//! (`queue/ledger.ndjson`) whose entries decide each item's state. Every
//! change holds `queue/claim.lock`. The operating system releases that lock
//! when its holder exits, so the next holder finds a claim the previous one
//! left unfinished and settles it ([`Locked::recover`]): the goal of a claim
//! has the ID `g-<claim>`, so the goal file alone shows whether it was made.
use crate::{
    config::{self, LoopDefaults},
    model::{Control, Evidence, Goal, GoalStatus, validate_id},
    runner,
    store::Store,
};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs::File, path::PathBuf};

/// The lock every queue change holds, relative to the workspace state.
const LOCK: &str = "queue/claim.lock";

/// One queued contract, written once and never changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Item {
    pub version: u32,
    pub id: String,
    pub added_at: DateTime<Utc>,
    /// The original Herdr terminal that queued it. History only: the goal
    /// belongs to the manager that takes the item out.
    pub added_by: Option<String>,
    /// Where the contract was read from, for the person's reference.
    pub source: PathBuf,
    /// The user-authored contract, verbatim; the only source of its commands.
    pub contract_text: String,
    /// `loop` defaults in effect when the contract was queued and validated.
    pub loop_defaults: LoopDefaults,
    /// The user's words agreeing to run this contract.
    pub agreement: Evidence,
}

/// What a ledger entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Added,
    Parked,
    Unparked,
    Removed,
    Claimed,
    Dispatched,
    ClaimAbandoned,
    DispatchFailed,
}

/// One line of the ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub at: DateTime<Utc>,
    pub item: String,
    pub kind: Kind,
    #[serde(default)]
    pub detail: Value,
}

/// An item's state, decided by its ledger entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    Queued,
    Parked,
    Claimed,
    Dispatched,
    /// A dispatch was refused; the person queues it again or removes it.
    Failed,
    Removed,
}
impl State {
    /// The state `kind` leads to from this one, or `None` when it cannot.
    fn after(self, kind: Kind) -> Option<Self> {
        use self::{Kind as K, State as S};
        Some(match (self, kind) {
            (S::Queued, K::Parked) => S::Parked,
            (S::Parked, K::Unparked) | (S::Claimed, K::ClaimAbandoned) => S::Queued,
            (S::Queued | S::Parked | S::Failed, K::Removed) => S::Removed,
            (S::Queued, K::Claimed) => S::Claimed,
            (S::Claimed, K::Dispatched) => S::Dispatched,
            (S::Queued | S::Claimed, K::DispatchFailed) => S::Failed,
            _ => return None,
        })
    }
    fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Parked => "parked",
            Self::Claimed => "claimed",
            Self::Dispatched => "dispatched",
            Self::Failed => "failed",
            Self::Removed => "removed",
        }
    }
}

/// An item as its ledger entries describe it, in queueing order.
#[derive(Debug, Clone)]
pub(crate) struct Status {
    pub id: String,
    pub state: State,
    /// Content fingerprint recorded when the item was queued.
    pub fingerprint: String,
    pub claim: Option<String>,
    pub goal: Option<String>,
    /// Why the last dispatch was refused.
    pub reason: Option<String>,
    pub history: Vec<Entry>,
}

/// Content fingerprint of a parsed contract: SHA-256 of its JSON form. Object
/// keys serialize in sorted order (serde_json without `preserve_order`), so
/// layout, comments and key order do not change it.
pub(crate) fn fingerprint(raw: &Value) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(raw)?)))
}

fn parse_contract(text: &str) -> Result<Value> {
    serde_norway::from_str(text).context("parse contract YAML")
}

/// The goal a claim creates; its file shows whether the claim got that far.
fn goal_id(claim: &str) -> String {
    format!("g-{claim}")
}

/// The branch of the checkout `queue next` makes for item `id`: `hgm/` and
/// the item id up to its first eight hex digits (`hgm/q-1a2b3c4d`).
pub(crate) fn branch_for(id: &str) -> String {
    format!("hgm/{}", &id[..id.len().min(10)])
}

/// Where a factory checkout starts: the workspace's current branch, or its
/// commit when HEAD is detached. A repository without a commit has none.
fn current_base(workspace: &std::path::Path) -> Result<String> {
    let git = |args: &[&str]| -> Result<String> {
        let output = std::process::Command::new("git")
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .current_dir(workspace)
            .output()
            .context("run git")?;
        ensure!(
            output.status.success(),
            "git {} failed in {}: {}",
            args.join(" "),
            workspace.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"])
        .context("the factory checkout needs a commit to start from")?;
    if branch == "HEAD" {
        git(&["rev-parse", "HEAD"])
    } else {
        Ok(branch)
    }
}

/// Folds ledger entries into item states; an impossible step is refused.
fn fold(entries: &[Entry]) -> Result<Vec<Status>> {
    let mut items: Vec<Status> = vec![];
    for (index, entry) in entries.iter().enumerate() {
        let line = index + 1;
        let text = |key: &str| entry.detail[key].as_str().map(String::from);
        let position = items.iter().position(|s| s.id == entry.item);
        let Some(position) = position else {
            ensure!(
                entry.kind == Kind::Added,
                "queue ledger line {line}: {:?} for unknown item {}",
                entry.kind,
                entry.item
            );
            items.push(Status {
                id: entry.item.clone(),
                state: State::Queued,
                fingerprint: text("fingerprint").with_context(|| {
                    format!("queue ledger line {line}: added without a fingerprint")
                })?,
                claim: None,
                goal: None,
                reason: None,
                history: vec![entry.clone()],
            });
            continue;
        };
        let status = &mut items[position];
        status.state = status.state.after(entry.kind).with_context(|| {
            format!(
                "queue ledger line {line}: {:?} cannot follow {} for item {}",
                entry.kind,
                status.state.name(),
                entry.item
            )
        })?;
        match entry.kind {
            Kind::Claimed => status.claim = text("claim"),
            Kind::ClaimAbandoned => status.claim = None,
            Kind::Dispatched => status.goal = text("goal"),
            Kind::DispatchFailed => {
                status.goal = text("goal");
                status.reason = text("reason");
            }
            _ => {}
        }
        status.history.push(entry.clone());
    }
    Ok(items)
}

/// The queue of one workspace, read without the lock.
pub(crate) struct Queue<'a> {
    store: &'a Store,
    directory: PathBuf,
}
impl<'a> Queue<'a> {
    /// Opens the queue directory. Callers check `factory.enabled` first.
    pub fn open(store: &'a Store) -> Result<Self> {
        let directory = store.private_directory("queue")?;
        std::fs::create_dir_all(directory.join("items"))?;
        Ok(Self { store, directory })
    }
    fn ledger_path(&self) -> PathBuf {
        self.directory.join("ledger.ndjson")
    }
    fn item_path(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.directory.join("items").join(format!("{id}.json")))
    }
    /// Ledger entries in append order. A line that does not parse, or a last
    /// line without its newline (a torn write), stops every queue command:
    /// nothing is skipped silently.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let path = self.ledger_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let repair = || {
            format!(
                "inspect or repair {} before using the queue; nothing was changed",
                path.display()
            )
        };
        ensure!(
            text.is_empty() || text.ends_with('\n'),
            "the last line of the queue ledger is incomplete; {}",
            repair()
        );
        text.lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(|(index, line)| {
                serde_json::from_str(line).with_context(|| {
                    format!(
                        "queue ledger line {} is unreadable; {}",
                        index + 1,
                        repair()
                    )
                })
            })
            .collect()
    }
    pub fn statuses(&self) -> Result<Vec<Status>> {
        fold(&self.entries()?)
    }
    pub fn status(&self, id: &str) -> Result<Status> {
        validate_id(id)?;
        self.statuses()?
            .into_iter()
            .find(|status| status.id == id)
            .with_context(|| format!("unknown queue item {id}"))
    }
    pub fn item(&self, id: &str) -> Result<Item> {
        let path = self.item_path(id)?;
        let body = std::fs::read(&path).with_context(|| format!("read queue item {id}"))?;
        let item: Item = serde_json::from_slice(&body).context("parse queue item")?;
        ensure!(
            item.version == 1 && item.id == id,
            "queue item {} does not match its file",
            path.display()
        );
        Ok(item)
    }
    /// What `queue list` shows for one item, with the agreement's age.
    pub fn view(&self, status: &Status, now: DateTime<Utc>) -> Result<Value> {
        let item = self.item(&status.id)?;
        let objective = parse_contract(&item.contract_text)
            .ok()
            .and_then(|raw| raw["objective"].as_str().map(String::from));
        let minutes = (now - item.agreement.recorded_at).num_minutes().max(0);
        // Where the item's goal works, when it has its own checkout.
        let checkout = status
            .goal
            .as_deref()
            .and_then(|goal| self.store.load(goal).ok())
            .and_then(|goal| goal.checkout);
        let mut view = json!({
            "id": status.id,
            "state": status.state,
            "objective": objective,
            "source": item.source,
            "added_at": item.added_at,
            "added_by": item.added_by,
            "agreement": item.agreement.note,
            "agreed_at": item.agreement.recorded_at,
            "agreement_age": age(minutes),
            "agreement_age_minutes": minutes,
            "fingerprint": status.fingerprint,
            "goal": status.goal,
            "reason": status.reason,
            "history": status.history,
        });
        if let Some(checkout) = checkout {
            view["checkout"] = json!({"path": checkout.path, "branch": checkout.branch,
                                     "removed_at": checkout.removed_at});
        }
        Ok(view)
    }
    /// Takes the queue lock, waiting briefly for another queue command.
    pub fn lock(self) -> Result<Locked<'a>> {
        let lock = self.store.wait_lock(LOCK)?;
        Ok(Locked {
            queue: self,
            _lock: lock,
        })
    }
}

/// Minutes as a short age: `45m`, `5h`, `3d`.
pub(crate) fn age(minutes: i64) -> String {
    match minutes {
        ..60 => format!("{minutes}m"),
        60..2880 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    }
}

/// The queue with its lock held; every change goes through here.
pub(crate) struct Locked<'a> {
    queue: Queue<'a>,
    _lock: File,
}
impl<'a> std::ops::Deref for Locked<'a> {
    type Target = Queue<'a>;
    fn deref(&self) -> &Queue<'a> {
        &self.queue
    }
}
impl Locked<'_> {
    fn append(&self, item: &str, kind: Kind, detail: Value) -> Result<()> {
        let entry = Entry {
            at: Utc::now(),
            item: item.into(),
            kind,
            detail,
        };
        self.store.append_private(&self.ledger_path(), &[entry])
    }
    /// Checks that `kind` may follow the item's state, then records it.
    fn step(&self, id: &str, kind: Kind, detail: Value) -> Result<Status> {
        let status = self.status(id)?;
        if status.state.after(kind).is_none() {
            let hint = match status.state {
                State::Dispatched => format!(
                    "; it became goal {}: pause or cancel that goal instead",
                    status.goal.as_deref().unwrap_or("?")
                ),
                State::Claimed => "; a dispatch is settling it".into(),
                State::Removed => "; queue the contract again with hgm queue add".into(),
                State::Failed => {
                    "; queue the corrected contract again with hgm queue add, or remove it".into()
                }
                _ => String::new(),
            };
            bail!("queue item {id} is {}{hint}", status.state.name());
        }
        self.append(id, kind, detail)?;
        self.status(id)
    }

    /// Queues a user-authored contract with the user's agreement. The contract
    /// must pass goal validation and confirmation (an `auto` loop needs a
    /// command check), and the same content may not run twice at once: an
    /// item that is queued, parked or claimed, or dispatched to a goal that is
    /// not completed or cancelled, refuses it. A removed or failed item, or one
    /// whose goal finished, may be queued again with a new agreement.
    pub fn add(
        &self,
        contract_text: String,
        source: PathBuf,
        evidence: &str,
        added_by: Option<String>,
        loop_defaults: LoopDefaults,
    ) -> Result<Status> {
        ensure!(
            !evidence.trim().is_empty(),
            "queueing a contract needs the user's agreement; quote their words with --evidence"
        );
        let raw = parse_contract(&contract_text)?;
        let fingerprint = fingerprint(&raw)?;
        let spec = config::goal_spec_with(raw.clone(), &loop_defaults)?;
        Goal::new(spec, self.store.workspace.clone())?.confirm(evidence.into(), String::new())?;
        for other in self
            .statuses()?
            .iter()
            .filter(|s| s.fingerprint == fingerprint)
        {
            match other.state {
                State::Queued | State::Parked | State::Claimed => bail!(
                    "the same contract is already {} as queue item {}; park, remove or dispatch that item instead of queueing it twice",
                    other.state.name(),
                    other.id
                ),
                State::Dispatched => {
                    let goal = other
                        .goal
                        .as_deref()
                        .context("dispatched item without a goal")?;
                    let status = self.store.load(goal)?.status;
                    ensure!(
                        matches!(status, GoalStatus::Completed | GoalStatus::Cancelled),
                        "the same contract is running as goal {goal} (queue item {}); accept or cancel that goal before queueing it again",
                        other.id
                    );
                }
                State::Failed | State::Removed => {}
            }
        }
        let now = Utc::now();
        let item = Item {
            version: 1,
            id: format!("q-{}", uuid::Uuid::new_v4()),
            added_at: now,
            added_by,
            source,
            contract_text,
            loop_defaults,
            agreement: Evidence {
                note: evidence.into(),
                recorded_at: now,
            },
        };
        // The item file comes first: a stop before the ledger line leaves an
        // unlisted file, never a listed item without its contract.
        self.store
            .write_private(&self.item_path(&item.id)?, &item)?;
        self.append(
            &item.id,
            Kind::Added,
            json!({"fingerprint": fingerprint, "objective": raw["objective"]}),
        )?;
        self.status(&item.id)
    }
    /// Holds a queued item back from dispatch; its history stays.
    pub fn park(&self, id: &str, reason: Option<&str>) -> Result<Status> {
        self.step(id, Kind::Parked, json!({"reason": reason}))
    }
    pub fn unpark(&self, id: &str) -> Result<Status> {
        self.step(id, Kind::Unparked, json!({}))
    }
    /// Takes a queued, parked or failed item off the queue; its file and
    /// history stay.
    pub fn remove(&self, id: &str) -> Result<Status> {
        self.step(id, Kind::Removed, json!({}))
    }

    /// Turns a queued item into a confirmed goal owned by `owner` (the
    /// original terminal of the manager taking it out). The stored contract
    /// must still match the fingerprint recorded when it was queued; the
    /// stored agreement becomes the confirmation evidence. Any refusal is
    /// recorded as `dispatch_failed`, so the item leaves the queue with its
    /// reason instead of blocking the items behind it.
    /// With `own_checkout`, a contract that names no checkout gets one on the
    /// branch [`branch_for`] names, from the workspace's current branch; the
    /// confirmation creates it, so a failure fails the item like any refusal.
    pub async fn dispatch(
        &self,
        id: &str,
        owner: Option<String>,
        own_checkout: bool,
    ) -> Result<(Item, Goal)> {
        let status = self.status(id)?;
        ensure!(
            status.state == State::Queued,
            "queue item {id} is {}, not queued",
            status.state.name()
        );
        let item = self.item(id)?;
        let spec = parse_contract(&item.contract_text).and_then(|raw| {
            ensure!(
                fingerprint(&raw)? == status.fingerprint,
                "the stored contract of {id} no longer matches what was agreed (its fingerprint changed since it was queued); queue the contract again with a new agreement"
            );
            let mut spec = config::goal_spec_with(raw, &item.loop_defaults)?;
            if own_checkout && spec.checkout.is_none() {
                spec.checkout = Some(crate::model::CheckoutSpec {
                    repository: None,
                    base: current_base(&self.store.workspace)?,
                    branch: branch_for(id),
                    path: None,
                });
            }
            Ok(spec)
        });
        let spec = match spec {
            Ok(spec) => spec,
            Err(error) => return Err(self.fail(id, None, None, error)),
        };
        let claim = uuid::Uuid::new_v4().to_string();
        self.append(id, Kind::Claimed, json!({"claim": claim}))?;
        let goal = goal_id(&claim);
        if let Err(error) = self.store.add_with_id(spec, owner, &goal) {
            return Err(self.fail(id, Some(&claim), None, error));
        }
        let goal = self.finish(&item, &claim, &goal).await?;
        Ok((item, goal))
    }
    /// Confirms a claimed item's goal with the stored agreement and records
    /// the outcome.
    async fn finish(&self, item: &Item, claim: &str, goal: &str) -> Result<Goal> {
        let evidence = format!(
            "Queued agreement {} ({}): {}",
            item.id,
            item.agreement.recorded_at.format("%Y-%m-%d %H:%M UTC"),
            item.agreement.note
        );
        match runner::confirm_goal(self.store, goal, &evidence).await {
            Ok(confirmed) => {
                self.append(
                    &item.id,
                    Kind::Dispatched,
                    json!({"claim": claim, "goal": goal}),
                )?;
                Ok(confirmed)
            }
            Err(error) => Err(self.fail(&item.id, Some(claim), Some(goal), error)),
        }
    }
    /// Records a refused dispatch; the returned error says what happened. A
    /// goal already registered for it is cancelled with the reason: its
    /// record stays as evidence and it no longer waits to be started.
    fn fail(
        &self,
        id: &str,
        claim: Option<&str>,
        goal: Option<&str>,
        error: anyhow::Error,
    ) -> anyhow::Error {
        let reason = format!("{error:#}");
        if let Err(record) = self.append(
            id,
            Kind::DispatchFailed,
            json!({"claim": claim, "goal": goal, "reason": reason}),
        ) {
            return record.context(format!(
                "dispatch of {id} failed ({reason}) and could not be recorded"
            ));
        }
        let kept = match goal {
            None => String::new(),
            Some(goal) => match self.store.update(goal, |g| {
                g.message = Some(format!(
                    "Cancelled: dispatch of queue item {id} failed: {reason}"
                ));
                g.request(Control::Cancel)
            }) {
                Ok(_) => format!("; its unconfirmed goal {goal} is cancelled and kept as evidence"),
                Err(cancel) => {
                    format!("; its unconfirmed goal {goal} could not be cancelled ({cancel:#})")
                }
            },
        };
        error.context(format!(
            "queue item {id} was not dispatched and is marked failed{kept}; queue the corrected contract again or remove it"
        ))
    }

    /// Settles claims a stopped holder left: a claim without a goal returns
    /// its item to the queue; a goal not yet confirmed is confirmed with the
    /// stored agreement (or the item fails with the reason); a confirmed goal
    /// is recorded as dispatched. Returns one line per settled item.
    pub async fn recover(&self) -> Result<Vec<String>> {
        let mut notes = vec![];
        for status in self.statuses()? {
            if status.state != State::Claimed {
                continue;
            }
            let claim = status
                .claim
                .as_deref()
                .context("claimed item without a claim")?;
            let goal = goal_id(claim);
            if !self.store.goal_exists(&goal)? {
                self.append(
                    &status.id,
                    Kind::ClaimAbandoned,
                    json!({"claim": claim, "reason": "the claimer stopped before registering a goal"}),
                )?;
                notes.push(format!(
                    "Queue item {} returned to the queue: an earlier dispatch stopped before creating a goal.",
                    status.id
                ));
            } else if self.store.load(&goal)?.confirmed.is_none() {
                let item = self.item(&status.id)?;
                notes.push(match self.finish(&item, claim, &goal).await {
                    Ok(_) => format!(
                        "Queue item {} became goal {goal}: an earlier dispatch stopped before confirming it.",
                        status.id
                    ),
                    Err(error) => format!("{error:#}"),
                });
            } else {
                self.append(
                    &status.id,
                    Kind::Dispatched,
                    json!({"claim": claim, "goal": goal, "recovered": true}),
                )?;
                notes.push(format!(
                    "Queue item {} is goal {goal}: an earlier dispatch stopped before recording it.",
                    status.id
                ));
            }
        }
        Ok(notes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTRACT: &str = "objective: Write summary.json\nacceptance: [summary.json has the totals]\nchecks:\n  - name: summary\n    command: test -f summary.json\n";

    struct Fixture {
        _workspace: tempfile::TempDir,
        _state: tempfile::TempDir,
        store: Store,
    }
    fn fixture() -> Result<Fixture> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        Ok(Fixture {
            _workspace: workspace,
            _state: state,
            store,
        })
    }
    fn add(store: &Store, text: &str) -> Result<Status> {
        Queue::open(store)?.lock()?.add(
            text.into(),
            "/contracts/goal.yaml".into(),
            "Yes, queue it",
            Some("terminal-a".into()),
            LoopDefaults::default(),
        )
    }
    fn state(store: &Store, id: &str) -> Result<State> {
        Ok(Queue::open(store)?.status(id)?.state)
    }

    #[test]
    fn fingerprint_ignores_layout_comments_and_key_order() -> Result<()> {
        let same = "# reordered\nchecks: [{command: test -f summary.json, name: summary}]\nacceptance:\n  - summary.json has the totals\nobjective: Write summary.json\n";
        let print = |text: &str| fingerprint(&parse_contract(text)?);
        assert_eq!(print(CONTRACT)?, print(same)?);
        assert_ne!(
            print(CONTRACT)?,
            print(&CONTRACT.replace("totals", "total"))?
        );
        Ok(())
    }

    #[test]
    fn add_refuses_missing_agreement_invalid_contracts_and_a_second_live_copy() -> Result<()> {
        let f = fixture()?;
        let locked = Queue::open(&f.store)?.lock()?;
        let add = |text: &str, evidence: &str| {
            locked.add(
                text.into(),
                "/c.yaml".into(),
                evidence,
                None,
                LoopDefaults::default(),
            )
        };
        let error = add(CONTRACT, "  ").unwrap_err().to_string();
        assert!(error.contains("--evidence"), "{error}");
        assert!(add("objective: No acceptance\n", "yes").is_err());
        let error = add("objective: o\nacceptance: [a]\n", "yes")
            .unwrap_err()
            .to_string();
        assert!(error.contains("command check"), "{error}");
        let first = add(CONTRACT, "yes")?;
        let reordered = "acceptance: [summary.json has the totals]\nobjective: Write summary.json\nchecks: [{name: summary, command: test -f summary.json}]\n";
        let error = add(reordered, "yes").unwrap_err().to_string();
        assert!(
            error.contains(&first.id) && error.contains("queued"),
            "{error}"
        );
        locked.park(&first.id, None)?;
        assert!(
            add(CONTRACT, "yes")
                .unwrap_err()
                .to_string()
                .contains("parked")
        );
        locked.remove(&first.id)?;
        let again = add(CONTRACT, "yes again")?;
        assert_ne!(again.id, first.id);
        assert!(f.store.list()?.is_empty(), "queueing creates no goal");
        Ok(())
    }

    #[test]
    fn park_unpark_and_remove_follow_the_ledger_and_keep_history() -> Result<()> {
        let f = fixture()?;
        let id = add(&f.store, CONTRACT)?.id;
        let locked = Queue::open(&f.store)?.lock()?;
        assert_eq!(
            locked.park(&id, Some("wait for the API"))?.state,
            State::Parked
        );
        assert!(
            locked
                .park(&id, None)
                .unwrap_err()
                .to_string()
                .contains("is parked")
        );
        assert_eq!(locked.unpark(&id)?.state, State::Queued);
        let removed = locked.remove(&id)?;
        assert_eq!(removed.state, State::Removed);
        let kinds: Vec<_> = removed.history.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [Kind::Added, Kind::Parked, Kind::Unparked, Kind::Removed]
        );
        assert_eq!(removed.history[1].detail["reason"], "wait for the API");
        let item = locked.item(&id)?;
        assert_eq!(item.added_by.as_deref(), Some("terminal-a"));
        assert!(
            locked
                .unpark(&id)
                .unwrap_err()
                .to_string()
                .contains("removed")
        );
        Ok(())
    }

    #[test]
    fn the_ledger_refuses_an_impossible_step_and_a_torn_line() -> Result<()> {
        let f = fixture()?;
        let id = add(&f.store, CONTRACT)?.id;
        let queue = Queue::open(&f.store)?;
        let ledger = queue.ledger_path();
        let good = std::fs::read_to_string(&ledger)?;
        std::fs::write(&ledger, format!("{good}{{\"at\":\"2026-11-15T00:00:00Z\""))?;
        let error = queue.statuses().unwrap_err().to_string();
        assert!(error.contains("incomplete"), "{error}");
        let dispatched =
            json!({"at":"2026-11-15T00:00:00Z","item":id,"kind":"dispatched","detail":{}});
        std::fs::write(&ledger, format!("{good}{dispatched}\n"))?;
        let error = format!("{:#}", queue.statuses().unwrap_err());
        assert!(
            error.contains("line 2") && error.contains("cannot follow queued"),
            "{error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dispatch_confirms_a_goal_for_the_taking_manager_with_the_stored_agreement()
    -> Result<()> {
        let f = fixture()?;
        let id = add(&f.store, CONTRACT)?.id;
        let locked = Queue::open(&f.store)?.lock()?;
        let (item, goal) = locked
            .dispatch(&id, Some("terminal-b".into()), false)
            .await?;
        assert_eq!(
            goal.owner.as_deref(),
            Some("terminal-b"),
            "R1: not added_by"
        );
        assert_eq!(item.added_by.as_deref(), Some("terminal-a"));
        let confirmed = goal.confirmed.context("confirmed")?.note;
        assert!(
            confirmed.contains("Yes, queue it") && confirmed.contains(&id),
            "{confirmed}"
        );
        assert_eq!(
            goal.status,
            GoalStatus::Ready,
            "never completed by dispatch"
        );
        assert!(goal.acceptance.is_none());
        let status = locked.status(&id)?;
        assert_eq!(status.state, State::Dispatched);
        assert_eq!(status.goal.as_deref(), Some(goal.id.as_str()));
        let again = locked.add(
            CONTRACT.into(),
            "/c.yaml".into(),
            "again",
            None,
            LoopDefaults::default(),
        );
        assert!(
            again.unwrap_err().to_string().contains(&goal.id),
            "R5: a live goal blocks a second copy"
        );
        f.store
            .update(&goal.id, |g| g.request(crate::model::Control::Cancel))?;
        locked.add(
            CONTRACT.into(),
            "/c.yaml".into(),
            "again",
            None,
            LoopDefaults::default(),
        )?;
        Ok(())
    }

    #[tokio::test]
    async fn a_refused_confirmation_fails_the_item_and_the_next_one_still_dispatches() -> Result<()>
    {
        let f = fixture()?;
        let not_git = tempfile::tempdir()?;
        let with_checkout = format!(
            "{CONTRACT}checkout: {{repository: '{}', base: main, branch: goal/x}}\n",
            not_git.path().display()
        );
        let refused = add(&f.store, &with_checkout)?.id;
        let next = add(&f.store, CONTRACT)?.id;
        let locked = Queue::open(&f.store)?.lock()?;
        let error = format!(
            "{:#}",
            locked.dispatch(&refused, None, false).await.unwrap_err()
        );
        assert!(
            error.contains("not a Git repository") && error.contains("marked failed"),
            "{error}"
        );
        let status = locked.status(&refused)?;
        assert_eq!(status.state, State::Failed);
        assert!(
            status
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("not a Git repository"))
        );
        let kept = status.goal.context("the unconfirmed goal is kept")?;
        let kept = f.store.load(&kept)?;
        assert!(kept.confirmed.is_none());
        assert_eq!(
            kept.status,
            GoalStatus::Cancelled,
            "not left waiting to start"
        );
        assert!(kept.message.is_some_and(|m| m.contains(&refused)));
        assert!(locked.recover().await?.is_empty(), "nothing is retried");
        locked.dispatch(&next, None, false).await?;
        assert_eq!(locked.status(&next)?.state, State::Dispatched);
        assert_eq!(locked.remove(&refused)?.state, State::Removed);
        Ok(())
    }

    #[tokio::test]
    async fn a_changed_stored_contract_is_refused_before_claiming() -> Result<()> {
        let f = fixture()?;
        let id = add(&f.store, CONTRACT)?.id;
        let locked = Queue::open(&f.store)?.lock()?;
        let mut item = locked.item(&id)?;
        item.contract_text = CONTRACT.replace("summary.json has", "summary.json may have");
        std::fs::write(locked.item_path(&id)?, serde_json::to_vec(&item)?)?;
        let error = format!("{:#}", locked.dispatch(&id, None, false).await.unwrap_err());
        assert!(
            error.contains("no longer matches what was agreed"),
            "{error}"
        );
        let status = locked.status(&id)?;
        assert_eq!((status.state, status.claim), (State::Failed, None));
        assert!(f.store.list()?.is_empty(), "no goal for a changed contract");
        Ok(())
    }

    #[tokio::test]
    async fn recovery_settles_each_way_a_claimer_can_stop() -> Result<()> {
        let f = fixture()?;
        let locked = Queue::open(&f.store)?.lock()?;
        let claim = |text: &str| -> Result<(String, String)> {
            let id = locked
                .add(
                    text.into(),
                    "/c.yaml".into(),
                    "Yes, queue it",
                    None,
                    LoopDefaults::default(),
                )?
                .id;
            let claim = uuid::Uuid::new_v4().to_string();
            locked.append(&id, Kind::Claimed, json!({"claim": claim}))?;
            Ok((id, claim))
        };
        let spec = || config::goal_spec_with(parse_contract(CONTRACT)?, &LoopDefaults::default());
        // Stopped before registering a goal.
        let (none, _) = claim(CONTRACT)?;
        // Stopped after registering, before confirming.
        let (unconfirmed, c2) = claim(&CONTRACT.replace("totals", "sums"))?;
        f.store.add_with_id(spec()?, None, &goal_id(&c2))?;
        // Stopped after confirming, before recording the dispatch.
        let (confirmed, c3) = claim(&CONTRACT.replace("totals", "counts"))?;
        f.store.add_with_id(spec()?, None, &goal_id(&c3))?;
        runner::confirm_goal(&f.store, &goal_id(&c3), "yes").await?;
        assert_eq!(locked.recover().await?.len(), 3);
        assert_eq!(state(&f.store, &none)?, State::Queued);
        assert_eq!(state(&f.store, &unconfirmed)?, State::Dispatched);
        let goal = f.store.load(&goal_id(&c2))?;
        assert!(
            goal.confirmed
                .is_some_and(|c| c.note.contains("Yes, queue it"))
        );
        assert_eq!(state(&f.store, &confirmed)?, State::Dispatched);
        assert_eq!(f.store.list()?.len(), 2, "recovery never adds a goal");
        assert!(locked.recover().await?.is_empty());
        Ok(())
    }

    #[test]
    fn two_claimers_dispatch_one_item_once() -> Result<()> {
        let f = fixture()?;
        let id = add(&f.store, CONTRACT)?.id;
        let (workspace, root) = (f.store.workspace.clone(), f.store.root.clone());
        // Both claimers wait on the held lock, then race for the one item.
        let held = f.store.wait_lock(LOCK)?;
        let claimers: Vec<_> = (0..2)
            .map(|_| {
                let (workspace, root) = (workspace.clone(), root.clone());
                std::thread::spawn(move || -> Result<Option<String>> {
                    let store = Store::new(&workspace, &root)?;
                    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
                    runtime.block_on(async {
                        let locked = Queue::open(&store)?.lock()?;
                        locked.recover().await?;
                        let next = locked
                            .statuses()?
                            .into_iter()
                            .find(|s| s.state == State::Queued);
                        Ok(match next {
                            Some(status) => {
                                Some(locked.dispatch(&status.id, None, false).await?.1.id)
                            }
                            None => None,
                        })
                    })
                })
            })
            .collect();
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(held);
        let results: Vec<_> = claimers
            .into_iter()
            .map(|c| c.join().expect("claimer thread"))
            .collect::<Result<_>>()?;
        assert_eq!(results.iter().flatten().count(), 1, "{results:?}");
        assert_eq!(f.store.list()?.len(), 1);
        let dispatched = Queue::open(&f.store)?
            .status(&id)?
            .history
            .iter()
            .filter(|e| e.kind == Kind::Dispatched)
            .count();
        assert_eq!(dispatched, 1);
        Ok(())
    }
}
