//! User and project overrides for prompts and workflow defaults.
//!
//! The user config (`$XDG_CONFIG_HOME/herdr-goal-manager/config.yaml`, or
//! `~/.config/...`) may replace the manager guide. A project config
//! (`<workspace>/.hgm/config.yaml`) may only add to it, like `AGENTS.md`: a
//! cloned repository must not rewrite the manager's ground rules. Neither can
//! configure away code-enforced rules (confirmation, budgets, word caps, `auto`
//! needing a check, human acceptance) or add commands: stop checks and checks
//! belong to each confirmed goal.
use crate::{
    contract::{Autonomy, LoopPolicy},
    model::GoalSpec,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The built-in manager guide; `prompts.manager` in the user config replaces it.
pub(crate) const BUILTIN_GUIDE: &str = include_str!("../prompts/manager.md");
/// First user turn for every provider. The guide itself travels as system or
/// developer instructions so it is not mistaken for the user's task.
pub(crate) const BUILTIN_OPENER: &str = "Continue our goal conversation. Run hgm overview once, then ask only what is needed to move forward.";
/// Wakes per goal and manager context: five-minute checks for about eight hours.
pub(crate) const DEFAULT_MAX_WAKES: u32 = 100;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct File {
    /// Manager provider when the original tab runs no known agent.
    agent: Option<String>,
    /// Model per provider (`claude`, `codex`, `copilot`).
    models: BTreeMap<String, String>,
    prompts: Prompts,
    #[serde(rename = "loop")]
    loop_defaults: LoopDefaults,
    watch: WatchDefaults,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Prompts {
    /// File replacing the built-in guide (user config only).
    manager: Option<PathBuf>,
    /// Files appended to the guide, relative to the config file.
    manager_append: Vec<PathBuf>,
    opener: Option<String>,
    /// Text appended to every wake notice.
    wake_append: Option<String>,
    /// Text appended to every linked assignment, before the contract.
    assignment_append: Option<String>,
}

/// Defaults for a new goal's `loop`; fields the goal sets itself win.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LoopDefaults {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_assignments: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_hours: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patrol_minutes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<Autonomy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_first: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reanchor_minutes: Option<u32>,
}

/// Monitoring defaults for `supervise` and linked sends.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct WatchDefaults {
    pub idle_secs: Option<u64>,
    pub cooldown_secs: Option<u64>,
    pub max_wakes: Option<u32>,
    /// Seconds between reconcile reads while Herdr events drive the watch.
    pub reconcile_secs: Option<u64>,
    /// `false` hides the sheep in the Goal watch tab.
    pub sheep: Option<bool>,
}

/// Effective overrides after merging the user and project configs.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Config {
    pub agent: Option<String>,
    pub models: BTreeMap<String, String>,
    /// Replacement guide text, when the user config sets one.
    #[serde(skip)]
    pub guide: Option<String>,
    /// Appended guide sections: (source file, text).
    #[serde(skip)]
    pub guide_append: Vec<(PathBuf, String)>,
    pub opener: Option<String>,
    pub wake_append: Option<String>,
    pub assignment_append: Option<String>,
    #[serde(rename = "loop")]
    pub loop_defaults: LoopDefaults,
    pub watch: WatchDefaults,
    /// Config files that were read, user first.
    pub sources: Vec<PathBuf>,
}

pub(crate) fn user_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|base| base.join("herdr-goal-manager/config.yaml"))
}
pub(crate) fn project_path(workspace: &Path) -> PathBuf {
    workspace.join(".hgm/config.yaml")
}

fn read_file(path: &Path) -> Result<Option<File>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(Some(
        serde_norway::from_str(&text).with_context(|| format!("parse {}", path.display()))?,
    ))
}
fn read_text(base: &Path, file: &Path) -> Result<(PathBuf, String)> {
    let path = base.join(file);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read prompt file {}", path.display()))?;
    Ok((path, text))
}
fn join(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
        (a, b) => a.or(b),
    }
}

/// Reads the user config, then the workspace config; later values win and
/// appended text accumulates.
pub(crate) fn load(workspace: &Path) -> Result<Config> {
    let mut config = Config::default();
    let project = project_path(workspace);
    for (path, is_user) in user_path()
        .map(|p| (p, true))
        .into_iter()
        .chain([(project, false)])
    {
        let Some(file) = read_file(&path)? else {
            continue;
        };
        let base = path.parent().unwrap_or(Path::new("."));
        let prompts = file.prompts;
        if let Some(manager) = &prompts.manager {
            ensure!(
                is_user,
                "{}: prompts.manager can only be set in the user config ({}); a project may add manager_append files",
                path.display(),
                user_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            );
            config.guide = Some(read_text(base, manager)?.1);
        }
        for file in &prompts.manager_append {
            config.guide_append.push(read_text(base, file)?);
        }
        config.opener = prompts.opener.or(config.opener);
        config.wake_append = join(config.wake_append, prompts.wake_append);
        config.assignment_append = join(config.assignment_append, prompts.assignment_append);
        config.agent = file.agent.or(config.agent);
        config.models.extend(file.models);
        let mut defaults = serde_json::to_value(&config.loop_defaults)?;
        merge(&mut defaults, serde_json::to_value(&file.loop_defaults)?);
        config.loop_defaults = serde_json::from_value(defaults)?;
        let watch = file.watch;
        config.watch = WatchDefaults {
            idle_secs: watch.idle_secs.or(config.watch.idle_secs),
            cooldown_secs: watch.cooldown_secs.or(config.watch.cooldown_secs),
            max_wakes: watch.max_wakes.or(config.watch.max_wakes),
            reconcile_secs: watch.reconcile_secs.or(config.watch.reconcile_secs),
            sheep: watch.sheep.or(config.watch.sheep),
        };
        config.sources.push(path.clone());
        config
            .validate()
            .with_context(|| format!("invalid setting in {}", path.display()))?;
    }
    Ok(config)
}
/// Copies keys from `defaults` that `target` does not set.
fn fill(target: &mut Map<String, Value>, defaults: Value) {
    if let Value::Object(defaults) = defaults {
        for (key, value) in defaults {
            target.entry(key).or_insert(value);
        }
    }
}
/// Overwrites `base` keys with `overlay` keys.
fn merge(base: &mut Value, overlay: Value) {
    if let (Some(base), Value::Object(overlay)) = (base.as_object_mut(), overlay) {
        base.extend(overlay);
    }
}

