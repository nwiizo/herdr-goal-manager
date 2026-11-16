//! User and project overrides for prompts and workflow defaults.
//!
//! The user config (`$XDG_CONFIG_HOME/herdr-goal-manager/config.yaml`, or
//! `~/.config/...`) may replace the manager guide. A project config
//! (`<workspace>/.hgm/config.yaml`) may only add to it, like `AGENTS.md`: a
//! cloned repository must not rewrite the manager's ground rules. Neither can
//! configure away code-enforced rules (confirmation, budgets, word caps, `auto`
//! needing a check, human acceptance) or add commands: stop checks and checks
//! belong to each confirmed goal. The optional factory mode (`factory`) is off
//! by default; a project may switch it on or off but may only tighten its
//! review limits below the user's (or the built-in) values.
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
/// The guide section for the factory queue, added while factory mode is on.
pub(crate) const FACTORY_GUIDE: &str = include_str!("../prompts/factory.md");
/// First user turn for every provider. The guide itself travels as system or
/// developer instructions so it is not mistaken for the user's task.
pub(crate) const BUILTIN_OPENER: &str = "Continue our goal conversation. Run hgm overview once, then ask only what is needed to move forward.";
/// Wakes per goal and manager context: three-minute checks for about eight hours.
pub(crate) const DEFAULT_MAX_WAKES: u32 = 160;
/// The most wakes a goal's watch may be given.
pub(crate) const MAX_WAKES_LIMIT: u32 = 500;
/// User decision (2026-11-15): the watch's times default to three minutes.
/// Seconds without an observed change before the manager is woken.
pub(crate) const DEFAULT_IDLE_SECS: u64 = 180;
/// Seconds between wakes for patrols and unchanged screens.
pub(crate) const DEFAULT_COOLDOWN_SECS: u64 = 180;
/// Seconds the worker may sit settled short of completion before the person is told.
pub(crate) const DEFAULT_IDLE_NOTICE_SECS: u64 = 180;

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
    factory: FactoryDefaults,
    manager: ManagerDefaults,
}

