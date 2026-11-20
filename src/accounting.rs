use crate::{events::Event, model::Goal};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

pub(crate) fn manager_action(event: &Event) -> bool {
    match event.kind.as_str() {
        "assigned" => event.detail["via"].as_str() != Some("watch"),
        "correction" | "reanchor" | "task_completed" | "waiting" => true,
        _ => false,
    }
}
fn work_sent(event: &Event) -> bool {
    matches!(event.kind.as_str(), "assigned" | "correction" | "reanchor")
}
fn settled(event: &Event) -> bool {
    event.kind == "settle_checks"
        || (event.kind == "wake"
            && event.detail["reason"].as_str().is_some_and(|reason| {
                reason.contains("settled") || reason.starts_with("target state changed to")
            }))
}
fn stops_idle(event: &Event) -> bool {
    event.kind == "waiting"
        || event.kind == "accepted"
        || (event.kind == "status"
            && matches!(
                event.detail["to"].as_str(),
                Some("ready_for_review" | "completed" | "cancelled" | "paused")
            ))
}

pub(crate) fn compute(goal: &Goal, events: &[Event], now: DateTime<Utc>) -> Value {
    let wake_at: Vec<usize> = (0..events.len())
        .filter(|&i| events[i].kind == "wake")
        .collect();
    let acted = wake_at
        .iter()
        .enumerate()
        .filter(|&(n, &i)| {
            let end = wake_at.get(n + 1).copied().unwrap_or(events.len());
            events[i + 1..end].iter().any(manager_action)
        })
        .count();
    let mut stretches: Vec<i64> = vec![];
    let mut since: Option<DateTime<Utc>> = None;
    for event in events {
        if work_sent(event) || stops_idle(event) {
            if let Some(start) = since.take() {
                stretches.push((event.at - start).num_seconds().max(0));
            }
        } else if settled(event) && since.is_none() {
            since = Some(event.at);
        }
    }
    if let Some(start) = since
        && crate::watch::short_of_completion(goal)
    {
        stretches.push((now - start).num_seconds().max(0));
    }
    let notices = events
        .iter()
        .filter(|e| crate::events::notifies_person(e))
        .count();
    let ready = events
        .iter()
        .rposition(|e| e.kind == "status" && e.detail["to"].as_str() == Some("ready_for_review"));
    let awaiting_review_secs = ready.map(|i| {
        let start = events[i].at;
        let end = events[i + 1..]
            .iter()
            .find(|e| {
                e.kind == "accepted"
                    || (e.kind == "status" && e.detail["from"] == "ready_for_review")
            })
            .map_or(now, |e| e.at);
        (end - start).num_seconds().max(0)
    });
    let count = |kind: &str| events.iter().filter(|e| e.kind == kind).count();
    json!({
        "wakes": wake_at.len(),
        "wakes_acted_on": acted,
        "idle_total_secs": stretches.iter().sum::<i64>(),
        "idle_longest_secs": stretches.iter().copied().max().unwrap_or(0),
        "notices": notices,
        "awaiting_review_secs": awaiting_review_secs,
        "corrections": count("correction"),
        "refused": count("refused"),
    })
}

pub(crate) fn line(accounting: &Value) -> String {
    let n = |key: &str| accounting[key].as_i64().unwrap_or(0);
    let duration = crate::watch_view::duration;
    let mut text = format!(
        "{} wakes ({} followed by an action) · idle {}, longest {} · {} notices to the person",
        n("wakes"),
        n("wakes_acted_on"),
        duration(n("idle_total_secs")),
        duration(n("idle_longest_secs")),
        n("notices"),
    );
    if let Some(secs) = accounting["awaiting_review_secs"].as_i64() {
        text += &format!(" · awaiting review {}", duration(secs));
    }
    text += &format!(
        " · {} correction(s) · {} refused command(s)",
        n("corrections"),
        n("refused")
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_794_700_000, 0).unwrap() + Duration::minutes(minute)
    }
    fn event(minute: i64, kind: &str, detail: Value) -> Event {
        let mut event = Event::new("g-1", kind, detail);
        event.at = at(minute);
        event
    }

    #[test]
    fn accounting_counts_wakes_acted_on_idle_stretches_notices_review_and_refusals() {
        let goal = Goal::new(
            serde_norway::from_str("objective: o\nacceptance: [a]\n").unwrap(),
            std::env::temp_dir(),
        )
        .unwrap();
        let events = vec![
            event(0, "assigned", json!({"task": "work"})),
            event(10, "settle_checks", json!({"passed": [], "failed": []})),
            event(
                10,
                "wake",
                json!({"reason": "main settled and every command check passes"}),
            ),
            event(14, "assigned", json!({"task": "work"})),
            event(20, "wake", json!({"reason": "3-minute check of main"})),
            event(
                25,
                "wake",
                json!({"reason": "target state changed to \"done\""}),
            ),
            event(
                26,
                "refused",
                json!({"command": "send", "reason": "budget"}),
            ),
            event(31, "correction", json!({})),
            event(
                40,
                "attention",
                json!({"cause": "idle_no_next_step", "reason": "idle"}),
            ),
            event(45, "assigned", json!({"task": "work", "via": "watch"})),
            event(
                50,
                "status",
                json!({"from": "running", "to": "ready_for_review"}),
            ),
        ];
        let accounting = compute(&goal, &events, at(80));
        assert_eq!(accounting["wakes"], 3);
        assert_eq!(accounting["wakes_acted_on"], 2);
        assert_eq!(accounting["idle_total_secs"], 600);
        assert_eq!(accounting["idle_longest_secs"], 360);
        assert_eq!(accounting["notices"], 2);
        assert_eq!(accounting["awaiting_review_secs"], 30 * 60);
        assert_eq!(accounting["corrections"], 1);
        assert_eq!(accounting["refused"], 1);
        let text = line(&accounting);
        assert!(
            text.starts_with("3 wakes (2 followed by an action) · idle 10m"),
            "{text}"
        );
        assert!(text.contains("awaiting review 30m"), "{text}");
    }
}