impl Config {
    fn validate(&self) -> Result<()> {
        if let Some(agent) = &self.agent {
            ensure!(
                matches!(agent.as_str(), "claude" | "codex" | "copilot"),
                "agent must be claude, codex, or copilot, got {agent}"
            );
        }
        for provider in self.models.keys() {
            ensure!(
                matches!(provider.as_str(), "claude" | "codex" | "copilot"),
                "models keys must be claude, codex, or copilot, got {provider}"
            );
        }
        let secs = 10..=86400;
        ensure!(
            self.watch.idle_secs.is_none_or(|s| secs.contains(&s))
                && self.watch.cooldown_secs.is_none_or(|s| secs.contains(&s)),
            "watch.idle_secs and watch.cooldown_secs must be 10..=86400"
        );
        ensure!(
            self.watch
                .max_wakes
                .is_none_or(|w| (1..=DEFAULT_MAX_WAKES).contains(&w)),
            "watch.max_wakes must be 1..=100"
        );
        ensure!(
            self.watch
                .reconcile_secs
                .is_none_or(|s| (10..=300).contains(&s)),
            "watch.reconcile_secs must be 10..=300"
        );
        let mut policy = serde_json::to_value(LoopPolicy::default())?;
        merge(&mut policy, serde_json::to_value(&self.loop_defaults)?);
        serde_json::from_value::<LoopPolicy>(policy)?.validate()
    }
    /// The manager guide: built-in or replaced, then appended sections.
    pub(crate) fn guide(&self) -> String {
        let mut guide = self
            .guide
            .clone()
            .unwrap_or_else(|| BUILTIN_GUIDE.to_string());
        for (path, text) in &self.guide_append {
            guide += &format!(
                "\n\n## Local instructions ({})\n\n{}",
                path.display(),
                text.trim()
            );
        }
        guide
    }
    pub(crate) fn opener(&self) -> &str {
        self.opener.as_deref().unwrap_or(BUILTIN_OPENER)
    }
    pub(crate) fn model(&self, provider: &str) -> Option<&str> {
        self.models.get(provider).map(String::as_str)
    }
    pub(crate) fn idle_secs(&self) -> u64 {
        self.watch.idle_secs.unwrap_or(300)
    }
    pub(crate) fn cooldown_secs(&self) -> u64 {
        self.watch.cooldown_secs.unwrap_or(300)
    }
    pub(crate) fn max_wakes(&self) -> u32 {
        self.watch.max_wakes.unwrap_or(DEFAULT_MAX_WAKES)
    }
    /// Default 30 s: a closed pane and dropped events show up within this time.
    pub(crate) fn reconcile_secs(&self) -> u64 {
        self.watch.reconcile_secs.unwrap_or(30)
    }
    /// Whether the Goal watch tab shows its running sheep (default on).
    pub(crate) fn sheep(&self) -> bool {
        self.watch.sheep.unwrap_or(true)
    }
}

/// Builds a goal spec from a parsed goal file, filling `loop` fields the goal
/// does not set from the configured defaults.
pub(crate) fn goal_spec(workspace: &Path, mut raw: Value) -> Result<GoalSpec> {
    let defaults = serde_json::to_value(load(workspace)?.loop_defaults)?;
    let Some(object) = raw.as_object_mut() else {
        bail!("a goal must be a mapping with objective and acceptance");
    };
    if let Some(policy) = object
        .entry("loop")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    {
        fill(policy, defaults);
    }
    serde_json::from_value(raw).context("invalid goal definition")
}

/// Text appended to wake notices. A config typo must not end an unattended
/// loop, so an error is reported and the notice goes out without it.
pub(crate) fn wake_extra(workspace: &Path) -> String {
    match load(workspace) {
        Ok(config) => config
            .wake_append
            .map(|text| format!("\n{text}"))
            .unwrap_or_default(),
        Err(error) => {
            eprintln!("Ignoring Goal Manager config for this wake: {error:#}");
            String::new()
        }
    }
}