/// The Goal Manager's own settings.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ManagerDefaults {
    /// `false` keeps hgm from setting a native goal (`/goal`) on a Claude
    /// Code Goal Manager; unset means on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_goal: Option<bool>,
}
impl ManagerDefaults {
    fn is_unset(&self) -> bool {
        *self == Self::default()
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_goal: Option<crate::contract::NativeGoalMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_review: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patrol_wake: Option<crate::contract::PatrolWake>,
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
    /// Seconds the worker may sit settled short of completion before the
    /// person is told.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_notice_secs: Option<u64>,
}

/// Results awaiting review before the factory queue stops dispatching: one
/// under review and one finished, so a third waits for the person.
pub(crate) const DEFAULT_MAX_AWAITING_REVIEW: u32 = 2;
/// Confirmed goals in progress before the factory queue stops dispatching:
/// the original tab runs one agent, and parallel loops are not started.
pub(crate) const DEFAULT_MAX_ACTIVE: u32 = 1;

/// The optional factory mode: a local queue of agreed contracts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct FactoryDefaults {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_awaiting_review: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_active: Option<u32>,
    /// Whether `queue next` gives each goal its own checkout. Unset: on in a
    /// Git repository, off elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout: Option<bool>,
}
impl FactoryDefaults {
    /// No config file sets a factory key; `hgm config` then omits the section.
    fn is_unset(&self) -> bool {
        *self == Self::default()
    }
    pub(crate) fn max_awaiting_review(&self) -> u32 {
        self.max_awaiting_review
            .unwrap_or(DEFAULT_MAX_AWAITING_REVIEW)
    }
    pub(crate) fn max_active(&self) -> u32 {
        self.max_active.unwrap_or(DEFAULT_MAX_ACTIVE)
    }
    /// Applies one config file. The user file sets values; a later project
    /// file may switch the mode and lower, never raise, a review limit.
    fn apply(&mut self, file: Self, is_user: bool) -> Result<()> {
        if !is_user {
            for (key, value, ceiling) in [
                (
                    "max_awaiting_review",
                    file.max_awaiting_review,
                    self.max_awaiting_review(),
                ),
                ("max_active", file.max_active, self.max_active()),
            ] {
                ensure!(
                    value.is_none_or(|value| value <= ceiling),
                    "a project config may only lower factory.{key} (at most {ceiling} from the user config or the default); raise it in the user config instead"
                );
            }
        }
        *self = Self {
            enabled: file.enabled.or(self.enabled),
            max_awaiting_review: file.max_awaiting_review.or(self.max_awaiting_review),
            max_active: file.max_active.or(self.max_active),
            checkout: file.checkout.or(self.checkout),
        };
        Ok(())
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            (1..=20).contains(&self.max_awaiting_review()),
            "factory.max_awaiting_review must be 1..=20"
        );
        ensure!(
            (1..=10).contains(&self.max_active()),
            "factory.max_active must be 1..=10"
        );
        Ok(())
    }
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
    /// Factory mode settings; omitted from `hgm config` when no file sets one.
    #[serde(skip_serializing_if = "FactoryDefaults::is_unset")]
    pub factory: FactoryDefaults,
    /// Goal Manager settings; omitted from `hgm config` when no file sets one.
    #[serde(skip_serializing_if = "ManagerDefaults::is_unset")]
    pub manager: ManagerDefaults,
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
            idle_notice_secs: watch.idle_notice_secs.or(config.watch.idle_notice_secs),
        };
        config
            .factory
            .apply(file.factory, is_user)
            .with_context(|| format!("invalid setting in {}", path.display()))?;
        config.manager.native_goal = file.manager.native_goal.or(config.manager.native_goal);
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
                && self.watch.cooldown_secs.is_none_or(|s| secs.contains(&s))
                && self
                    .watch
                    .idle_notice_secs
                    .is_none_or(|s| secs.contains(&s)),
            "watch.idle_secs, watch.cooldown_secs and watch.idle_notice_secs must be 10..=86400"
        );
        ensure!(
            self.watch
                .max_wakes
                .is_none_or(|w| (1..=MAX_WAKES_LIMIT).contains(&w)),
            "watch.max_wakes must be 1..={MAX_WAKES_LIMIT}"
        );
        ensure!(
            self.watch
                .reconcile_secs
                .is_none_or(|s| (10..=300).contains(&s)),
            "watch.reconcile_secs must be 10..=300"
        );
        self.factory.validate()?;
        let mut policy = serde_json::to_value(LoopPolicy::default())?;
        merge(&mut policy, serde_json::to_value(&self.loop_defaults)?);
        serde_json::from_value::<LoopPolicy>(policy)?.validate()
    }
    /// Whether factory mode is on (`factory.enabled: true`).
    pub(crate) fn factory_enabled(&self) -> bool {
        self.factory.enabled == Some(true)
    }
    /// Refuses a factory command unless `factory.enabled` is true.
    pub(crate) fn require_factory(&self) -> Result<()> {
        ensure!(
            self.factory.enabled == Some(true),
            "factory mode is off; set factory.enabled: true in the user config ({}) or in this project's .hgm/config.yaml to use the hgm queue commands",
            user_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "~/.config/herdr-goal-manager/config.yaml".into())
        );
        Ok(())
    }
    /// The manager guide: built-in or replaced, then appended sections.
    pub(crate) fn guide(&self) -> String {
        let mut guide = self
            .guide
            .clone()
            .unwrap_or_else(|| BUILTIN_GUIDE.to_string());
        // The queue section joins only where factory mode is on, so the
        // guide stays the same for everyone else.
        if self.factory_enabled() {
            guide += "\n\n";
            guide += FACTORY_GUIDE.trim_end();
        }
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
        self.watch.idle_secs.unwrap_or(DEFAULT_IDLE_SECS)
    }
    pub(crate) fn cooldown_secs(&self) -> u64 {
        self.watch.cooldown_secs.unwrap_or(DEFAULT_COOLDOWN_SECS)
    }
    pub(crate) fn idle_notice_secs(&self) -> u64 {
        self.watch
            .idle_notice_secs
            .unwrap_or(DEFAULT_IDLE_NOTICE_SECS)
    }
    pub(crate) fn max_wakes(&self) -> u32 {
        self.watch.max_wakes.unwrap_or(DEFAULT_MAX_WAKES)
    }
    /// Default 30 s: a closed pane and dropped events show up within this time.
    pub(crate) fn reconcile_secs(&self) -> u64 {
        self.watch.reconcile_secs.unwrap_or(30)
    }
    /// Whether hgm sets a native goal on a Claude Code Goal Manager (default on).
    pub(crate) fn manager_native_goal(&self) -> bool {
        self.manager.native_goal.unwrap_or(true)
    }
    /// Whether the Goal watch tab shows its running sheep (default on).
    pub(crate) fn sheep(&self) -> bool {
        self.watch.sheep.unwrap_or(true)
    }
}

