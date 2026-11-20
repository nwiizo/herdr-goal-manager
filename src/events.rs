use crate::model::{AttemptStatus, Goal};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::process::{Child, Stdio};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Event {
    pub at: DateTime<Utc>,
    pub goal: String,
    pub kind: String,
    #[serde(default)]
    pub detail: Value,
}
impl Event {
    pub fn new(goal: &str, kind: &str, detail: Value) -> Self {
        Self {
            at: Utc::now(),
            goal: goal.into(),
            kind: kind.into(),
            detail,
        }
    }
}

pub(crate) fn transitions(before: Option<&Goal>, after: &Goal) -> Vec<Event> {
    let event = |kind: &str, detail: Value| Event::new(&after.id, kind, detail);
    let Some(before) = before else {
        return vec![event(
            "registered",
            json!({
                "objective": after.spec.objective,
                "owner": after.owner,
                "tasks": after.tasks.iter().map(|t| &t.spec.id).collect::<Vec<_>>(),
                "checks": after.spec.checks.len(),
            }),
        )];
    };
    let mut events = vec![];
    if after.tasks.len() > before.tasks.len() {
        let added: Vec<_> = after.tasks[before.tasks.len()..]
            .iter()
            .map(|t| &t.spec.id)
            .collect();
        events.push(event("tasks_planned", json!({ "tasks": added })));
    }
    for (old, new) in before.tasks.iter().zip(&after.tasks) {
        match (old.succeeded(), new.succeeded()) {
            (false, true) => events.push(event(
                "task_completed",
                json!({
                    "task": new.spec.id,
                    "evidence": new.manual_evidence.as_ref().map(|e| &e.note),
                }),
            )),
            (true, false) => events.push(event("task_reopened", json!({ "task": new.spec.id }))),
            _ => {}
        }
    }
    for guidance in after.guidance.iter().skip(before.guidance.len()) {
        events.push(event("guidance", json!({ "note": guidance.note })));
    }
    let finished = |goal: &Goal| {
        goal.verifications
            .iter()
            .filter(|v| v.finished_at.is_some())
            .count()
    };
    if finished(after) > finished(before)
        && let Some(verification) = after
            .verifications
            .iter()
            .rev()
            .find(|v| v.finished_at.is_some())
    {
        events.push(event(
            "checks",
            json!({
                "count": verification.results.len(),
                "passed": verification
                    .results
                    .iter()
                    .all(|r| r.attempt.status == AttemptStatus::Succeeded),
                "results": verification
                    .results
                    .iter()
                    .map(|r| json!({ "name": r.name, "status": r.attempt.status }))
                    .collect::<Vec<_>>(),
            }),
        ));
    }
    if before.confirmed.is_none()
        && let Some(confirmed) = &after.confirmed
    {
        events.push(event("confirmed", json!({ "evidence": confirmed.note })));
    }
    if before.waiting.is_none()
        && let Some(waiting) = &after.waiting
    {
        events.push(event("waiting", json!({ "reason": waiting.note })));
    }
    if before.acceptance.is_none()
        && let Some(acceptance) = &after.acceptance
    {
        events.push(event("accepted", json!({ "evidence": acceptance.note })));
    }
    if before.status != after.status {
        events.push(event(
            "status",
            json!({ "from": before.status, "to": after.status, "message": after.message }),
        ));
    }
    events
}

pub(crate) fn notifies_person(event: &Event) -> bool {
    attention(event).is_some()
}
fn attention(event: &Event) -> Option<(&'static str, String, &'static str)> {
    let text = |key: &str| event.detail[key].as_str().unwrap_or_default().to_string();
    match event.kind.as_str() {
        "status" => match event.detail["to"].as_str()? {
            "ready_for_review" => Some(("Goal ready for your review", text("message"), "request")),
            "needs_input" => Some(("Goal needs your input", text("message"), "request")),
            _ => None,
        },
        "waiting" => Some(("Goal is waiting for you", text("reason"), "request")),
        "attention" if event.detail["cause"] == crate::watch::IDLE_NOTICE => {
            Some(("Worker idle short of completion", text("reason"), "request"))
        }
        "attention" if event.detail["cause"] == crate::manager_goal::SPINNING => {
            Some(("Goal Manager's /goal cleared", text("reason"), "request"))
        }
        "attention" => Some(("Original agent needs you", text("reason"), "request")),
        "watch_ended" if event.detail["notify"] == true => {
            Some(("Goal monitoring stopped", text("reason"), "request"))
        }
        "accepted" => Some(("Goal accepted", text("evidence"), "done")),
        "reviewed" => Some((
            "Review arrived",
            format!(
                "{} says {} with {} finding(s)",
                text("reviewer"),
                text("verdict"),
                event.detail["findings"].as_array().map_or(0, Vec::len)
            ),
            "request",
        )),
        _ => None,
    }
}

pub(crate) fn notice(event: &Event) -> Option<String> {
    if let Some((title, reason, _)) = attention(event) {
        return Some(if reason.is_empty() {
            title.into()
        } else {
            format!("{title}: {reason}")
        });
    }
    (event.kind == "wake").then(|| {
        let reason = event.detail["reason"].as_str().unwrap_or_default();
        format!("Woke the manager: {reason}")
    })
}

