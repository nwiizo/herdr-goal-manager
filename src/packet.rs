//! What a reviewer of unattended work needs in one place: the worker's own
//! records (choices it made inside the contract, findings that do not block)
//! and the review packet that gathers them with the acceptance items, the
//! latest check results and the verdicts. None of these completes a task or
//! accepts a goal; hgm's checks and the user's recorded acceptance decide.
use crate::{
    admission::CORRECTION_WORD_BUDGET,
    events::Event,
    model::{AttemptStatus, Goal, GoalStatus, word_units},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

/// Timeline kind of a choice the worker made inside the contract.
pub(crate) const DECISION: &str = "decision";
/// Timeline kind of a finding that does not block the work.
pub(crate) const FOLLOW_UP: &str = "follow_up";
/// The guidance prefix `hgm revise` writes; each one is a revision.
const REVISION: &str = "Revision requested: ";

/// Who runs this hgm: a helper role by name (refused unless its kind may run
/// `command`), the manager (its session sets HGM_CONTEXT), or main.
pub(crate) fn recorder(store: &Store, command: &str) -> Result<String> {
    if let Some(role) = crate::roles::caller(store, command)? {
        return Ok(role);
    }
    Ok(if std::env::var_os("HGM_CONTEXT").is_some() {
        "manager".into()
    } else {
        "main".into()
    })
}

/// The goal a record belongs to: live, and with `task` among its tasks.
pub(crate) fn record_target(store: &Store, id: &str, task: Option<&str>) -> Result<Goal> {
    let goal = store.load(id)?;
    ensure!(
        !matches!(goal.status, GoalStatus::Completed | GoalStatus::Cancelled),
        "goal {id} is {:?}; records belong to a live goal",
        goal.status
    );
    if let Some(task) = task {
        ensure!(
            goal.tasks.iter().any(|t| t.spec.id == task),
            "unknown task ID {task}"
        );
    }
    Ok(goal)
}

/// One part of a record: present and short enough to read later.
fn bounded(label: &str, text: &str) -> Result<()> {
    let words = word_units(text);
    ensure!(
        (1..=CORRECTION_WORD_BUDGET).contains(&words),
        "{label} takes 1..{CORRECTION_WORD_BUDGET} words; record one choice or finding at a time"
    );
    Ok(())
}

/// Records a choice the worker made inside the contract, and why.
pub(crate) fn decide(
    store: &Store,
    id: &str,
    task: Option<&str>,
    what: &str,
    why: &str,
) -> Result<Value> {
    bounded("the decision", what)?;
    bounded("--why", why)?;
    let by = recorder(store, "decision")?;
    record_target(store, id, task)?;
    store.record(
        id,
        DECISION,
        json!({"by": by, "task": task, "what": what, "why": why}),
    )?;
    Ok(json!({"recorded": DECISION, "goal": id, "task": task, "by": by}))
}

/// Records a finding that should not block the work, to take up later.
pub(crate) fn follow_up(store: &Store, id: &str, task: Option<&str>, text: &str) -> Result<Value> {
    bounded("the follow-up", text)?;
    let by = recorder(store, "follow-up")?;
    record_target(store, id, task)?;
    store.record(id, FOLLOW_UP, json!({"by": by, "task": task, "text": text}))?;
    Ok(json!({"recorded": FOLLOW_UP, "goal": id, "task": task, "by": by}))
}

/// The latest result of one registered check, from `hgm run`/`hgm accept`
/// verifications and the watch's settle checks, whichever is newer, with the
/// fingerprint of the work dir it ran on when one was kept.
struct CheckView {
    result: &'static str,
    at: Option<DateTime<Utc>>,
    source: &'static str,
    detail: String,
    fingerprint: Option<String>,
}
fn latest_check(goal: &Goal, events: &[Event], name: &str) -> CheckView {
    let mut latest = CheckView {
        result: "not_run",
        at: None,
        source: "",
        detail: String::new(),
        fingerprint: None,
    };
    let mut consider = |view: CheckView| {
        if view.at > latest.at {
            latest = view;
        }
    };
    for verification in &goal.verifications {
        for result in verification.results.iter().filter(|r| r.name == name) {
            let result_name = match result.attempt.status {
                AttemptStatus::Succeeded => "passed",
                AttemptStatus::Failed => "failed",
                AttemptStatus::Interrupted => "interrupted",
                AttemptStatus::Running => continue,
            };
            consider(CheckView {
                result: result_name,
                at: result.attempt.finished_at,
                source: "run",
                detail: result.detail.chars().take(300).collect(),
                fingerprint: verification.fingerprint.clone(),
            });
        }
    }
    for event in events.iter().filter(|e| e.kind == "settle_checks") {
        let passed = event.detail["passed"]
            .as_array()
            .is_some_and(|names| names.iter().any(|n| n == name));
        let failed = event.detail["failed"]
            .as_array()
            .and_then(|items| items.iter().find(|f| f["name"] == name));
        let (result, detail) = match (passed, failed) {
            (true, _) => ("passed", String::new()),
            (false, Some(failed)) => (
                "failed",
                failed["detail"].as_str().unwrap_or_default().to_string(),
            ),
            (false, None) => continue,
        };
        consider(CheckView {
            result,
            at: Some(event.at),
            source: "settle",
            detail,
            fingerprint: event.detail["fingerprint"].as_str().map(String::from),
        });
    }
    latest
}

/// Each registered check's latest result, with `state`: `current` when the
/// work dir still has the fingerprint the check ran on, `stale` when it
/// changed since, `unknown` when no fingerprint was kept (records written
/// before it was) or the work dir cannot be read.
pub(crate) fn check_states(
    goal: &Goal,
    events: &[Event],
    fingerprints: &mut crate::fingerprint::Fingerprints,
) -> Vec<Value> {
    let last_work = events
        .iter()
        .rev()
        .find(|e| matches!(e.kind.as_str(), "assigned" | "correction"))
        .map(|e| e.at);
    let mut now: Option<Option<String>> = None;
    goal.spec
        .checks
        .iter()
        .map(|check| {
            let view = latest_check(goal, events, &check.name);
            let state = match &view.fingerprint {
                _ if view.result == "not_run" => None,
                None => Some("unknown"),
                Some(ran_on) => {
                    let now = now.get_or_insert_with(|| {
                        goal.work_dir().ok().and_then(|dir| fingerprints.of(dir))
                    });
                    Some(match now {
                        Some(now) if now == ran_on => "current",
                        Some(_) => "stale",
                        None => "unknown",
                    })
                }
            };
            json!({
                "name": check.name,
                "result": view.result,
                "at": view.at,
                "source": (!view.source.is_empty()).then_some(view.source),
                "detail": (!view.detail.is_empty()).then_some(view.detail),
                "state": state,
                "before_last_assignment": view.at.zip(last_work).is_some_and(|(at, work)| at < work),
            })
        })
        .collect()
}

/// Why a check's latest result does not verify the current work dir, or
/// `None` when it passed on the state the work dir still has.
pub(crate) fn check_unverified(check: &Value) -> Option<String> {
    let name = check["name"].as_str().unwrap_or_default();
    let at = serde_json::from_value::<DateTime<Utc>>(check["at"].clone())
        .map(when)
        .unwrap_or_default();
    match (check["result"].as_str(), check["state"].as_str()) {
        (Some("not_run"), _) => Some(format!("check {name} has no recorded result")),
        (Some("passed"), Some("current")) => None,
        (Some("passed"), Some("stale")) => Some(format!(
            "check {name} passed at {at} on an earlier state of the work dir"
        )),
        (Some("passed"), _) => Some(format!(
            "check {name} passed at {at}; whether the work dir changed since is unknown (no fingerprint was kept)"
        )),
        (result, _) => Some(format!(
            "check {name} {} at {at}",
            result.unwrap_or("failed")
        )),
    }
}

fn when(at: DateTime<Utc>) -> String {
    crate::events::local_time(at)
}

/// The review packet of `goal` as data; `hgm packet` prints it or its JSON.
pub(crate) fn build(store: &Store, goal: &Goal) -> Result<Value> {
    let events = store.events(&goal.id)?;
    let count = |kind: &str| events.iter().filter(|e| e.kind == kind).count();
    let mut fingerprints = crate::fingerprint::Fingerprints::default();
    let mut not_verified: Vec<Value> = vec![];
    let mut unverified = |at: Option<DateTime<Utc>>, kind: &str, what: String| {
        not_verified.push(json!({"at": at, "kind": kind, "what": what}));
    };
    let checks = check_states(goal, &events, &mut fingerprints);
    for check in &checks {
        if let Some(what) = check_unverified(check) {
            unverified(
                serde_json::from_value(check["at"].clone()).ok(),
                "check",
                what,
            );
        }
    }
    let mut decisions = vec![];
    let mut reports = vec![];
    let mut follow_ups = vec![];
    let mut outside_scope = vec![];
    let mut latest_by_reviewer: std::collections::BTreeMap<String, &Event> = Default::default();
    for event in &events {
        let d = &event.detail;
        match event.kind.as_str() {
            DECISION => decisions.push(json!({
                "at": event.at, "by": d["by"], "task": d["task"],
                "what": d["what"], "why": d["why"],
            })),
            "report" => reports.push(json!({
                "at": event.at, "from": d["from"], "task": d["task"], "text": d["text"],
            })),
            FOLLOW_UP => follow_ups.push(json!({
                "at": event.at, "by": d["by"], "task": d["task"], "text": d["text"],
                "source": "worker",
            })),
            "reviewed" => {
                let findings = d["findings"].as_array().map_or(&[][..], Vec::as_slice);
                for finding in findings.iter().filter(|f| f["severity"] != "blocking") {
                    follow_ups.push(json!({
                        "at": event.at, "by": d["reviewer"], "task": null,
                        "text": finding["issue"], "where": finding["where"],
                        "evidence": finding["evidence"], "source": "review", "review": d["review"],
                    }));
                }
                if let Some(reviewer) = d["reviewer"].as_str() {
                    latest_by_reviewer.insert(reviewer.into(), event);
                }
            }
            crate::scope::SCOPE_EXCEEDED => outside_scope.push(json!({
                "at": event.at, "task": d["task"], "by": d["role"],
                "paths": d["paths"], "write_paths": d["write_paths"],
            })),
            "boundary_crossed" => unverified(
                Some(event.at),
                "protected_paths",
                format!(
                    "protected paths changed while the worker worked: {}",
                    d["paths"]
                ),
            ),
            "prepared_delivery_uncertain" => unverified(
                Some(event.at),
                "delivery",
                format!(
                    "the prepared step for {} may not have arrived",
                    d["to"].as_str().unwrap_or("main")
                ),
            ),
            kind if d["delivery"] == "uncertain" => unverified(
                Some(event.at),
                "delivery",
                format!("{kind} may not have arrived (delivery uncertain)"),
            ),
            // A send whose input line did not empty, and a wake that went
            // the same way: both may still sit unread.
            kind if d["consumed"] == false => unverified(
                Some(event.at),
                "delivery",
                format!("{kind} may still sit in the input (input not consumed)"),
            ),
            "wake"
                if d["via"]
                    .as_str()
                    .is_some_and(|via| via.ends_with("_unconfirmed")) =>
            {
                unverified(
                    Some(event.at),
                    "delivery",
                    "a wake may still sit in the manager's input (not confirmed)".into(),
                )
            }
            _ => {}
        }
    }
    let reviews = crate::roles::review_states(goal, &events, &mut fingerprints);
    for review in reviews.iter().filter(|r| r["fresh"] != "current") {
        let at = serde_json::from_value(review["at"].clone()).ok();
        let name = review["review"].as_str().unwrap_or_default();
        let verdict = review["verdict"].as_str().unwrap_or_default();
        unverified(
            at,
            "review",
            if review["fresh"] == "stale" {
                format!("review {name} ({verdict}) judged an earlier state of the work dir")
            } else {
                format!("review {name} ({verdict}) cannot be compared: the work dir is unreadable")
            },
        );
    }
    if let Some(native) = events.iter().rev().find(|e| e.kind == "native_goal") {
        unverified(
            Some(native.at),
            "native_goal",
            format!(
                "the native goal reported {}; an observation, not evidence",
                native.detail["outcome"].as_str().unwrap_or("an outcome")
            ),
        );
    }
    // Blocking findings stand until their reviewer submits a newer verdict.
    let blocking: Vec<Value> = latest_by_reviewer
        .values()
        .flat_map(|event| {
            let d = &event.detail;
            d["findings"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|f| f["severity"] == "blocking")
                .map(|f| {
                    json!({
                        "review": d["review"], "reviewer": d["reviewer"], "at": event.at,
                        "where": f["where"], "issue": f["issue"], "evidence": f["evidence"],
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let tasks: Vec<Value> = goal
        .tasks
        .iter()
        .map(|t| json!({"id": t.spec.id, "done": t.succeeded()}))
        .collect();
    let mut packet = json!({
        "goal": goal.id,
        "objective": goal.spec.objective,
        "status": goal.status,
        "work_dir": goal.work_dir().ok(),
        "acceptance": goal.spec.acceptance.iter().enumerate()
            .map(|(i, text)| json!({"id": format!("AC{}", i + 1), "text": text}))
            .collect::<Vec<_>>(),
        "tasks": tasks,
        "checks": checks,
        "counts": {
            "assignments": count("assigned"),
            "corrections": count("correction"),
            "revisions": goal.guidance.iter().filter(|g| g.note.starts_with(REVISION)).count(),
        },
        "attention": crate::accounting::compute(goal, &events, Utc::now()),
        "decisions": decisions,
        "reports": reports,
        "follow_ups": follow_ups,
        "outside_write_paths": outside_scope,
        "blocking_findings": blocking,
        "reviews": reviews,
        "not_verified": not_verified,
        "accepted": goal.acceptance.as_ref().map(|a| &a.note),
    });
    if let Some(checkout) = &goal.checkout {
        packet["checkout"] = json!({"path": checkout.path, "branch": checkout.branch,
                                    "removed_at": checkout.removed_at});
    }
    Ok(packet)
}

/// One titled part of the text packet; an empty part says so.
fn section(title: &str, lines: Vec<String>) -> String {
    let mut text = format!("{title}:\n");
    if lines.is_empty() {
        text += "  (none)\n";
    }
    for line in lines {
        text += &format!("  {line}\n");
    }
    text
}

/// The packet as text: one section per part, empty parts said plainly.
pub(crate) fn render(packet: &Value) -> String {
    let mut out = String::new();
    let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
    let at = |value: &Value| {
        serde_json::from_value::<DateTime<Utc>>(value.clone())
            .map(when)
            .unwrap_or_default()
    };
    let task = |value: &Value| {
        value
            .as_str()
            .map(|task| format!(" [{task}]"))
            .unwrap_or_default()
    };
    let list = |key: &str| packet[key].as_array().cloned().unwrap_or_default();
    out += &format!(
        "Review packet for {}  {}\n{}\n",
        text(&packet["goal"]),
        text(&packet["status"]),
        text(&packet["objective"])
    );
    if let Some(dir) = packet["work_dir"].as_str() {
        out += &format!("Work dir: {dir}\n");
    }
    if let Some(checkout) = packet.get("checkout") {
        out += &format!(
            "Checkout: {} (branch {})\n",
            text(&checkout["path"]),
            text(&checkout["branch"])
        );
    }
    out += &section(
        "Acceptance",
        list("acceptance")
            .iter()
            .map(|a| format!("{}: {}", text(&a["id"]), text(&a["text"])))
            .collect(),
    );
    out += &section(
        "Tasks",
        list("tasks")
            .iter()
            .map(|t| {
                format!(
                    "{}: {}",
                    text(&t["id"]),
                    if t["done"] == true { "done" } else { "open" }
                )
            })
            .collect(),
    );
    out += &section(
        "Checks (latest result)",
        list("checks")
            .iter()
            .map(|c| {
                let mut line = format!("{}: {}", text(&c["name"]), text(&c["result"]));
                if c["at"].is_string() {
                    line += &format!(" at {} ({})", at(&c["at"]), text(&c["source"]));
                }
                match c["state"].as_str() {
                    Some("stale") => line += ", on an earlier state of the work dir",
                    Some("unknown") => line += ", on a state not recorded",
                    _ => {}
                }
                // The fingerprint decides; the time only adds to a doubtful pass.
                if c["before_last_assignment"] == true && c["state"] != "current" {
                    line += ", before the last assignment";
                }
                if let Some(detail) = c["detail"].as_str().filter(|_| c["result"] != "passed") {
                    let first = detail.lines().next().unwrap_or_default();
                    line += &format!(" — {first}");
                }
                line
            })
            .collect(),
    );
    let counts = &packet["counts"];
    out += &format!(
        "Counts: {} assignment(s), {} correction(s), {} revision(s)\n",
        counts["assignments"], counts["corrections"], counts["revisions"]
    );
    if packet["attention"].is_object() {
        out += &format!(
            "Attention (from the timeline, an observation): {}\n",
            crate::accounting::line(&packet["attention"])
        );
    }
    out += &section(
        "Decisions",
        list("decisions")
            .iter()
            .map(|d| {
                format!(
                    "{} {}{}: {} — why: {}",
                    at(&d["at"]),
                    text(&d["by"]),
                    task(&d["task"]),
                    text(&d["what"]),
                    text(&d["why"])
                )
            })
            .collect(),
    );
    // Reports in full: the manager reads them here, not in a transcript.
    out += &section(
        "Reports",
        list("reports")
            .iter()
            .flat_map(|r| {
                let head = format!("{} {}{}:", at(&r["at"]), text(&r["from"]), task(&r["task"]));
                std::iter::once(head).chain(
                    text(&r["text"])
                        .lines()
                        .map(|l| format!("  {l}"))
                        .collect::<Vec<_>>(),
                )
            })
            .collect(),
    );
    out += &section(
        "Follow-ups (do not block)",
        list("follow_ups")
            .iter()
            .map(|f| {
                let place = f["where"]
                    .as_str()
                    .filter(|w| !w.is_empty())
                    .map(|w| format!(" ({w})"))
                    .unwrap_or_default();
                let review = f["review"]
                    .as_str()
                    .map(|r| format!(" from review {r}"))
                    .unwrap_or_default();
                format!(
                    "{} {}{}{review}: {}{place}",
                    at(&f["at"]),
                    text(&f["by"]),
                    task(&f["task"]),
                    text(&f["text"])
                )
            })
            .collect(),
    );
    out += &section(
        "Writes outside write_paths (recorded, not blocked)",
        list("outside_write_paths")
            .iter()
            .map(|w| {
                let names = |key: &str| {
                    w[key]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                format!(
                    "{} {} [{}] outside {}: {}",
                    at(&w["at"]),
                    text(&w["by"]),
                    text(&w["task"]),
                    names("write_paths"),
                    names("paths")
                )
            })
            .collect(),
    );
    out += &section(
        "Blocking findings",
        list("blocking_findings")
            .iter()
            .map(|f| {
                format!(
                    "{} (review {}): {} — {}",
                    text(&f["where"]),
                    text(&f["review"]),
                    text(&f["issue"]),
                    text(&f["evidence"])
                )
            })
            .collect(),
    );
    out += &section("Reviews", list("reviews").iter().map(review_line).collect());
    out += &section(
        "Not verified",
        list("not_verified")
            .iter()
            .map(|n| text(&n["what"]))
            .collect(),
    );
    out += &match packet["accepted"].as_str() {
        Some(evidence) => format!("Accepted: {evidence}"),
        None => "Not accepted. This packet is a summary for review: it completes no task and accepts nothing; the user's hgm accept does.".into(),
    };
    out
}

/// One review: who judged, the verdict, the findings and whether it still
/// describes the work.
pub(crate) fn review_line(review: &Value) -> String {
    let text = |key: &str| review[key].as_str().unwrap_or_default().to_string();
    let fresh = match review["fresh"].as_str() {
        Some("current") => "current",
        Some("stale") if review["current_at_submit"] == false => {
            "stale: the work dir changed during the review"
        }
        Some("stale") => "stale: the work dir changed since",
        _ => "unknown: the work dir cannot be read",
    };
    format!(
        "{} {} ({}): {} with {} blocking, {} advisory — {fresh}",
        text("review"),
        text("reviewer"),
        text("agent"),
        text("verdict"),
        review["blocking"],
        review["advisory"],
    )
}

/// The packet for `id`, printed as text or JSON.
pub(crate) fn show(store: &Store, id: &str, json_output: bool) -> Result<()> {
    let goal = store.load(id)?;
    let packet = build(store, &goal).context("build the review packet")?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&packet)?);
    } else {
        println!("{}", render(&packet));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Attempt, CheckResult, Evidence, Verification};
    use chrono::Duration;

    fn store() -> Result<(tempfile::TempDir, tempfile::TempDir, Store)> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let store = Store::new(workspace.path(), state.path())?;
        Ok((workspace, state, store))
    }
    fn result(name: &str, status: AttemptStatus, finished: DateTime<Utc>) -> CheckResult {
        let mut attempt = Attempt::running();
        attempt.status = status;
        attempt.finished_at = Some(finished);
        CheckResult {
            name: name.into(),
            attempt,
            detail: format!("{name} output"),
        }
    }

    #[test]
    fn the_packet_takes_the_newest_check_result_and_lists_what_is_not_verified() -> Result<()> {
        let (workspace, _s, store) = store()?;
        let id = store
            .add(
                serde_norway::from_str(
                    "objective: o\nacceptance: [first, second]\nchecks: [{name: fmt, command: 'true'}, {name: test, command: 'true'}, {name: build, command: 'true'}, {name: doc, command: 'true'}, {name: lint, command: 'true'}]\ntasks: [{id: work, instruction: w}]\n",
                )?,
                None,
            )?
            .id;
        let now = crate::fingerprint::digest(workspace.path())?;
        let early = Utc::now() - Duration::minutes(30);
        store.update(&id, |goal| {
            // Kept before fingerprints: whether lint still holds is unknown.
            goal.verifications.push(Verification {
                started_at: early,
                finished_at: Some(early),
                results: vec![result("lint", AttemptStatus::Succeeded, early)],
                fingerprint: None,
            });
            goal.verifications.push(Verification {
                started_at: early,
                finished_at: Some(early),
                results: vec![
                    result("fmt", AttemptStatus::Succeeded, early),
                    result("test", AttemptStatus::Succeeded, early),
                    result("doc", AttemptStatus::Succeeded, early),
                ],
                fingerprint: Some("an earlier state".into()),
            });
            goal.guidance.push(Evidence {
                note: format!("{REVISION}tighten the parser"),
                recorded_at: early,
            });
            Ok(())
        })?;
        store.record(&id, "assigned", json!({"task": "work", "to": "main"}))?;
        // The watch's settle check is newer than the run: its result wins, and
        // it ran on the state the work dir still has.
        store.record(
            &id,
            "settle_checks",
            json!({"role": "main", "passed": ["fmt"], "failed": [{"name": "test", "detail": "1 failed"}], "fingerprint": now}),
        )?;
        store.record(
            &id,
            "correction",
            json!({"task": "work", "delivery": "uncertain"}),
        )?;
        store.record(&id, "boundary_crossed", json!({"paths": [".github"]}))?;
        store.record(&id, "native_goal", json!({"outcome": "met"}))?;
        decide(&store, &id, Some("work"), "Use a table", "Fewer branches")?;
        follow_up(&store, &id, None, "Rename later")?;

        let packet = build(&store, &store.load(&id)?)?;
        let checks = packet["checks"].as_array().unwrap();
        let summary: Vec<(&str, &str, &str)> = checks
            .iter()
            .map(|c| {
                (
                    c["result"].as_str().unwrap(),
                    c["source"].as_str().unwrap_or("-"),
                    c["state"].as_str().unwrap_or("-"),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("passed", "settle", "current"),
                ("failed", "settle", "current"),
                ("not_run", "-", "-"),
                ("passed", "run", "stale"),
                ("passed", "run", "unknown"),
            ]
        );
        // The correction came later, yet the work dir has not changed: still current.
        assert_eq!(checks[0]["before_last_assignment"], true);
        assert_eq!(checks[1]["detail"], "1 failed");
        assert_eq!(packet["counts"]["assignments"], 1);
        assert_eq!(packet["counts"]["corrections"], 1);
        assert_eq!(packet["counts"]["revisions"], 1);
        assert_eq!(packet["decisions"][0]["what"], "Use a table");
        assert_eq!(packet["decisions"][0]["task"], "work");
        assert_eq!(packet["follow_ups"][0]["text"], "Rename later");
        let not_verified: Vec<(&str, &str)> = packet["not_verified"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| (n["kind"].as_str().unwrap(), n["what"].as_str().unwrap()))
            .collect();
        assert_eq!(not_verified.len(), 7, "{not_verified:?}");
        assert!(not_verified[0].1.starts_with("check test failed at "));
        assert_eq!(not_verified[1].1, "check build has no recorded result");
        assert!(
            not_verified[2].1.starts_with("check doc passed at ")
                && not_verified[2]
                    .1
                    .ends_with(" on an earlier state of the work dir"),
            "{not_verified:?}"
        );
        assert!(
            not_verified[3].1.ends_with(
                "whether the work dir changed since is unknown (no fingerprint was kept)"
            )
        );
        let kinds: Vec<&str> = not_verified.iter().map(|n| n.0).collect();
        assert_eq!(kinds[4..], ["delivery", "protected_paths", "native_goal"]);

        // A changed work dir makes the current pass describe an earlier state.
        std::fs::write(workspace.path().join("new.rs"), "x")?;
        let changed = build(&store, &store.load(&id)?)?;
        assert_eq!(changed["checks"][0]["state"], "stale");

        let text = render(&packet);
        for part in [
            "Review packet for ",
            "  AC2: second",
            "  work: open",
            "  fmt: passed at ",
            "  test: failed at ",
            " — 1 failed",
            "  build: not_run",
            "(run), on an earlier state of the work dir, before the last assignment",
            "(run), on a state not recorded, before the last assignment",
            "Counts: 1 assignment(s), 1 correction(s), 1 revision(s)",
            "[work]: Use a table — why: Fewer branches",
            "Follow-ups (do not block):",
            "Blocking findings:\n  (none)",
            "Reviews:\n  (none)",
            "correction may not have arrived (delivery uncertain)",
            "the native goal reported met; an observation, not evidence",
            "it completes no task and accepts nothing",
        ] {
            assert!(text.contains(part), "missing {part:?} in\n{text}");
        }
        Ok(())
    }

    #[test]
    fn records_need_a_live_goal_a_known_task_and_a_short_text() -> Result<()> {
        let (_w, _s, store) = store()?;
        let id = store
            .add(
                serde_norway::from_str("objective: o\nacceptance: [a]\n")?,
                None,
            )?
            .id;
        let error = |result: Result<Value>| format!("{:#}", result.unwrap_err());
        assert!(error(decide(&store, &id, Some("nope"), "x", "y")).contains("unknown task ID"));
        assert!(error(decide(&store, &id, None, "", "y")).contains("1..150 words"));
        let long = "word ".repeat(151);
        assert!(error(follow_up(&store, &id, None, &long)).contains("1..150 words"));
        store.update(&id, |goal| goal.request(crate::model::Control::Cancel))?;
        assert!(error(decide(&store, &id, None, "x", "y")).contains("live goal"));
        assert!(store.events(&id)?.iter().all(|e| e.kind != DECISION));
        Ok(())
    }

    #[test]
    fn reports_appear_in_full_in_the_packet_and_the_text_log() -> Result<()> {
        let (_w, _s, store) = store()?;
        let id = store
            .add(
                serde_norway::from_str(
                    "objective: o\nacceptance: [a]\ntasks: [{id: work, instruction: i}]\n",
                )?,
                None,
            )?
            .id;
        let body = "Changed parser.rs.\nAC1: verified with cargo test.\nUnresolved: none.";
        store.record(
            &id,
            "report",
            json!({"from": "main", "task": "work", "text": body}),
        )?;
        let packet = build(&store, &store.load(&id)?)?;
        assert_eq!(packet["reports"][0]["from"], "main");
        let text = render(&packet);
        assert!(text.contains("Reports:\n"), "{text}");
        assert!(text.contains("main [work]:\n"), "{text}");
        assert!(
            text.contains("    AC1: verified with cargo test.\n"),
            "{text}"
        );
        assert!(text.contains("    Unresolved: none.\n"), "{text}");
        // `hgm log` prints the same lines under the event's summary line.
        let event = store
            .events(&id)?
            .into_iter()
            .find(|e| e.kind == "report")
            .unwrap();
        assert_eq!(crate::events::full_text(&event), Some(body));
        assert!(crate::events::summary(&event).ends_with("main [work]: Changed parser.rs."));
        Ok(())
    }
}
