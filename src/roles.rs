//! Helper agents next to main: `impl` takes a split of the work and reviewers
//! judge results independently. Each role is a pinned Herdr agent in a tab the
//! manager owns; reviews are recorded apart from implementation.
use crate::{
    herdr_cli as herdr,
    manager::{Provider, codex_environment, manager_path, quote},
    store::Store,
    supervision::{Target, args, live_pane, operation_lease, path, refusal, screen, session},
};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

const IMPL_GUIDE: &str = include_str!("../prompts/impl.md");
const REVIEWER_GUIDE: &str = include_str!("../prompts/reviewer.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RoleKind {
    /// Implements tasks assigned to it, alongside main.
    Impl,
    /// Reviews results against the contract without changing files.
    Reviewer,
}
impl RoleKind {
    fn default_name(self) -> &'static str {
        match self {
            Self::Impl => "impl",
            Self::Reviewer => "reviewer",
        }
    }
    pub(crate) fn guide(self) -> &'static str {
        match self {
            Self::Impl => IMPL_GUIDE,
            Self::Reviewer => REVIEWER_GUIDE,
        }
    }
    /// The only hgm commands an agent in this role may run.
    pub(crate) fn commands(self) -> &'static [&'static str] {
        match self {
            Self::Impl => &["show", "report", "decision", "follow-up"],
            Self::Reviewer => &["show", "review-submit"],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Role {
    pub name: String,
    pub kind: RoleKind,
    pub target: Target,
    pub started_at: DateTime<Utc>,
}

fn roles_path(store: &Store, context: &str) -> Result<std::path::PathBuf> {
    path(store, context, "-roles.json")
}
pub(crate) fn load(store: &Store, context: &str) -> Result<BTreeMap<String, Role>> {
    let file = roles_path(store, context)?;
    if !file.exists() {
        return Ok(BTreeMap::new());
    }
    Ok(serde_json::from_slice(&std::fs::read(file)?)?)
}
fn save(store: &Store, context: &str, roles: &BTreeMap<String, Role>) -> Result<()> {
    store.write_private(&roles_path(store, context)?, roles)
}

/// The pinned target for `name`: `None` for main (the original pane), the
/// helper's target otherwise. An unknown role is refused, never guessed.
pub(crate) fn resolve(store: &Store, name: &str) -> Result<Option<Target>> {
    if name == "main" {
        return Ok(None);
    }
    let context = session(store)?;
    let role = load(store, &context.key())?.remove(name).with_context(|| {
        format!("role {name} is not pinned in this manager; start it with hgm role start")
    })?;
    Ok(Some(role.target))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 24
        && name != "main"
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Starts a helper agent in a new, unfocused tab of this workspace and pins it.
/// The provider defaults to main's (prefer another one for reviewers).
pub(crate) async fn start(
    store: &Store,
    kind: RoleKind,
    name: Option<&str>,
    agent: Option<Provider>,
    model: Option<&str>,
    cwd: Option<&Path>,
) -> Result<Value> {
    let context = session(store)?;
    let name = name.unwrap_or(kind.default_name());
    // The helper may work in another folder (a sub-repository or a copy); its
    // hgm commands still use this manager's goals and state.
    let cwd = match cwd {
        Some(dir) => dir
            .canonicalize()
            .with_context(|| format!("role folder {} does not exist", dir.display()))?,
        None => store.workspace.clone(),
    };
    ensure!(
        valid_name(name),
        "role names use a-z, 0-9 and -, up to 24 characters, and cannot be main"
    );
    let mut roles = load(store, &context.key())?;
    if let Some(existing) = roles.get(name)
        && live_pane(&existing.target.pane).await.is_ok()
    {
        bail!(
            "{name} already runs in pane {}; send work to it with --to {name}, or stop it first",
            existing.target.pane
        );
    }
    let main_agent = context.origin.as_ref().and_then(|o| o.agent.clone());
    let provider = agent
        .or_else(|| {
            main_agent
                .as_deref()
                .and_then(|a| Provider::from_str(a, true).ok())
        })
        .unwrap_or(Provider::Codex);
    let config = crate::config::load(&store.workspace)?;
    let model = model.or(config.model(provider.name()));
    let directory = store.private_directory("manager")?;
    let executable = std::env::current_exe()?.canonicalize()?;
    let search_path = manager_path(&directory, &context.id, &executable)?;
    let guide = kind.guide();
    let mut environment = vec![
        ("HGM_CONTEXT", context.id.clone()),
        ("HGM_ROLE", name.to_string()),
        ("HGM_WORKSPACE", store.workspace.display().to_string()),
        ("HGM_STATE_DIR", store.root.display().to_string()),
        ("HGM_BIN", executable.display().to_string()),
        ("PATH", search_path.to_string_lossy().into_owned()),
    ];
    let mut agent_args = args(provider.full_access_args());
    match provider {
        Provider::Claude => {
            // Herdr refuses multi-line agent arguments, so the guide goes in a file.
            let guide_file = directory.join(format!("{}-{name}-guide.md", context.id));
            std::fs::write(&guide_file, guide)
                .with_context(|| format!("write the {name} guide"))?;
            agent_args.extend(args(&["--add-dir"]));
            agent_args.push(store.root.display().to_string());
            agent_args.push("--append-system-prompt-file".into());
            agent_args.push(guide_file.display().to_string());
            agent_args.push("--allowedTools".into());
            agent_args.extend(kind.commands().iter().map(|c| format!("Bash(hgm {c}:*)")));
        }
        Provider::Codex => {
            agent_args.push("--no-daemon".into());
            agent_args.push("-c".into());
            agent_args.push(codex_environment(&environment)?);
            agent_args.push("-c".into());
            agent_args.push(format!(
                "developer_instructions={}",
                serde_json::to_string(guide)?
            ));
            agent_args.push("--add-dir".into());
            agent_args.push(store.root.display().to_string());
        }
        Provider::Copilot => {
            let instructions = directory.join(format!("{}-{name}-copilot", context.id));
            let rules = instructions.join(".github/instructions");
            std::fs::create_dir_all(&rules)?;
            std::fs::write(
                rules.join("hgm-role.instructions.md"),
                format!("---\napplyTo: \"**\"\n---\n{guide}"),
            )?;
            environment.push((
                "COPILOT_CUSTOM_INSTRUCTIONS_DIRS",
                instructions.display().to_string(),
            ));
            agent_args.push("--add-dir".into());
            agent_args.push(store.root.display().to_string());
        }
    }
    if let Some(model) = model {
        agent_args.push("--model".into());
        agent_args.push(model.into());
    }
    // Checked before a tab is opened, so a refused launch leaves nothing behind.
    single_line(&agent_args)?;
    let mut launcher = None;
    if provider == Provider::Codex {
        // Codex takes its guide only inline, and the line Herdr types into the
        // shell may not exceed 1024 bytes: a private launcher named `codex`,
        // first on this tab's PATH, passes the arguments instead.
        let bin = directory.join(format!("{}-{name}-bin", context.id));
        let file = write_launcher(&bin, provider.name(), &std::mem::take(&mut agent_args))?;
        let path =
            std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&search_path)))
                .context("build the role PATH")?;
        for (key, value) in &mut environment {
            if *key == "PATH" {
                *value = path.to_string_lossy().into_owned();
            }
        }
        launcher = Some(file);
    }
    fits_shell_line(provider.name(), &agent_args)?;
    single_line(
        &environment
            .iter()
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>(),
    )?;
    let workspace_id = std::env::var("HERDR_WORKSPACE_ID").context("workspace ID missing")?;
    let mut create = args(&["tab", "create", "--workspace", &workspace_id, "--cwd"]);
    create.push(cwd.display().to_string());
    create.extend(args(&["--label", name, "--no-focus"]));
    for (key, value) in &environment {
        create.push("--env".into());
        create.push(format!("{key}={value}"));
    }
    let created = herdr::json_query(&create)
        .await
        .context("open a tab for the role")?;
    let pane = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .context("Herdr did not return the new pane")?
        .to_string();
    let tab = created["result"]["tab"]["tab_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let herdr_name = format!("hgm-{name}-{}", &context.id[2..8]);
    let mut launch = args(&[
        "agent",
        "start",
        &herdr_name,
        "--kind",
        provider.name(),
        "--pane",
        &pane,
        "--timeout",
        "60000",
        "--",
    ]);
    launch.extend(agent_args);
    if let Err(error) = herdr::json_query(&launch).await {
        // The role is not registered, so nothing uses the tab. An agent left in
        // it (waiting on the provider's own folder-trust question, which hgm
        // cannot answer) would keep the role's Herdr name taken.
        let closed = !tab.is_empty() && herdr::query(&args(&["tab", "close", &tab])).await.is_ok();
        return Err(error).context(format!(
            "start {} in tab {tab} ({}); a provider that first asks whether to trust this folder must be trusted for it outside hgm, or start the role with another provider",
            provider.name(),
            if closed {
                "the tab is closed"
            } else {
                "close that tab"
            }
        ));
    }
    let target = Target::from_value(&live_pane(&pane).await?)?;
    ensure!(
        target.agent.as_deref() == Some(provider.name()),
        "pane {pane} does not run {} after start; inspect tab {tab}",
        provider.name()
    );
    let cross_model = main_agent.as_deref() != Some(provider.name());
    roles.insert(
        name.into(),
        Role {
            name: name.into(),
            kind,
            target,
            started_at: Utc::now(),
        },
    );
    save(store, &context.key(), &roles)?;
    Ok(
        json!({"role": name, "kind": kind, "agent": provider.name(), "pane": pane, "tab": tab,
              "cwd": cwd, "different_from_main": cross_model, "launcher": launcher}),
    )
}

/// Herdr types `<program> <arguments>` at the shell prompt, which reads at most
/// 1024 bytes per line in canonical mode; longer launches fail after the tab opens.
fn fits_shell_line(program: &str, arguments: &[String]) -> Result<()> {
    // Each argument may gain quotes and a separator when Herdr encodes it.
    let length = program.len() + arguments.iter().map(|a| a.len() + 3).sum::<usize>();
    ensure!(
        length < 1000,
        "the {program} launch line would take about {length} bytes; Herdr types it into a shell that reads at most 1024 bytes per line"
    );
    Ok(())
}

/// Writes `bin/<program>`, which runs the real `program` found on PATH (outside
/// `bin`) with `arguments` plus any arguments it receives.
fn write_launcher(bin: &Path, program: &str, arguments: &[String]) -> Result<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let search = std::env::var_os("PATH").unwrap_or_default();
    let real = std::env::split_paths(&search)
        .filter(|dir| !dir.starts_with(bin))
        .map(|dir| dir.join(program))
        .find(|file| {
            std::fs::metadata(file)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .with_context(|| format!("{program} is not on PATH"))?;
    std::fs::create_dir_all(bin)?;
    let mut script = format!("#!/bin/sh\nexec {}", quote(real.as_os_str()));
    for argument in arguments {
        script.push(' ');
        script.push_str(&quote(std::ffi::OsStr::new(argument)));
    }
    script.push_str(" \"$@\"\n");
    let file = bin.join(program);
    std::fs::write(&file, script).with_context(|| format!("write the {program} launcher"))?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700))?;
    Ok(file)
}