pub(crate) fn notify(objective: &str, events: &[Event]) {
    if cfg!(test)
        || std::env::var("HERDR_ENV").as_deref() != Ok("1")
        || std::env::var_os("HGM_CONTEXT").is_none()
    {
        return;
    }
    for event in events {
        let Some((title, reason, sound)) = attention(event) else {
            continue;
        };
        let body: String = format!("{objective} — {reason}")
            .chars()
            .take(300)
            .collect();
        let binary = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
        let spawned = std::process::Command::new(binary)
            .args(["notification", "show", title, "--body", &body])
            .args(["--sound", sound])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => wait_bounded(&mut child),
            Err(error) => eprintln!("Goal notification not shown: {error}"),
        }
    }
}
fn wait_bounded(child: &mut Child) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

pub(crate) fn since(text: &str) -> Result<DateTime<Utc>> {
    if let Ok(time) = DateTime::parse_from_rfc3339(text) {
        return Ok(time.with_timezone(&Utc));
    }
    let (number, unit) = text.split_at(text.len().saturating_sub(1));
    let amount: i64 = number
        .parse()
        .with_context(|| format!("--since must be RFC 3339 or like 30m/2h/1d, got {text}"))?;
    let span = match unit {
        "m" => Duration::minutes(amount),
        "h" => Duration::hours(amount),
        "d" => Duration::days(amount),
        _ => bail!("--since unit must be m, h, or d"),
    };
    Ok(Utc::now() - span)
}

pub(crate) fn full_text(event: &Event) -> Option<&str> {
    (event.kind == "report")
        .then(|| event.detail["text"].as_str())
        .flatten()
        .filter(|text| text.contains('\n') || text.chars().count() > 120)
}
pub(crate) fn local_time(at: DateTime<Utc>) -> String {
    at.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S %:z")
        .to_string()
}
pub(crate) fn local_clock(at: DateTime<Utc>) -> String {
    at.with_timezone(&chrono::Local).format("%H:%M").to_string()
}
pub(crate) fn describe(event: &Event) -> String {
    format!(
        "{}  {}  {:<14} {}",
        local_time(event.at),
        &event.goal[..event.goal.len().min(10)],
        event.kind,
        summary(event)
    )
}
pub(crate) fn summary(event: &Event) -> String {
    let d = &event.detail;
    let text = |key: &str| d[key].as_str().unwrap_or_default().to_string();
    let summary = match event.kind.as_str() {
        "registered" => text("objective"),
        "status" => format!(
            "{} -> {}{}",
            text("from"),
            text("to"),
            d["message"]
                .as_str()
                .map(|m| format!(": {m}"))
                .unwrap_or_default()
        ),
        "task_completed" | "task_reopened" => format!(
            "{}{}",
            text("task"),
            d["evidence"]
                .as_str()
                .map(|e| format!(": {e}"))
                .unwrap_or_default()
        ),
        "assigned" => format!("{}: {}", text("task"), text("text")),
        "checks" => format!("{} checks, passed={}", d["count"], d["passed"]),
        "guidance" => text("note"),
        "waiting" | "attention" | "watch_ended" => text("reason"),
        "accepted" | "confirmed" => text("evidence"),
        "patrol" if d["quiet"] == true => {
            format!("{} (progressing, nothing found: no wake)", text("reason"))
        }
        "patrol" => text("reason"),
        "prepared" => format!("{}: {}", text("task"), text("text")),
        "prepared_held" | "prepared_withdrawn" => text("reason"),
        "provider_error" | "provider_continue" => text("line"),
        "decision" => format!(
            "{}{}: {} (why: {})",
            text("by"),
            d["task"]
                .as_str()
                .map(|t| format!(" [{t}]"))
                .unwrap_or_default(),
            text("what"),
            text("why")
        ),
        "native_goal_queued" => format!(
            "queued with the assignment for {}; pending until Goal set: shows",
            text("task")
        ),
        "manager_goal_set" => match (d["delivery"].as_str(), d["via"].as_str()) {
            (Some(_), _) => format!("not confirmed: {}", text("why")),
            (_, Some("queued")) if d["pending"] == true => {
                "queued on the Goal Manager; pending until Goal set: shows".into()
            }
            _ => "set on the Goal Manager".into(),
        },
        "manager_goal" => format!("{}: {}", text("outcome"), text("line")),
        "manager_goal_cleared" => text("reason"),
        "cross_check_started" => format!("{} checks {}", text("reviewer"), text("task")),
        "cross_checked" => format!(
            "{} says {} ({} findings)",
            text("reviewer"),
            text("verdict"),
            d["findings"].as_array().map_or(0, Vec::len)
        ),
        "refused" => format!("{} to {}: {}", text("command"), text("to"), text("reason")),
        "report" => format!(
            "{}{}: {}",
            text("from"),
            d["task"]
                .as_str()
                .map(|t| format!(" [{t}]"))
                .unwrap_or_default(),
            text("text").lines().next().unwrap_or_default()
        ),
        "follow_up" => format!(
            "{}{}: {}",
            text("by"),
            d["task"]
                .as_str()
                .map(|t| format!(" [{t}]"))
                .unwrap_or_default(),
            text("text")
        ),
        "no_progress" => d["retry"].as_str().map_or_else(
            || "no retry notice".into(),
            |line| format!("retrying: {line}"),
        ),
        "watch_joined" => format!(
            "shares the watch on {} ({} goals)",
            text("pane"),
            d["goals"].as_array().map_or(0, Vec::len)
        ),
        "settle_checks" => {
            let names = |key: &str, name: &dyn Fn(&Value) -> Option<&str>| {
                d[key]
                    .as_array()
                    .map(|items| items.iter().filter_map(name).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default()
            };
            format!(
                "passed: {}; failed: {}",
                names("passed", &|v| v.as_str()),
                names("failed", &|v| v["name"].as_str())
            )
        }
        _ => d.to_string(),
    };
    summary.chars().take(200).collect()
}
