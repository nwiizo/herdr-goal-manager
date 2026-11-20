use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Autonomy {
    #[default]
    Auto,
    Ask,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FlagOrName {
    Flag(bool),
    Name(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeGoalMode {
    Assignment,
    Settle,
    Off,
}
impl Serialize for NativeGoalMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Assignment => serializer.serialize_str("assignment"),
            Self::Settle => serializer.serialize_str("settle"),
            Self::Off => serializer.serialize_bool(false),
        }
    }
}
impl<'de> Deserialize<'de> for NativeGoalMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match FlagOrName::deserialize(deserializer)? {
            FlagOrName::Flag(true) => Ok(Self::Settle),
            FlagOrName::Flag(false) => Ok(Self::Off),
            FlagOrName::Name(name) => match name.as_str() {
                "assignment" => Ok(Self::Assignment),
                "settle" => Ok(Self::Settle),
                "off" => Ok(Self::Off),
                other => Err(serde::de::Error::custom(format!(
                    "loop.native_goal must be assignment, settle or false, not {other}"
                ))),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CrossCheck {
    #[default]
    Off,
    Auto,
    Codex,
    Claude,
}
impl CrossCheck {
    fn is_off(&self) -> bool {
        *self == Self::Off
    }
}
impl Serialize for CrossCheck {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Off => serializer.serialize_bool(false),
            Self::Auto => serializer.serialize_str("auto"),
            Self::Codex => serializer.serialize_str("codex"),
            Self::Claude => serializer.serialize_str("claude"),
        }
    }
}
impl<'de> Deserialize<'de> for CrossCheck {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match FlagOrName::deserialize(deserializer)? {
            FlagOrName::Flag(true) => Ok(Self::Auto),
            FlagOrName::Flag(false) => Ok(Self::Off),
            FlagOrName::Name(name) => match name.as_str() {
                "auto" | "on" => Ok(Self::Auto),
                "off" => Ok(Self::Off),
                "codex" => Ok(Self::Codex),
                "claude" => Ok(Self::Claude),
                other => Err(serde::de::Error::custom(format!(
                    "loop.cross_check must be false, true, auto, codex or claude, not {other}"
                ))),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PatrolWake {
    #[default]
    Finding,
    Always,
}
impl PatrolWake {
    fn is_finding(&self) -> bool {
        *self == Self::Finding
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LoopPolicy {
    /// Linked assignments per task (or per goal without a task) before escalating.
    /// A human revision starts a new budget for the reopened task.
    pub max_assignments: u32,
    /// Hours after the first assignment before monitoring escalates.
    pub max_hours: Option<u32>,
    /// Minutes between drift checks while the original agent works; 0 disables.
    pub patrol_minutes: u32,
    pub autonomy: Autonomy,
    /// Human-written abort conditions checked at every wake (e.g. "the fix
    /// requires changing packages/auth").
    pub stop_when: Vec<String>,
    /// Original-agent context use that triggers handoff and restart.
    pub handoff_percent: u8,
    /// Each task needs an approved plan before implementation assignments.
    pub plan_first: bool,
    /// Machine-checked abort conditions run before each wake; exit 0 stops.
    pub stop_checks: Vec<StopCheck>,
    /// Corrections per task (since its last reopen) before the user is asked.
    pub max_corrections: u32,
    /// Run the goal's command checks when the worker settles after new work,
    /// so a missed acceptance is corrected instead of waiting for review.
    pub check_on_settle: bool,
    /// Minutes of work between goal checks (the worker restates the goal so
    /// long work does not drift); 30..=60.
    pub reanchor_minutes: u32,
    /// When to set the provider's native goal (`/goal`) on a Claude Code
    /// worker under autonomy auto: with each linked assignment (`assignment`,
    /// written into new contracts), after the worker first settles (`settle`,
    /// what an unset or `true` field of a stored contract means), or never
    /// (`false`). Only Claude Code has it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_goal: Option<NativeGoalMode>,
    /// `hgm run` moves the goal to review only with a current approving
    /// verdict from a reviewer running another provider than the workers,
    /// and while no reviewer's latest current verdict asks for a revision.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub require_review: bool,
    /// Whether a patrol without findings wakes the manager (`always`) or only records progress (`finding`).
    #[serde(skip_serializing_if = "PatrolWake::is_finding")]
    pub patrol_wake: PatrolWake,
    /// Whether another provider checks the watched worker's work, read-only,
    /// each time it settles after new work: `false` (the default), `true` or
    /// `auto` (Codex checks Claude Code, Claude Code checks the rest), `codex`
    /// or `claude`. Its verdict wakes the manager; it is an observation only.
    #[serde(skip_serializing_if = "CrossCheck::is_off")]
    pub cross_check: CrossCheck,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StopCheck {
    pub name: String,
    pub command: String,
    #[serde(default = "stop_check_timeout")]
    pub timeout_secs: u64,
}
fn stop_check_timeout() -> u64 {
    60
}
impl Default for LoopPolicy {
    fn default() -> Self {
        Self {
            max_assignments: 3,
            max_hours: None,
            patrol_minutes: 3,
            autonomy: Autonomy::Auto,
            stop_when: vec![],
            handoff_percent: 85,
            plan_first: false,
            stop_checks: vec![],
            max_corrections: 5,
            check_on_settle: true,
            reanchor_minutes: 45,
            native_goal: None,
            require_review: false,
            patrol_wake: PatrolWake::Finding,
            cross_check: CrossCheck::Off,
        }
    }
}
impl LoopPolicy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=100).contains(&self.max_assignments),
            "loop.max_assignments must be 1..=100"
        );
        ensure!(
            self.max_hours
                .is_none_or(|hours| (1..=720).contains(&hours)),
            "loop.max_hours must be 1..=720"
        );
        ensure!(
            self.patrol_minutes <= 1440,
            "loop.patrol_minutes must be 0..=1440"
        );
        ensure!(
            (30..=95).contains(&self.handoff_percent),
            "loop.handoff_percent must be 30..=95"
        );
        ensure!(
            self.stop_when.iter().all(|s| !s.trim().is_empty()),
            "loop.stop_when entries must not be empty"
        );
        ensure!(
            self.stop_checks.iter().all(|check| {
                !check.name.trim().is_empty()
                    && !check.command.trim().is_empty()
                    && check.timeout_secs > 0
            }),
            "loop.stop_checks need a name, a command, and a positive timeout"
        );
        ensure!(
            (1..=50).contains(&self.max_corrections),
            "loop.max_corrections must be 1..=50"
        );
        ensure!(
            (30..=60).contains(&self.reanchor_minutes),
            "loop.reanchor_minutes must be 30..=60"
        );
        Ok(())
    }
}

pub(crate) fn protected_digest(workspace: &Path, paths: &[String]) -> Result<String> {
    let mut hasher = Sha256::new();
    for prefix in paths {
        let mut files = vec![];
        collect_files(&workspace.join(prefix), &mut files)?;
        files.sort();
        hasher.update(prefix.as_bytes());
        hasher.update([0]);
        for file in files {
            let relative = file.strip_prefix(workspace).unwrap_or(&file);
            let link = file.symlink_metadata()?.is_symlink();
            if link {
                hasher.update(b"\0symlink\0");
            }
            hasher.update(relative.as_os_str().as_encoded_bytes());
            hasher.update([0]);
            if link {
                hasher.update(std::fs::read_link(&file)?.as_os_str().as_encoded_bytes());
            } else {
                hasher.update(
                    std::fs::read(&file).with_context(|| format!("read {}", file.display()))?,
                );
            }
            hasher.update([0]);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            collect_files(&entry?.path(), files)?;
        }
    } else if metadata.is_file() || metadata.is_symlink() {
        files.push(path.to_path_buf());
    }
    Ok(())
}

#[cfg(test)]
mod protected_path_tests {
    use super::*;

    #[test]
    fn a_protected_link_tracks_its_destination_even_when_dangling() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let link = dir.path().join("settings");
        let digest = || protected_digest(dir.path(), &["settings".into()]);
        let missing = digest()?;
        std::os::unix::fs::symlink("v1", &link)?;
        let first = digest()?;
        assert_ne!(first, missing);
        std::fs::remove_file(&link)?;
        std::os::unix::fs::symlink("v2", &link)?;
        assert_ne!(digest()?, first);
        std::fs::remove_file(link)?;
        assert_eq!(digest()?, missing);
        Ok(())
    }
}