/// Herdr encodes agent arguments for the target shell and refuses line breaks
/// (`invalid_agent_argument`); multi-line text has to travel in a file.
fn single_line(values: &[String]) -> Result<()> {
    if let Some(value) = values.iter().find(|v| v.contains(['\n', '\r'])) {
        let head: String = value
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(60)
            .collect();
        bail!(
            "agent argument spans several lines ({head}...); Herdr cannot pass it to the shell, so put the text in a file"
        );
    }
    Ok(())
}

/// Pinned roles with their live state, main first.
pub(crate) async fn list(store: &Store) -> Result<Value> {
    let context = session(store)?;
    let mut rows = vec![];
    if let Some(origin) = &context.origin {
        rows.push(row("main", None, origin).await);
    }
    for role in load(store, &context.key())?.values() {
        rows.push(row(&role.name, Some(role.kind), &role.target).await);
    }
    Ok(Value::Array(rows))
}
async fn row(name: &str, kind: Option<RoleKind>, target: &Target) -> Value {
    let state = match live_pane(&target.pane).await {
        Ok(live) if target.check(&live).is_ok() => live["agent_status"].clone(),
        Ok(_) => json!("replaced"),
        Err(_) => json!("missing"),
    };
    json!({"role": name, "kind": kind, "agent": target.agent, "pane": target.pane, "state": state})
}

