//! Herdr's event stream for one watched pane, read from its Unix socket
//! (protocol 22: one JSON object per line).
//!
//! Observed with Herdr 0.9.5: a connection accepts one `events.subscribe`;
//! `pane.output_matched` fires once per subscription and also for text already
//! on screen; process exit arrives as `pane_exited`, while closing a tab sends
//! no event, so the watch's reconcile read notices a closed pane.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::UnixStream,
};

/// What Herdr reported about the watched pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Signal {
    /// The pane's agent changed state (`idle`, `working`, `blocked`, `done`, `unknown`).
    Status(String),
    /// Output matched the attention pattern; the matched line.
    Output(String),
    /// The pane's process exited or the pane closed.
    Gone,
}

/// One subscription on its own connection.
pub(crate) struct Subscription {
    lines: Lines<BufReader<UnixStream>>,
    pane: String,
}

/// Agent state changes, exit and close of `pane`.
pub(crate) fn state_subscriptions(pane: &str) -> Value {
    json!([
        {"type": "pane.agent_status_changed", "pane_id": pane},
        {"type": "pane.exited"},
        {"type": "pane.closed"},
    ])
}
/// The first visible line of `pane` that matches `pattern` (a regex).
pub(crate) fn output_subscription(pane: &str, pattern: &str) -> Value {
    json!([{
        "type": "pane.output_matched",
        "pane_id": pane,
        "source": "visible",
        "match": {"type": "regex", "value": pattern},
    }])
}

/// Connects to `socket`, subscribes, and waits for Herdr to confirm.
pub(crate) async fn subscribe(
    socket: &Path,
    pane: &str,
    subscriptions: Value,
) -> Result<Subscription> {
    crate::herdr_cli::count_call();
    let limit = Duration::from_secs(5);
    let mut stream = tokio::time::timeout(limit, UnixStream::connect(socket))
        .await
        .context("Herdr socket did not answer")?
        .with_context(|| format!("connect to Herdr at {}", socket.display()))?;
    let request = json!({
        "id": "hgm-watch",
        "method": "events.subscribe",
        "params": {"subscriptions": subscriptions},
    });
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .context("send the subscription")?;
    let mut lines = BufReader::new(stream).lines();
    let reply = tokio::time::timeout(limit, lines.next_line())
        .await
        .context("Herdr did not confirm the subscription")??
        .context("Herdr closed the connection before confirming")?;
    let reply: Value = serde_json::from_str(&reply).context("parse Herdr's reply")?;
    if reply["result"]["type"] != "subscription_started" {
        bail!("Herdr refused the subscription: {}", reply["error"]);
    }
    Ok(Subscription {
        lines,
        pane: pane.into(),
    })
}

impl Subscription {
    /// The next signal for the watched pane; `None` when the stream ended.
    /// Events for other panes and unknown kinds are skipped.
    pub(crate) async fn next(&mut self) -> Result<Option<Signal>> {
        while let Some(line) = self.lines.next_line().await? {
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(signal) = signal_for(&event, &self.pane) {
                return Ok(Some(signal));
            }
        }
        Ok(None)
    }
}

/// Herdr names stream events with dots (`pane.agent_status_changed`) and
/// lifecycle events with underscores (`pane_exited`); both are accepted.
fn signal_for(event: &Value, pane: &str) -> Option<Signal> {
    let data = &event["data"];
    if data["pane_id"].as_str() != Some(pane) {
        return None;
    }
    match event["event"].as_str()?.replace('_', ".").as_str() {
        "pane.agent.status.changed" => Some(Signal::Status(
            data["agent_status"].as_str().unwrap_or("unknown").into(),
        )),
        "pane.output.matched" => Some(Signal::Output(
            data["matched_line"].as_str().unwrap_or_default().into(),
        )),
        "pane.exited" | "pane.closed" => Some(Signal::Gone),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_for_the_watched_pane_become_signals() {
        let status = json!({"event":"pane.agent_status_changed","data":{"pane_id":"w1:p1","agent_status":"done"}});
        assert_eq!(
            signal_for(&status, "w1:p1"),
            Some(Signal::Status("done".into()))
        );
        assert_eq!(signal_for(&status, "w1:p2"), None);
        let exited = json!({"event":"pane_exited","data":{"pane_id":"w1:p1","type":"pane_exited"}});
        assert_eq!(signal_for(&exited, "w1:p1"), Some(Signal::Gone));
        let matched = json!({"event":"pane.output_matched","data":{"pane_id":"w1:p1","matched_line":"Do you want to proceed?"}});
        assert_eq!(
            signal_for(&matched, "w1:p1"),
            Some(Signal::Output("Do you want to proceed?".into()))
        );
        let other = json!({"event":"pane.scroll_changed","data":{"pane_id":"w1:p1"}});
        assert_eq!(signal_for(&other, "w1:p1"), None);
    }
}