/// Builds a goal spec from a parsed goal file, filling `loop` fields the goal
/// does not set from the configured defaults.
pub(crate) fn goal_spec(workspace: &Path, raw: Value) -> Result<GoalSpec> {
    goal_spec_with(raw, &load(workspace)?.loop_defaults)
}
/// Builds a goal spec from a parsed goal file and the given `loop` defaults,
/// such as those recorded when a contract was queued.
pub(crate) fn goal_spec_with(mut raw: Value, defaults: &LoopDefaults) -> Result<GoalSpec> {
    let defaults = serde_json::to_value(defaults)?;
    let Some(object) = raw.as_object_mut() else {
        bail!("a goal must be a mapping with objective and acceptance");
    };
    if let Some(policy) = object
        .entry("loop")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    {
        fill(policy, defaults);
        // A new contract sets the native goal with each assignment; a stored
        // one without the field keeps setting it after the first settle.
        policy
            .entry("native_goal")
            .or_insert_with(|| json!("assignment"));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn factory(enabled: Option<bool>, review: Option<u32>, active: Option<u32>) -> FactoryDefaults {
        FactoryDefaults {
            enabled,
            max_awaiting_review: review,
            max_active: active,
            checkout: None,
        }
    }

    #[test]
    fn factory_mode_is_off_with_limits_of_two_reviews_and_one_active_goal() {
        let config = Config::default();
        assert!(config.require_factory().is_err());
        assert!(format!("{:#}", config.require_factory().unwrap_err()).contains("factory.enabled"));
        assert_eq!(config.factory.max_awaiting_review(), 2);
        assert_eq!(config.factory.max_active(), 1);
        assert!(config.factory.is_unset());
    }

    #[test]
    fn a_project_may_switch_factory_mode_and_only_lower_its_limits() -> Result<()> {
        let mut merged = FactoryDefaults::default();
        merged.apply(factory(Some(true), Some(4), None), true)?;
        merged.apply(factory(Some(false), Some(3), Some(1)), false)?;
        assert_eq!(merged, factory(Some(false), Some(3), Some(1)));
        let raise = merged.apply(factory(None, Some(5), None), false);
        assert!(format!("{:#}", raise.unwrap_err()).contains("factory.max_awaiting_review"));
        let mut defaults = FactoryDefaults::default();
        let raise = defaults.apply(factory(Some(true), None, Some(2)), false);
        assert!(format!("{:#}", raise.unwrap_err()).contains("at most 1"));
        let mut user = FactoryDefaults::default();
        user.apply(factory(None, None, Some(3)), true)?;
        user.apply(factory(Some(true), None, Some(2)), false)?;
        assert_eq!(user.max_active(), 2);
        Ok(())
    }

    #[test]
    fn factory_limits_are_bounded() {
        assert!(factory(None, Some(0), None).validate().is_err());
        assert!(factory(None, Some(21), None).validate().is_err());
        assert!(factory(None, None, Some(11)).validate().is_err());
        assert!(factory(Some(true), Some(20), Some(10)).validate().is_ok());
    }

    #[test]
    fn the_watch_times_default_to_three_minutes_and_stay_configurable() {
        let config = Config::default();
        assert_eq!(
            (
                config.idle_secs(),
                config.cooldown_secs(),
                config.idle_notice_secs(),
                config.max_wakes()
            ),
            (180, 180, 180, 160)
        );
        assert_eq!(LoopPolicy::default().patrol_minutes, 3);
        let mut config = Config::default();
        config.watch.max_wakes = Some(500);
        config.watch.idle_notice_secs = Some(600);
        assert!(config.validate().is_ok());
        assert_eq!(config.idle_notice_secs(), 600);
        config.watch.max_wakes = Some(501);
        assert!(config.validate().is_err());
        config.watch.max_wakes = None;
        config.watch.idle_notice_secs = Some(5);
        assert!(format!("{:#}", config.validate().unwrap_err()).contains("idle_notice_secs"));
    }

    #[test]
    fn the_guide_makes_keeping_main_working_part_of_the_managers_goal() {
        for rule in [
            "Keeping main working toward the confirmed goal is part of your goal",
            "look at main first",
            "--prepare",
            "approve-plan GOAL --task TASK for each task",
            "only when a human",
            "wait GOAL --reason TEXT",
            "Work autonomously inside the contract",
            "stay exactly as they are",
        ] {
            assert!(BUILTIN_GUIDE.contains(rule), "the guide lacks {rule:?}");
        }
        let spec: GoalSpec = serde_norway::from_str("objective: o\nacceptance: [a]\n").unwrap();
        assert!(
            spec.assignment_contract().contains(
                "Decide choices inside this contract yourself and report them afterwards"
            )
        );
    }

    #[test]
    fn each_guide_stays_within_its_word_budget() {
        // The manager guide may not grow past its length before the
        // 2026-11-15 additions: four goals added paragraphs one at a time that
        // day, so a new rule must replace words, not add them. The other
        // budgets sit just above the 2026-11-16 revision.
        const MANAGER: usize = 2_135;
        const IMPL: usize = 300;
        const REVIEWER: usize = 340;
        const FACTORY: usize = 320;
        let words = |text: &str| text.split_whitespace().count();
        for (name, guide, budget) in [
            ("manager", BUILTIN_GUIDE, MANAGER),
            ("impl", crate::roles::RoleKind::Impl.guide(), IMPL),
            (
                "reviewer",
                crate::roles::RoleKind::Reviewer.guide(),
                REVIEWER,
            ),
            ("factory", FACTORY_GUIDE, FACTORY),
        ] {
            assert!(
                words(guide) <= budget,
                "the {name} guide has {} words (budget {budget})",
                words(guide)
            );
        }
    }

    #[test]
    fn the_factory_guide_joins_only_in_factory_mode() {
        let off = Config::default();
        assert_eq!(off.guide(), BUILTIN_GUIDE);
        let mut on = Config::default();
        on.factory.enabled = Some(true);
        let guide = on.guide();
        assert!(guide.starts_with(BUILTIN_GUIDE));
        for rule in [
            "hgm queue next",
            "accepted or cancelled",
            "hgm send --goal GOAL --task TASK",
        ] {
            assert!(guide.contains(rule), "missing {rule:?}");
        }
    }
}