/// Closes a role's tab (only tabs hgm opened) and unpins it. A working agent
/// is left alone unless `force`.
pub(crate) async fn stop(store: &Store, name: &str, force: bool) -> Result<Value> {
    let context = session(store)?;
    let mut roles = load(store, &context.key())?;
    let role = roles
        .get(name)
        .with_context(|| format!("role {name} is not pinned in this manager"))?
        .clone();
    if let Ok(live) = live_pane(&role.target.pane).await
        && role.target.check(&live).is_ok()
    {
        ensure!(
            force || live["agent_status"] != "working",
            "{name} is working; wait for it, or pass --force to close its tab anyway"
        );
        herdr::query(&args(&["tab", "close", &role.target.tab])).await?;
    }
    roles.remove(name);
    save(store, &context.key(), &roles)?;
    Ok(json!({"role": name, "stopped": true}))
}

/// The calling role, from the environment hgm set when it started the agent.
fn calling_role(store: &Store) -> Result<Role> {
    let name = std::env::var("HGM_ROLE").context("only a helper role runs this command")?;
    let context = crate::binding::session(store)?.key();
    load(store, &context)?
        .remove(&name)
        .with_context(|| format!("role {name} is not pinned in its manager"))
}

/// The helper role running this hgm, refused unless its kind may run
/// `command`; `None` outside a helper role (main or the manager).
pub(crate) fn caller(store: &Store, command: &str) -> Result<Option<String>> {
    if std::env::var_os("HGM_ROLE").is_none() {
        return Ok(None);
    }
    let role = calling_role(store)?;
    ensure!(
        role.kind.commands().contains(&command),
        "the {} role may not run hgm {command}",
        role.name
    );
    Ok(Some(role.name))
}

