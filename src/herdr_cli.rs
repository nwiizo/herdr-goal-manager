use anyhow::{Context, Result, ensure};
use std::{ffi::OsString, process::Stdio, time::Duration};

fn binary() -> OsString {
    std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into())
}
static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) fn count_call() {
    CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}
pub(crate) fn calls() -> u64 {
    CALLS.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) async fn query(args: &[String]) -> Result<String> {
    count_call();
    let mut command = tokio::process::Command::new(binary());
    command.args(args).stdin(Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(35), command.output())
        .await
        .context("Herdr response timed out; inspect state before retrying")??;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        output.status.success(),
        "Herdr: {}{}",
        stderr.trim_end(),
        socket_hint(&stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}
fn socket_hint(stderr: &str) -> String {
    if !(stderr.contains("PermissionDenied") || stderr.contains("Operation not permitted")) {
        return String::new();
    }
    let socket = std::env::var("HERDR_SOCKET_PATH").unwrap_or_else(|_| "Herdr's socket".into());
    format!(
        "\nThis process may not open {socket}; an agent sandbox can block Unix sockets. \
         Allow that socket (Claude Code: sandbox.network.allowUnixSockets) or run hgm outside the sandbox."
    )
}
pub(crate) async fn json_query(args: &[String]) -> Result<serde_json::Value> {
    serde_json::from_str(&query(args).await?).context("parse Herdr response")
}
pub(crate) async fn call(args: &[String]) -> Result<()> {
    query(args).await.map(|_| ())
}
pub(crate) async fn present_current_tab(label: &str) -> Result<()> {
    if let Ok(tab) = std::env::var("HERDR_TAB_ID") {
        call(&["tab".into(), "rename".into(), tab.clone(), label.into()]).await?;
        call(&["tab".into(), "focus".into(), tab]).await?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn socket_hint_only_explains_a_refused_socket() {
        assert_eq!(super::socket_hint("Error: no such pane"), "");
        let hint = super::socket_hint(
            "Error: Os { code: 1, kind: PermissionDenied, message: \"Operation not permitted\" }",
        );
        assert!(hint.contains("sandbox.network.allowUnixSockets"), "{hint}");
    }
}