/// Points the manager at news from a helper: the next wake carries `note`.
/// Returns whether a running watch of `goal` will carry it.
fn nudge_manager(store: &Store, goal: &str, note: &str) -> Result<bool> {
    let context = crate::binding::session(store)?.key();
    if !crate::watch::watch_path(store, &context)?.exists() {
        return Ok(false);
    }
    for _ in 0..20 {
        if let Ok(_lease) = operation_lease(store, &context) {
            let mut watch = crate::watch::load_watch(store, &context)?;
            if watch.active && watch.goal == goal {
                watch.pending = Some(match watch.pending.take() {
                    Some(pending) => format!("{pending}; also {note}"),
                    None => note.into(),
                });
                crate::watch::save_watch(store, &watch)?;
                return Ok(true);
            }
            return Ok(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    eprintln!("The manager's watch is busy; it will see this on the timeline.");
    Ok(true)
}

/// Sends a review request to a reviewer role, recording the workspace state it
/// judges. The reviewer answers with `hgm review-submit`.
pub(crate) async fn request_review(
    store: &Store,
    goal_id: &str,
    reviewer: &str,
    text: &str,
) -> Result<Value> {
    let words = crate::model::word_units(text);
    ensure!(
        (1..=crate::admission::ASSIGNMENT_WORD_BUDGET).contains(&words),
        "describe what to review in 1..{} words",
        crate::admission::ASSIGNMENT_WORD_BUDGET
    );
    let context = session(store)?;
    let role = load(store, &context.key())?
        .remove(reviewer)
        .with_context(|| {
            format!("role {reviewer} is not pinned; start it with hgm role start reviewer")
        })?;
    ensure!(
        role.kind == RoleKind::Reviewer,
        "{reviewer} is not a reviewer role"
    );
    let goal = store.load(goal_id)?;
    let live = live_pane(&role.target.pane).await?;
    role.target.check(&live)?;
    let current = screen(&role.target.pane).await?;
    if let Some(refusal) = refusal(&live, &current) {
        bail!("{}; nothing was sent", refusal.explain(reviewer));
    }
    let review = format!("r-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let digest = crate::fingerprint::digest(goal.work_dir()?)?;
    let list = |items: &[String]| {
        if items.is_empty() {
            "(none)".to_string()
        } else {
            items.join("; ")
        }
    };
    let request = format!(
        "Review request {review} for goal {goal_id}. You are an independent reviewer: you did not write this work; do not change files.\nWhat to review: {text}\nWork dir (review the files there): {work_dir}\nGoal: {}\nAcceptance: {}\nInvariants: {}\nOut of scope: {}\nSubmit once, with findings as a JSON array on stdin:\nhgm review-submit {goal_id} --review {review} --verdict approve|revise --findings - <<'EOF'\n[{{\"severity\":\"blocking\",\"where\":\"path:line\",\"issue\":\"what is wrong\",\"evidence\":\"how you know\"}}]\nEOF\nJudge the files and the diff, not a summary of them. A finding is blocking only when you can show it: a command that fails or a line that is wrong, named in where and evidence. Everything else is advisory and becomes a follow-up. Use revise when any finding is blocking; approve allows only advisory findings (or []).",
        goal.spec.objective,
        list(&goal.spec.acceptance),
        list(&goal.spec.invariants),
        list(&goal.spec.out_of_scope),
        work_dir = match &goal.checkout {
            Some(checkout) => format!(
                "{} (the goal's checkout, branch {})",
                checkout.path.display(),
                checkout.branch
            ),
            None => goal.work_dir()?.display().to_string(),
        },
    );
    store.record(
        goal_id,
        "review_requested",
        json!({"review": review, "reviewer": reviewer, "agent": role.target.agent,
               "digest": digest, "text": text}),
    )?;
    herdr::json_query(&args(&["agent", "prompt", &role.target.pane, &request]))
        .await
        .context("review request delivery uncertain; inspect the reviewer before resending")?;
    // As with send: the input line must empty, or the review may not start.
    let agent = role.target.agent.as_deref().unwrap_or("");
    let consumed = crate::supervision::input_consumed(&role.target.pane, agent)
        .await
        .unwrap_or(false);
    if !consumed {
        store.record(
            goal_id,
            "review_delivery",
            json!({"review": review, "reviewer": reviewer, "consumed": false, "delivery": "uncertain"}),
        )?;
        return Ok(
            json!({"review": review, "reviewer": reviewer, "status": "uncertain",
            "message": "the request may still sit in the reviewer's input; inspect it before resending"}),
        );
    }
    Ok(json!({"review": review, "reviewer": reviewer, "status": "requested"}))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Verdict {
    Approve,
    Revise,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Finding {
    severity: Severity,
    #[serde(default)]
    r#where: String,
    issue: String,
    #[serde(default)]
    evidence: String,
}
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Severity {
    Blocking,
    Advisory,
}

/// Records a reviewer's verdict for its own request. A verdict on a workspace
/// that changed since the request is kept but marked stale. The person is
/// notified; the manager learns it from its watch, or at once when no watch
/// runs (the goal awaits the person's acceptance).
pub(crate) async fn submit_review(
    store: &Store,
    goal_id: &str,
    review: &str,
    verdict: Verdict,
    findings: &str,
) -> Result<Value> {
    let role = calling_role(store)?;
    ensure!(
        role.kind == RoleKind::Reviewer,
        "only a reviewer role submits reviews"
    );
    let events = store.events(goal_id)?;
    let request = events
        .iter()
        .rev()
        .find(|e| e.kind == "review_requested" && e.detail["review"] == review)
        .with_context(|| format!("no review {review} was requested on {goal_id}"))?;
    ensure!(
        request.detail["reviewer"] == role.name.as_str(),
        "review {review} was requested from {}",
        request.detail["reviewer"]
    );
    ensure!(
        !events
            .iter()
            .any(|e| e.kind == "reviewed" && e.detail["review"] == review),
        "review {review} was already submitted"
    );
    let findings: Vec<Finding> = serde_json::from_str(findings)
        .context("findings must be a JSON array of {severity, where, issue, evidence}")?;
    ensure!(
        findings.iter().all(|f| !f.issue.trim().is_empty()),
        "every finding needs an issue"
    );
    let blocking = findings
        .iter()
        .filter(|f| f.severity == Severity::Blocking)
        .count();
    ensure!(
        (verdict == Verdict::Revise) == (blocking > 0),
        "revise needs at least one blocking finding, and approve allows none"
    );
    // The verdict keeps the state it was given for; once the work dir no
    // longer has this fingerprint, the verdict is shown as stale.
    let fingerprint = crate::fingerprint::digest(store.load(goal_id)?.work_dir()?)?;
    let current = fingerprint == request.detail["digest"];
    store.record(
        goal_id,
        "reviewed",
        json!({"review": review, "reviewer": role.name, "agent": role.target.agent,
               "verdict": verdict, "findings": findings, "current": current,
               "fingerprint": fingerprint}),
    )?;
    let note = format!(
        "review {review} from {}: {verdict:?} with {blocking} blocking finding(s){}; read it with hgm log {goal_id}",
        role.name,
        if current {
            ""
        } else {
            " (stale: the workspace changed during the review)"
        }
    );
    let mut delivered = nudge_manager(store, goal_id, &note)?;
    if !delivered {
        delivered = crate::watch::wake_manager_now(store, goal_id, &note)
            .await
            .unwrap_or_else(|error| {
                eprintln!("Could not wake the manager: {error:#}");
                false
            });
    }
    Ok(
        json!({"review": review, "verdict": verdict, "blocking": blocking, "current": current, "manager_told": delivered}),
    )
}

/// Every review verdict on a goal, oldest first, with whether it still
/// describes the work: `current` when the work dir has the fingerprint the
/// verdict was given for, `stale` when it changed since, `unknown` when the
/// work dir cannot be read. A verdict submitted on a work dir that changed
/// during its review is stale from the start. A verdict recorded before
/// fingerprints were stored is compared through its request.
pub(crate) fn review_states(
    goal: &crate::model::Goal,
    events: &[crate::events::Event],
    fingerprints: &mut crate::fingerprint::Fingerprints,
) -> Vec<Value> {
    let mut current: Option<Option<String>> = None;
    let mut states = vec![];
    for event in events.iter().filter(|e| e.kind == "reviewed") {
        let d = &event.detail;
        let given = d["fingerprint"].as_str().map(String::from).or_else(|| {
            events
                .iter()
                .find(|e| e.kind == "review_requested" && e.detail["review"] == d["review"])
                .and_then(|request| request.detail["digest"].as_str())
                .map(String::from)
        });
        let now = current
            .get_or_insert_with(|| goal.work_dir().ok().and_then(|dir| fingerprints.of(dir)));
        let fresh = match (now.as_deref(), given) {
            _ if d["current"] == false => "stale",
            (Some(now), Some(given)) if now == given => "current",
            (Some(_), _) => "stale",
            (None, _) => "unknown",
        };
        let findings = d["findings"].as_array().map_or(&[][..], Vec::as_slice);
        let blocking = findings
            .iter()
            .filter(|f| f["severity"] == "blocking")
            .count();
        states.push(json!({
            "review": d["review"], "reviewer": d["reviewer"], "agent": d["agent"],
            "verdict": d["verdict"], "at": event.at, "blocking": blocking,
            "advisory": findings.len() - blocking, "current_at_submit": d["current"],
            "fresh": fresh,
        }));
    }
    states
}

/// Under `loop.require_review`, why the goal may not move to review yet, with
/// the commands to run next; `None` when it may (or the setting is off). It
/// needs a current approving verdict from a reviewer whose provider received
/// none of the goal's linked assignments or corrections, and no reviewer's
/// latest current verdict may ask for a revision.
pub(crate) fn review_hold(
    goal: &crate::model::Goal,
    events: &[crate::events::Event],
) -> Option<String> {
    if !goal.spec.loop_policy.require_review {
        return None;
    }
    let id = &goal.id;
    let workers: std::collections::BTreeSet<&str> = events
        .iter()
        .filter(|e| matches!(e.kind.as_str(), "assigned" | "correction"))
        .filter_map(|e| e.detail["agent"].as_str())
        .collect();
    let states = review_states(goal, events, &mut Default::default());
    let mut latest: BTreeMap<&str, &Value> = BTreeMap::new();
    for state in &states {
        if let Some(reviewer) = state["reviewer"].as_str() {
            latest.insert(reviewer, state);
        }
    }
    let text = |state: &Value, key: &str| state[key].as_str().unwrap_or_default().to_string();
    if let Some(revise) = latest
        .values()
        .find(|s| s["fresh"] == "current" && s["verdict"] == "revise")
    {
        return Some(format!(
            "loop.require_review: review {} from {} asks for a revision with {} blocking finding(s); fix them, ask again with hgm review {id} TEXT, then run hgm resume {id}",
            text(revise, "review"),
            text(revise, "reviewer"),
            revise["blocking"]
        ));
    }
    let other_provider = |s: &Value| s["agent"].as_str().is_some_and(|a| !workers.contains(a));
    if latest
        .values()
        .any(|s| s["fresh"] == "current" && s["verdict"] == "approve" && other_provider(s))
    {
        return None;
    }
    let reason = match states.iter().rev().find(|s| s["verdict"] == "approve") {
        None => "no reviewer has approved this goal".to_string(),
        Some(s) if s["fresh"] != "current" => format!(
            "approving review {} is stale: the work dir changed after it",
            text(s, "review")
        ),
        Some(s) => format!(
            "approving review {} came from {}, a provider that worked on this goal",
            text(s, "review"),
            s["agent"].as_str().unwrap_or("an unknown provider")
        ),
    };
    let workers = if workers.is_empty() {
        "the worker's".to_string()
    } else {
        workers.into_iter().collect::<Vec<_>>().join(", ")
    };
    Some(format!(
        "loop.require_review: {reason}. Start a reviewer on a provider other than {workers} (hgm role start reviewer --agent OTHER) if none runs, ask with hgm review {id} TEXT, then run hgm resume {id}"
    ))
}

/// Records an implementer's report and points the manager at it.
/// The longest report, in words (two CJK characters count as one): a worker's
/// full report with plan, changes and verification ran to a few thousand
/// characters on 2026-11-15.
pub(crate) const REPORT_WORD_BUDGET: usize = 3_000;

/// Records a worker's report on the goal's timeline in full, from `text` or
/// the file at `file`, so the manager reads it with `hgm log` or the packet
/// instead of the provider's transcript. main, impl and the manager may
/// report; a reviewer answers with `review-submit`. Like a decision, a report
/// completes nothing.
pub(crate) fn report(
    store: &Store,
    goal_id: &str,
    task: Option<&str>,
    text: Option<&str>,
    file: Option<&Path>,
) -> Result<Value> {
    let by = crate::packet::recorder(store, "report")?;
    let text = match (text, file) {
        (Some(text), None) => text.to_string(),
        (None, Some(file)) => std::fs::read_to_string(file)
            .with_context(|| format!("read the report from {}", file.display()))?,
        _ => bail!("give the report as TEXT or with --file PATH, not both"),
    };
    let text = text.trim();
    let words = crate::model::word_units(text);
    ensure!(
        (1..=REPORT_WORD_BUDGET).contains(&words),
        "a report takes 1 to {REPORT_WORD_BUDGET} words; this one has about {words}: shorten it, or keep the details in a file in the work dir and name that file in the report"
    );
    crate::packet::record_target(store, goal_id, task)?;
    let mut detail = json!({"from": by, "task": task, "text": text});
    if let Some(file) = file {
        detail["file"] = json!(file);
    }
    store.record(goal_id, "report", detail)?;
    // A helper's report wakes the manager; main's goes with the wake its
    // settling brings, among the events since the last one.
    if std::env::var_os("HGM_ROLE").is_some() {
        let short: String = text.chars().take(200).collect();
        nudge_manager(
            store,
            goal_id,
            &format!("{by} reported on {}: {short}", task.unwrap_or("the goal")),
        )?;
    }
    Ok(json!({"recorded": "report", "by": by, "words": words}))
}

#[cfg(test)]
mod tests {
    use crate::events::Event;
    use serde_json::json;

    #[test]
    fn a_verdict_goes_stale_when_the_work_dir_changes_and_require_review_needs_another_provider()
    -> anyhow::Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = crate::store::Store::new(workspace.path(), state.path())?;
        std::fs::write(workspace.path().join("a.rs"), "one")?;
        let goal = store.add(
            serde_norway::from_str(
                "objective: o\nacceptance: [a]\nloop: {require_review: true}\n",
            )?,
            None,
        )?;
        let fingerprint = crate::fingerprint::digest(workspace.path())?;
        let event = |kind: &str, detail: serde_json::Value| Event::new(&goal.id, kind, detail);
        let verdict = |review: &str, reviewer: &str, agent: &str, verdict: &str| {
            let blocking = if verdict == "revise" {
                json!([{"severity": "blocking", "issue": "fails"}])
            } else {
                json!([])
            };
            event(
                "reviewed",
                json!({"review": review, "reviewer": reviewer, "agent": agent,
                       "verdict": verdict, "findings": blocking, "current": true,
                       "fingerprint": fingerprint}),
            )
        };
        let hold = |events: &[Event]| super::review_hold(&goal, events);
        let mut events = vec![event(
            "assigned",
            json!({"task": "work", "agent": "claude"}),
        )];
        assert!(
            hold(&events)
                .unwrap()
                .contains("no reviewer has approved this goal")
        );
        events.push(verdict("r-1", "reviewer", "claude", "approve"));
        let same = hold(&events).unwrap();
        assert!(
            same.contains("came from claude, a provider that worked on this goal")
                && same.contains("other than claude")
                && same.contains(&format!("hgm resume {}", goal.id)),
            "{same}"
        );
        events.push(verdict("r-2", "second", "codex", "approve"));
        assert_eq!(hold(&events), None);
        // A current revise from any reviewer holds it again.
        events.push(verdict("r-3", "reviewer", "claude", "revise"));
        assert!(
            hold(&events)
                .unwrap()
                .contains("review r-3 from reviewer asks for a revision with 1 blocking")
        );
        events.pop();

        let states = super::review_states(&goal, &events, &mut Default::default());
        assert!(states.iter().all(|s| s["fresh"] == "current"), "{states:?}");
        std::fs::write(workspace.path().join("a.rs"), "changed")?;
        let states = super::review_states(&goal, &events, &mut Default::default());
        assert!(states.iter().all(|s| s["fresh"] == "stale"), "{states:?}");
        assert!(
            hold(&events)
                .unwrap()
                .contains("approving review r-2 is stale")
        );

        // Without the setting, nothing is held.
        let plain = store.add(
            serde_norway::from_str("objective: o\nacceptance: [a]\n")?,
            None,
        )?;
        assert_eq!(super::review_hold(&plain, &[]), None);
        Ok(())
    }

    #[test]
    fn the_reviewer_guide_judges_the_diff_and_blocks_only_on_what_it_can_show() {
        let guide = super::REVIEWER_GUIDE;
        for rule in [
            "Judge the artifact and the diff, not the summary.",
            "Block only on a problem you can show: a command that fails",
            "or a line that is wrong",
            "Everything else",
            "is an\n  advisory finding; hgm keeps advisory findings as follow-ups",
            "it is shown as stale",
        ] {
            assert!(guide.contains(rule), "missing {rule:?}");
        }
        assert_eq!(
            super::RoleKind::Reviewer.commands(),
            ["show", "review-submit"]
        );
    }

    #[test]
    fn multi_line_agent_arguments_are_refused_before_launch() {
        let ok = vec![
            "--append-system-prompt-file".to_string(),
            "/x/guide.md".into(),
        ];
        assert!(super::single_line(&ok).is_ok());
        for broken in ["first\nsecond", "first\r\nsecond", "trailing\n"] {
            let error = super::single_line(&[broken.to_string()]).unwrap_err();
            assert!(format!("{error}").contains("in a file"), "{error}");
        }
    }
    #[test]
    fn launch_lines_beyond_the_shell_limit_are_refused() {
        assert!(super::fits_shell_line("claude", &["--model".into(), "opus".into()]).is_ok());
        let long = vec!["x".repeat(1100)];
        let error = super::fits_shell_line("codex", &long).unwrap_err();
        assert!(format!("{error}").contains("1024 bytes"), "{error}");
    }
}
