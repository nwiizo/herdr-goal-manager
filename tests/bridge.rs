//! Native-command boundary tests: isolated homes and controlled provider/Herdr binaries.
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
};
struct Bridge {
    root: tempfile::TempDir,
    workspace: PathBuf,
    state: PathBuf,
    context: String,
}
impl Bridge {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("work 'quoted'");
        let state = root.path().join("state");
        let bin = root.path().join("bin");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&bin).unwrap();
        for (name, source) in [
            ("herdr", include_str!("fixtures/herdr")),
            (
                "codex",
                "#!/bin/sh\nprintf '%s' \"$HGM_CONTEXT\" > \"$FIXTURE_ROOT/context\"\nprintf '%s\\n' \"$@\" > \"$FIXTURE_ROOT/arguments\"\nhgm --json status > \"$FIXTURE_ROOT/status\" 2>&1 || true\nprintf '%s' \"$COPILOT_CUSTOM_INSTRUCTIONS_DIRS\" > \"$FIXTURE_ROOT/instruction-dirs\"\n",
            ),
            (
                "claude",
                "#!/bin/sh\nprintf '%s' \"$HGM_CONTEXT\" > \"$FIXTURE_ROOT/context\"\nprintf '%s\\n' \"$@\" > \"$FIXTURE_ROOT/arguments\"\n",
            ),
        ] {
            let file = bin.join(name);
            std::fs::write(&file, source).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::copy(bin.join("codex"), bin.join("copilot")).unwrap();
        let mut value = Self {
            root,
            workspace,
            state,
            context: String::new(),
        };
        value.write("target.screen", "Finished output\n› ");
        value.write("manager.screen", "› ");
        value.inventory("idle", "idle", "original");
        let output = value.call(&["manager", "--agent", "codex"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        value.context = std::fs::read_to_string(value.root.path().join("context")).unwrap();
        value
    }
    /// The fake Herdr socket path; nothing listens unless a test binds it.
    fn socket(&self) -> PathBuf {
        self.root.path().join("herdr.sock")
    }
    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.root.path().join(name), text).unwrap();
    }
    fn inventory(&self, worker: &str, manager: &str, terminal: &str) {
        write_inventory(self.root.path(), worker, manager, terminal);
    }
    fn command(&self, args: &[&str]) -> Command {
        self.command_with(env!("CARGO_BIN_EXE_hgm").as_ref(), args)
    }
    fn command_with(&self, program: &std::path::Path, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command.env_clear().env("HOME",self.root.path())
            .env("PATH",format!("{}:/usr/bin:/bin",self.root.path().join("bin").display()))
            .env("FIXTURE_ROOT",self.root.path())
            .env("HERDR_ENV","1").env("HERDR_SOCKET_PATH",self.socket()).env("HGM_KEEP_AWAKE","0")
            .env("HERDR_PANE_ID","w1:p2").env("HERDR_TAB_ID","w1:t2").env("HERDR_WORKSPACE_ID","w1")
            .env("HERDR_BIN_PATH",self.root.path().join("bin/herdr"))
            .env("HGM_CONTEXT",&self.context)
            .env("HGM_ORIGIN_JSON",json!({"pane":"w1:p1","tab":"w1:t1","terminal":"original","agent":"codex","session":{"value":"worker"}}).to_string())
            .current_dir(&self.workspace).arg("--workspace").arg(&self.workspace)
            .arg("--state-dir").arg(&self.state).arg("--json").args(args);
        command
    }
    fn call(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let output = self.call(args);
        assert!(
            output.status.success(),
            "{}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn goal(&self) -> String {
        self.ok(&[
            "add",
            "Useful result",
            "--criterion",
            "Recipient can proceed",
            "--check",
            "true",
        ])["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn hook(&self, input: Value) -> Value {
        let mut child = self
            .command(&["hook-stop"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
#[test]
fn dispatch_keeps_original_pane_and_rejects_drafts_and_replacements() {
    let bridge = Bridge::new();
    bridge.ok(&["send", "Inspect report.txt and test the stated outcome"]);
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(sent.starts_with("w1:p1\n"));
    // A refusal names the one cause observed and what to do about it.
    let refused = |bridge: &Bridge| {
        let output = bridge.call(&["send", "do not append this"]);
        assert!(!output.status.success());
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    bridge.write("target.screen", "› user's unfinished draft");
    let draft = refused(&bridge);
    assert!(
        draft.contains("draft text in its input: \"user's unfinished draft\"")
            && draft.contains("ask the user")
            && !draft.contains("working"),
        "{draft}"
    );
    bridge.write("target.screen", "Editing src/lib.rs\n› ");
    bridge.inventory("working", "idle", "original");
    let working = refused(&bridge);
    assert!(
        working.contains("is working: its turn has not ended")
            && working.contains("send --interrupt")
            && !working.contains("draft"),
        "{working}"
    );
    bridge.inventory("idle", "idle", "original");
    bridge.write("target.screen", "› ");
    bridge.inventory("idle", "idle", "replacement");
    assert!(!bridge.call(&["send", "do not retarget"]).status.success());
    assert_eq!(
        std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap(),
        sent
    );
}
#[test]
fn watcher_wakes_manager_but_does_not_complete_work_and_pause_stops_it() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(sent.starts_with("w1:p2\n"));
    assert!(sent.contains("idle/done is not acceptance"));
    assert_eq!(bridge.ok(&["show", &goal])["status"], "ready");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert_eq!(
        std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap(),
        sent
    );
    bridge.ok(&[
        "wait",
        &goal,
        "--reason",
        "Which audience should receive this?",
    ]);
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert_eq!(bridge.ok(&["watch"])["active"], false);
}
#[test]
fn stop_hook_continues_once_only_for_pending_observation_and_respects_human_wait() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.inventory("done", "working", "original");
    bridge.ok(&["supervise", &goal]);
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    // A watch process is running (it holds the tab's watch lease).
    let _running = hold_watch_lease(&bridge);
    let wakes = |bridge: &Bridge| -> Vec<Value> {
        let log = bridge.ok(&["log", &goal]);
        log.as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "wake")
            .cloned()
            .collect()
    };
    assert!(wakes(&bridge).is_empty());
    assert_eq!(bridge.hook(json!({"stop_hook_active":true})), json!({}));
    let response = bridge.hook(json!({"stop_hook_active":false}));
    assert!(
        response["reason"]
            .as_str()
            .unwrap()
            .contains("A new observation is pending"),
        "{response}"
    );
    assert_eq!(response["decision"], "block");
    assert_eq!(bridge.hook(json!({"stop_hook_active":false})), json!({}));
    // A wake delivered through the hook is on the timeline once, like a typed one.
    let recorded = wakes(&bridge);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    let detail = &recorded[0]["detail"];
    assert_eq!(detail["via"], "hook");
    assert_eq!(detail["role"], "main");
    assert!(!detail["reason"].as_str().unwrap().is_empty(), "{detail}");
    assert!(
        detail["observed_at"].is_i64() && detail["mode"].is_string(),
        "{detail}"
    );
    bridge.ok(&["wait", &goal, "--reason", "Need a real user decision"]);
    assert_eq!(bridge.hook(json!({"stop_hook_active":false})), json!({}));
}
#[test]
fn uncertain_wake_is_not_automatically_replayed_after_restart() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    bridge.write("fail-send", "");
    assert!(!bridge.call(&["watch-loop", "--once"]).status.success());
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert_eq!(
        std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap(),
        sent
    );
    assert_eq!(bridge.ok(&["watch"])["active"], false);
}
#[test]
fn claude_hook_settings_are_session_scoped_and_no_hook_runs_without_supervision() {
    let bridge = Bridge::new();
    let output = bridge.call(&["manager", "--agent", "claude"]);
    assert!(output.status.success());
    let arguments = std::fs::read_to_string(bridge.root.path().join("arguments")).unwrap();
    let lines: Vec<_> = arguments.lines().collect();
    let index = lines.iter().position(|arg| *arg == "--settings").unwrap();
    let settings: Value =
        serde_json::from_slice(&std::fs::read(lines[index + 1]).unwrap()).unwrap();
    assert!(
        settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("hook-stop")
    );
    assert_eq!(settings["idleCompaction"], false);
    assert_eq!(
        settings["sandbox"],
        json!({"network": {"allowUnixSockets": [bridge.socket()]}})
    );
    let session = &settings["hooks"]["SessionStart"][0];
    assert_eq!(session["matcher"], "compact|resume|clear");
    assert!(
        session["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("hook-session")
    );
    let restored = bridge.call(&["hook-session"]);
    let restored = String::from_utf8_lossy(&restored.stdout);
    assert!(
        restored.contains("state restored") && restored.contains("# Needs you"),
        "{restored}"
    );
    assert!(!bridge.root.path().join(".claude/settings.json").exists());
    assert_eq!(bridge.hook(json!({})), json!({}));
}

#[test]
fn copilot_is_a_native_manager_and_dispatch_target() {
    let bridge = Bridge::new();
    let output = bridge.call(&["manager", "--agent", "copilot"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let arguments = std::fs::read_to_string(bridge.root.path().join("arguments")).unwrap();
    assert!(arguments.lines().any(|s| s == "--interactive"));
}

#[test]
fn managers_run_plain_hgm_with_full_access_on_every_provider() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    let codex = std::fs::read_to_string(bridge.root.path().join("arguments")).unwrap();
    assert!(
        codex
            .lines()
            .any(|s| s == "--dangerously-bypass-approvals-and-sandbox")
    );
    assert!(
        !codex
            .lines()
            .any(|s| s == "--sandbox" || s == "--ask-for-approval")
    );
    let output = bridge.call(&["manager", "--agent", "claude"]);
    assert!(output.status.success());
    let claude = std::fs::read_to_string(bridge.root.path().join("arguments")).unwrap();
    assert!(
        claude
            .lines()
            .any(|s| s == "--dangerously-skip-permissions")
    );
    let output = bridge.call(&["manager", "--agent", "copilot"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status = std::fs::read_to_string(bridge.root.path().join("status")).unwrap();
    assert!(
        status.contains(&goal),
        "plain hgm missed the goal: {status}"
    );
    let arguments = std::fs::read_to_string(bridge.root.path().join("arguments")).unwrap();
    assert!(arguments.lines().any(|s| s == "--allow-all"));
    assert!(!arguments.contains("--allow-tool="));
}

#[test]
fn manager_guide_is_an_instruction_not_the_first_user_message() {
    const GUIDE: &str = "You are the user's Goal Manager";
    const OPENER: &str = "Continue our goal conversation. Run hgm overview once, then ask only what is needed to move forward.";
    let bridge = Bridge::new();
    let codex = std::fs::read_to_string(bridge.root.path().join("arguments")).unwrap();
    assert!(
        codex
            .lines()
            .any(|arg| arg.starts_with("developer_instructions=") && arg.contains(GUIDE)),
        "{codex}"
    );
    assert_eq!(codex.lines().last(), Some(OPENER));

    let output = bridge.call(&["manager", "--agent", "copilot"]);
    assert!(output.status.success());
    let copilot = std::fs::read_to_string(bridge.root.path().join("arguments")).unwrap();
    assert!(!copilot.contains(GUIDE), "{copilot}");
    let lines: Vec<_> = copilot.lines().collect();
    let index = lines
        .iter()
        .position(|arg| *arg == "--interactive")
        .unwrap();
    assert_eq!(lines[index + 1], OPENER);
    let directory = std::fs::read_to_string(bridge.root.path().join("instruction-dirs")).unwrap();
    let instructions = std::fs::read_to_string(
        PathBuf::from(directory).join(".github/instructions/hgm-manager.instructions.md"),
    )
    .unwrap();
    assert!(instructions.starts_with("---\napplyTo: \"**\"\n---\n"));
    assert!(instructions.contains(GUIDE));
}

#[test]
fn briefs_are_kept_per_original_terminal() {
    let bridge = Bridge::new();
    let first = bridge.root.path().join("first.md");
    std::fs::write(&first, "Original tab: finish the report").unwrap();
    bridge.ok(&["brief", "--save", first.to_str().unwrap()]);

    let output = bridge
        .command(&["manager", "--agent", "codex"])
        .env(
            "HGM_ORIGIN_JSON",
            json!({"pane":"w1:p3","tab":"w1:t3","terminal":"other","agent":"codex","session":null})
                .to_string(),
        )
        .output()
        .unwrap();
    assert!(output.status.success());
    let other = std::fs::read_to_string(bridge.root.path().join("context")).unwrap();
    let brief_for = |context: &str, args: &[&str]| -> Value {
        let output = bridge
            .command(args)
            .env("HGM_CONTEXT", context)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    assert_eq!(brief_for(&other, &["brief"])["brief"], "");
    let second = bridge.root.path().join("second.md");
    std::fs::write(&second, "Other tab: review the draft").unwrap();
    brief_for(&other, &["brief", "--save", second.to_str().unwrap()]);
    assert_eq!(
        bridge.ok(&["brief"])["brief"],
        "Original tab: finish the report"
    );
    assert_eq!(
        brief_for(&other, &["brief"])["brief"],
        "Other tab: review the draft"
    );
}

#[test]
fn restart_requires_saved_handoff_and_matching_successor_receipt() {
    let bridge = Bridge::new();
    bridge.write("process.json", &json!({"result":{"process_info":{"foreground_processes":[{"argv":["codex","--sandbox","workspace-write","--ask-for-approval","on-request"]}]}}}).to_string());
    let transfer = bridge.ok(&["handoff"]);
    assert_eq!(transfer["phase"], "awaiting_handoff");
    assert_eq!(bridge.ok(&["restart"])["phase"], "awaiting_handoff");
    assert!(!bridge.call(&["handoff"]).status.success());
    let document = PathBuf::from(transfer["document"].as_str().unwrap());
    let ready = PathBuf::from(transfer["ready"].as_str().unwrap());
    std::fs::write(&document, "Goal: recipient can proceed. Verified: report exists. Next: check report. No dependent jobs. Authority: no publishing.").unwrap();
    std::fs::write(
        &ready,
        json!({"transfer":transfer["id"],"session":"wrong","safe_to_restart":true}).to_string(),
    )
    .unwrap();
    assert!(!bridge.call(&["restart"]).status.success());
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(!sent.lines().any(|s| s == "/exit"));
    std::fs::write(
        &ready,
        json!({"transfer":transfer["id"],"session":"worker","safe_to_restart":true}).to_string(),
    )
    .unwrap();
    bridge.inventory("working", "idle", "original");
    assert_eq!(bridge.ok(&["restart"])["phase"], "awaiting_handoff");
    bridge.inventory("idle", "idle", "original");
    let original: Value =
        serde_json::from_slice(&std::fs::read(bridge.root.path().join("panes.json")).unwrap())
            .unwrap();
    let mut exited = original["result"]["panes"].clone();
    exited[0].as_object_mut().unwrap().remove("agent");
    exited[0].as_object_mut().unwrap().remove("agent_session");
    bridge.write(
        "exited.json",
        &json!({"result":{"panes":exited}}).to_string(),
    );
    bridge.write(
        "exited-agents.json",
        &json!({"result":{"agents":[exited[1]]}}).to_string(),
    );
    let mut successor = original["result"]["panes"].clone();
    successor[0]["agent_session"]["value"] = json!("fresh-worker");
    bridge.write(
        "successor.json",
        &json!({"result":{"panes":successor}}).to_string(),
    );
    bridge.write(
        "successor-agents.json",
        &json!({"result":{"agents":successor}}).to_string(),
    );
    assert_eq!(bridge.ok(&["restart"])["phase"], "awaiting_exit");
    assert_eq!(bridge.ok(&["restart"])["phase"], "awaiting_successor");
    let adoption = bridge.ok(&["restart"]);
    assert_eq!(adoption["phase"], "awaiting_acknowledgement");
    assert_eq!(bridge.ok(&["restart"])["phase"], "awaiting_acknowledgement");
    let started = std::fs::read_to_string(bridge.root.path().join("started.txt")).unwrap();
    assert_eq!(started.lines().filter(|s| *s == "start").count(), 1);
    assert!(started.contains("workspace-write\n--ask-for-approval\non-request"));
    std::fs::write(
        transfer["acknowledged"].as_str().unwrap(),
        json!({"transfer":transfer["id"],"session":"fresh-worker","sha256":adoption["digest"]})
            .to_string(),
    )
    .unwrap();
    assert_eq!(bridge.ok(&["restart"])["phase"], "complete");
    assert_eq!(
        bridge.ok(&["target"])["binding"]["session"]["value"],
        "fresh-worker"
    );
}

#[test]
fn a_successor_without_a_session_yet_is_adopted_and_checked_against_the_session_it_reports() {
    // main is Copilot, whose session Herdr reports only after a first turn.
    let bridge = Bridge::new();
    let context_path = manager_dir(&bridge).join(format!("{}.json", bridge.context));
    let mut context: Value =
        serde_json::from_slice(&std::fs::read(&context_path).unwrap()).unwrap();
    context["origin"]["agent"] = json!("copilot");
    std::fs::write(&context_path, context.to_string()).unwrap();
    let main_as = |session: Option<&str>| -> Value {
        let panes: Value =
            serde_json::from_slice(&std::fs::read(bridge.root.path().join("panes.json")).unwrap())
                .unwrap();
        let mut panes = panes["result"]["panes"].clone();
        panes[0]["agent"] = json!("copilot");
        match session {
            Some(value) => panes[0]["agent_session"] = json!({ "value": value }),
            None => {
                panes[0].as_object_mut().unwrap().remove("agent_session");
            }
        }
        panes
    };
    let show = |panes: &Value| {
        bridge.write("panes.json", &json!({"result":{"panes":panes}}).to_string());
        bridge.write(
            "agents.json",
            &json!({"result":{"agents":panes}}).to_string(),
        );
    };
    let process = |pid: u64| {
        bridge.write("process.json", &json!({"result":{"process_info":{"foreground_processes":[{"argv":["copilot","--allow-all","--model","auto"],"pid":pid}]}}}).to_string());
    };
    let predecessor = main_as(Some("worker"));
    show(&predecessor);
    bridge.write(
        "target.screen",
        "Done\n╻▄▄▄\n┃\n╹▀▀▀\n Interactive · / commands\n",
    );
    process(100);
    let transfer = bridge.ok(&["handoff"]);
    std::fs::write(
        transfer["document"].as_str().unwrap(),
        "Goal: g. Verified: report. Next: check it.",
    )
    .unwrap();
    std::fs::write(
        transfer["ready"].as_str().unwrap(),
        json!({"transfer":transfer["id"],"session":"worker","safe_to_restart":true}).to_string(),
    )
    .unwrap();
    let mut exited = predecessor.clone();
    exited[0].as_object_mut().unwrap().remove("agent");
    exited[0].as_object_mut().unwrap().remove("agent_session");
    bridge.write(
        "exited.json",
        &json!({"result":{"panes":exited}}).to_string(),
    );
    bridge.write(
        "exited-agents.json",
        &json!({"result":{"agents":[exited[1]]}}).to_string(),
    );
    let fresh = main_as(None);
    bridge.write(
        "successor.json",
        &json!({"result":{"panes":fresh}}).to_string(),
    );
    bridge.write(
        "successor-agents.json",
        &json!({"result":{"agents":fresh}}).to_string(),
    );
    assert_eq!(bridge.ok(&["restart"])["phase"], "awaiting_exit");
    assert_eq!(bridge.ok(&["restart"])["phase"], "awaiting_successor");
    let sent = || std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap_or_default();
    // Still the predecessor's process: no adoption request.
    assert_eq!(bridge.ok(&["restart"])["phase"], "awaiting_successor");
    assert!(!sent().contains("fresh-session handoff"));
    // Another process with no session yet gets the request with its own id.
    process(200);
    let adoption = bridge.ok(&["restart"]);
    assert_eq!(adoption["phase"], "awaiting_acknowledgement");
    assert!(sent().contains("$COPILOT_AGENT_SESSION_ID"), "{}", sent());
    let acknowledge = |session: &str| {
        std::fs::write(
            transfer["acknowledged"].as_str().unwrap(),
            json!({"transfer":transfer["id"],"session":session,"sha256":adoption["digest"]})
                .to_string(),
        )
        .unwrap();
    };
    acknowledge("fresh");
    // Herdr has not reported the session yet: wait.
    assert_eq!(bridge.ok(&["restart"])["phase"], "awaiting_acknowledgement");
    // The predecessor's session, or another one than written, stops the transfer.
    show(&main_as(Some("worker")));
    acknowledge("worker");
    assert!(!bridge.call(&["restart"]).status.success());
    show(&main_as(Some("other")));
    acknowledge("fresh");
    assert!(!bridge.call(&["restart"]).status.success());
    show(&main_as(Some("fresh")));
    assert_eq!(bridge.ok(&["restart"])["phase"], "complete");
    assert_eq!(
        bridge.ok(&["target"])["binding"]["session"]["value"],
        "fresh"
    );
}

#[test]
fn linked_send_records_the_assignment_arms_monitoring_and_alerts_once_on_approval() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["confirm", &goal, "--evidence", "User: go ahead"]);
    let sent = bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "Write summary.json",
    ]);
    assert_eq!(sent["supervision"], "armed", "{sent}");
    let opened = std::fs::read_to_string(bridge.root.path().join("opened.txt")).unwrap();
    assert!(opened.contains("monitor"));
    let refused = bridge.call(&["send", "--goal", &goal, "--task", "missing", "x"]);
    assert!(!refused.status.success());

    // Herdr reports `done` while an approval dialog is visible.
    bridge.write(
        "target.screen",
        "Create file\nDo you want to create summary.json?\n❯ 1. Yes\n  2. No",
    );
    bridge.inventory("done", "idle", "original");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let notified =
        || std::fs::read_to_string(bridge.root.path().join("notified.txt")).unwrap_or_default();
    assert_eq!(notified().matches("Original agent needs you").count(), 1);
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert_eq!(notified().matches("Original agent needs you").count(), 1);
    // A one-off observation from the manager's tab keeps that tab's name.
    assert!(!bridge.root.path().join("renamed.txt").exists());

    bridge.ok(&["wait", &goal, "--reason", "Approve the file creation"]);
    assert!(notified().contains("Goal is waiting for you"));
    let kinds: Vec<_> = bridge
        .ok(&["log", &goal])
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"].as_str().unwrap().to_string())
        .collect();
    for kind in ["registered", "assigned", "attention", "waiting"] {
        assert!(kinds.iter().any(|k| k == kind), "{kind} missing: {kinds:?}");
    }
}

#[test]
fn goals_belong_to_their_tab_and_the_manager_uses_the_tab_agent() {
    let bridge = Bridge::new();
    let mine = bridge.goal();
    let output = bridge
        .command(&["manager"])
        .env(
            "HGM_ORIGIN_JSON",
            json!({"pane":"w1:p3","tab":"w1:t3","terminal":"other","agent":"claude","session":null})
                .to_string(),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let arguments = std::fs::read_to_string(bridge.root.path().join("arguments")).unwrap();
    assert!(
        arguments.contains("--append-system-prompt"),
        "claude expected: {arguments}"
    );
    let other = std::fs::read_to_string(bridge.root.path().join("context")).unwrap();
    let status_for = |args: &[&str]| -> Value {
        let output = bridge
            .command(args)
            .env("HGM_CONTEXT", &other)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    assert_eq!(status_for(&["status"]), json!([]));
    assert_eq!(status_for(&["status", "--all"])[0]["id"], mine);
    assert_eq!(bridge.ok(&["status"])[0]["id"], mine);
}

fn kinds(bridge: &Bridge, goal: &str) -> Vec<String> {
    bridge
        .ok(&["log", goal])
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"].as_str().unwrap().to_string())
        .collect()
}
fn add_file(bridge: &Bridge, spec: Value) -> String {
    let file = bridge.root.path().join("goal.json");
    std::fs::write(&file, spec.to_string()).unwrap();
    bridge.ok(&["add", "--file", file.to_str().unwrap()])["id"]
        .as_str()
        .unwrap()
        .into()
}
fn state_directory(bridge: &Bridge) -> PathBuf {
    std::fs::read_dir(bridge.state.join("workspaces"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}

#[test]
fn loop_contract_gates_assignments_and_a_spent_budget_reaches_the_inbox() {
    let bridge = Bridge::new();
    let goal = add_file(
        &bridge,
        json!({
            "objective":"Summarize orders","acceptance":["Totals match orders.csv"],
            "loop":{"max_assignments":1,"autonomy":"ask"}
        }),
    );
    let send = |extra: &[&str]| {
        let mut args = vec!["send", "--goal", &goal, "--task", "work", "--no-supervise"];
        args.extend_from_slice(extra);
        args.push("Write summary.json");
        bridge.call(&args)
    };
    let unconfirmed = send(&[]);
    assert!(String::from_utf8_lossy(&unconfirmed.stderr).contains("hgm confirm"));
    bridge.ok(&[
        "confirm",
        &goal,
        "--evidence",
        "User: yes, summarize the orders",
    ]);
    let unapproved = send(&[]);
    assert!(String::from_utf8_lossy(&unapproved.stderr).contains("--user-approval"));
    assert!(send(&["--user-approval", "User: send it"]).status.success());

    let spent = send(&["--user-approval", "User: try again"]);
    assert!(!spent.status.success());
    assert!(String::from_utf8_lossy(&spent.stderr).contains("budget exhausted"));
    let inbox = bridge.ok(&["inbox"]);
    assert_eq!(inbox[0]["kind"], "answer_manager");
    assert!(
        inbox[0]["reason"]
            .as_str()
            .unwrap()
            .contains("budget exhausted")
    );
    let notified = std::fs::read_to_string(bridge.root.path().join("notified.txt")).unwrap();
    assert!(notified.contains("Goal is waiting for you"));

    bridge.ok(&[
        "complete",
        &goal,
        "work",
        "--evidence",
        "summary.json written",
    ]);
    bridge.ok(&["revise", &goal, "--feedback", "Include tax"]);
    assert!(
        send(&["--user-approval", "User: fix the tax"])
            .status
            .success()
    );
    let kinds = kinds(&bridge, &goal);
    assert_eq!(
        kinds.iter().filter(|k| *k == "assigned").count(),
        2,
        "{kinds:?}"
    );
    assert!(kinds.contains(&"confirmed".to_string()));
}

#[test]
fn watcher_patrols_a_working_agent_and_escalates_a_spent_time_budget() {
    let bridge = Bridge::new();
    let goal = add_file(
        &bridge,
        json!({
            "objective":"Long report","acceptance":["Report covers Q3"],"checks":[{"name":"report","command":"true"}],
            "invariants":["Keep the Q2 numbers"],
            "loop":{"max_hours":1,"patrol_minutes":1,"stop_when":["Q3 data is unavailable"]}
        }),
    );
    bridge.ok(&["confirm", &goal, "--evidence", "User: go"]);
    bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "Draft the Q3 report",
    ]);
    bridge.inventory("working", "idle", "original");
    let watch_path = std::fs::read_dir(state_directory(&bridge).join("manager"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().ends_with("-watch.json"))
        .unwrap();
    let mut watch: Value = serde_json::from_slice(&std::fs::read(&watch_path).unwrap()).unwrap();
    watch["last_patrol"] = json!(0);
    std::fs::write(&watch_path, watch.to_string()).unwrap();
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(sent.contains("5-minute check of main"), "{sent}");
    // The first wake after an assignment carries it in full; the contract
    // (invariants, stop conditions) stays one command away.
    let wake = &sent[sent.rfind("Goal Manager observation").unwrap()..];
    assert!(wake.contains("New assignment (full text):\nDraft the Q3 report"));
    assert!(wake.contains(&format!("Contract: hgm show {goal}")));
    assert!(!wake.contains("Keep the Q2 numbers"), "{wake}");
    assert!(kinds(&bridge, &goal).contains(&"patrol".to_string()));

    // Age the first assignment past the one-hour budget.
    let events = state_directory(&bridge)
        .join("goals")
        .join(format!("{goal}.events.ndjson"));
    let aged = std::fs::read_to_string(&events)
        .unwrap()
        .lines()
        .map(|line| {
            let mut event: Value = serde_json::from_str(line).unwrap();
            if event["kind"] == "assigned" {
                event["at"] = json!("2000-01-01T00:00:00Z");
            }
            event.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&events, aged).unwrap();
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert_eq!(bridge.ok(&["watch"])["active"], false);
    let inbox = bridge.ok(&["inbox"]);
    assert!(
        inbox[0]["reason"].as_str().unwrap().contains("Time budget"),
        "{inbox}"
    );
}

#[test]
fn handoff_threshold_follows_the_goal_loop_policy() {
    let due = |percent: Option<u8>| -> bool {
        let bridge = Bridge::new();
        let mut spec = json!({"objective":"Refactor","acceptance":["Tests pass"],"checks":[{"name":"tests","command":"true"}]});
        if let Some(percent) = percent {
            spec["loop"] = json!({ "handoff_percent": percent });
        }
        let goal = add_file(&bridge, spec);
        bridge.ok(&["confirm", &goal, "--evidence", "User: go"]);
        bridge.ok(&[
            "send",
            "--goal",
            &goal,
            "--task",
            "work",
            "Refactor the module",
        ]);
        // Codex footer: 30% left means 70% used.
        bridge.write(
            "target.screen",
            "Working on it\n› \n  gpt · Context 30% left · ~/work\n",
        );
        let _ = bridge.call(&["watch-loop", "--once"]);
        bridge.ok(&["watch"])["handoff_due"] == true
    };
    assert!(due(Some(60)));
    assert!(!due(None));
}

#[test]
fn manager_guide_carries_the_command_reference_so_no_help_round_trips_are_needed() {
    let bridge = Bridge::new();
    let codex = std::fs::read_to_string(bridge.root.path().join("arguments")).unwrap();
    for expected in [
        "## hgm command reference",
        "do not run --help",
        "`hgm send <TEXT> [--goal <GOAL>] [--task <TASK>] [--no-supervise] [--user-approval <USER_APPROVAL>] [--plan] [--interrupt] [--to <TO>]`",
        "`hgm reanchor <ID> [--to <TO>] [--task <TASK>] [<TEXT>]`",
        "`hgm correct <ID> <TEXT> [--to <TO>] [--task <TASK>] [--user-approval <USER_APPROVAL>]`",
        "`hgm revise <ID> --feedback <FEEDBACK> [--task <TASK>...]`",
        "`hgm overview [--all]`",
    ] {
        assert!(codex.contains(expected), "missing {expected}");
    }
    for hidden in [
        "hgm watch-loop",
        "hgm hook-stop",
        "hgm hook-session",
        "hgm herdr-run",
        "hgm manager",
    ] {
        assert!(!codex.contains(hidden), "unexpected {hidden}");
    }
}

fn confirmed(bridge: &Bridge, spec: Value) -> String {
    let goal = add_file(bridge, spec);
    bridge.ok(&["confirm", &goal, "--evidence", "User: go"]);
    goal
}

#[test]
fn a_linked_send_waits_for_a_watch_step_and_types_nothing_past_the_bound() {
    use std::time::{Duration, Instant};
    let bridge = Bridge::new();
    let goal = confirmed(
        &bridge,
        json!({"objective":"Fix login copy","acceptance":["Copy updated"],
            "checks":[{"name":"copy","command":"true"}]}),
    );
    // Another process (a watch step) holds the supervision lock for `time`.
    let lock = state_directory(&bridge).join("manager/t-original-operation.lock");
    let hold = |time: Duration| {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock)
            .unwrap();
        file.lock().unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(time);
            drop(file);
        })
    };
    let sent = || std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap_or_default();
    let before = sent();
    let send = ["send", "--goal", &goal, "--task", "work", "Update the copy"];

    // Past the bound the send fails before typing, names the lock and records nothing.
    let holder = hold(Duration::from_secs(14));
    let start = Instant::now();
    let output = bridge.call(&send);
    let waited = start.elapsed();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && waited >= Duration::from_secs(9),
        "{waited:?}: {stderr}"
    );
    assert!(
        stderr.contains("t-original-operation.lock")
            && stderr.contains("nothing was delivered or changed"),
        "{stderr}"
    );
    assert_eq!(sent(), before);
    assert!(!kinds(&bridge, &goal).iter().any(|k| k == "assigned"));
    holder.join().unwrap();

    // A short hold delays the send; it then types, records and arms the watch.
    let holder = hold(Duration::from_millis(1500));
    let start = Instant::now();
    let result = bridge.ok(&send);
    assert!(start.elapsed() >= Duration::from_millis(1200), "{result}");
    assert_eq!(result["supervision"], "armed", "{result}");
    assert_ne!(sent(), before);
    assert!(kinds(&bridge, &goal).iter().any(|k| k == "assigned"));
    holder.join().unwrap();
}

#[test]
fn linked_assignments_carry_the_contract_and_stay_one_shot() {
    let bridge = Bridge::new();
    let goal = confirmed(
        &bridge,
        json!({
            "objective":"Export the report","acceptance":["One CSV row per record"],
            "invariants":["API response shape unchanged"],"out_of_scope":["packages/auth"],
            "checks":[{"name":"csv","command":"true"}]
        }),
    );
    bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "--no-supervise",
        "Add the export button",
    ]);
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(
        sent.contains("--- Goal contract (from hgm; binding) ---"),
        "{sent}"
    );
    assert!(sent.contains("- API response shape unchanged"));
    assert!(sent.contains("At a boundary: Stop and report"));
    let long = "word ".repeat(450);
    let refused = bridge.call(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "--no-supervise",
        &long,
    ]);
    assert!(String::from_utf8_lossy(&refused.stderr).contains("one-shot spec"));
}

#[test]
fn plan_first_requires_an_approved_plan_before_implementation() {
    let bridge = Bridge::new();
    let goal = confirmed(
        &bridge,
        json!({
            "objective":"Refactor the auth flow","acceptance":["Tests pass"],
            "checks":[{"name":"tests","command":"true"}],
            "loop":{"plan_first":true,"max_assignments":5}
        }),
    );
    let implement = || {
        bridge.call(&[
            "send",
            "--goal",
            &goal,
            "--task",
            "work",
            "--no-supervise",
            "Implement it",
        ])
    };
    assert!(String::from_utf8_lossy(&implement().stderr).contains("plan first"));
    bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "--no-supervise",
        "--plan",
        "Plan the refactor",
    ]);
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(sent.contains("Write a short plan only"));
    bridge.ok(&[
        "approve-plan",
        &goal,
        "--task",
        "work",
        "--evidence",
        "User reviewed steps 1-3",
    ]);
    assert!(implement().status.success());
}

#[test]
fn watcher_stops_on_protected_paths_and_stop_checks_and_only_wakes_on_context_rot() {
    let checked = |extra: Value| {
        let mut spec = json!({"objective":"Fix login copy","acceptance":["Copy updated"],
            "checks":[{"name":"copy","command":"true"}]});
        spec.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        spec
    };
    // A protected path changes while the agent works.
    let bridge = Bridge::new();
    std::fs::create_dir_all(bridge.workspace.join("auth")).unwrap();
    std::fs::write(bridge.workspace.join("auth/keys.txt"), "v1").unwrap();
    let goal = confirmed(&bridge, checked(json!({"protected_paths":["auth"]})));
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    std::fs::write(bridge.workspace.join("auth/keys.txt"), "v2").unwrap();
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(
        bridge.ok(&["inbox"])[0]["reason"]
            .as_str()
            .unwrap()
            .contains("Protected paths changed")
    );
    assert!(kinds(&bridge, &goal).contains(&"boundary_crossed".to_string()));

    // A machine stop condition holds when the agent settles.
    let bridge = Bridge::new();
    let goal = confirmed(
        &bridge,
        checked(json!({"loop":{"stop_checks":[{"name":"stop-file","command":"test -f STOP"}]}})),
    );
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    std::fs::write(bridge.workspace.join("STOP"), "").unwrap();
    bridge.inventory("done", "idle", "original");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(
        bridge.ok(&["inbox"])[0]["reason"]
            .as_str()
            .unwrap()
            .contains("Stop condition stop-file")
    );

    // Two signs of context rot wake the manager once with the evidence; the
    // manager decides on a handoff (`hgm handoff`), the watch does not.
    let bridge = Bridge::new();
    let goal = confirmed(&bridge, checked(json!({})));
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    let sent_before = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    bridge.write(
        "target.screen",
        "You're absolutely right!\n$ cargo test\n$ cargo test\n$ cargo test\n› ",
    );
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let watch = bridge.ok(&["watch"]);
    assert_eq!(watch["handoff_due"], false, "{watch}");
    assert_eq!(watch["rot_signals"], 0, "{watch}");
    assert_eq!(bridge.ok(&["handoff-status"])["phase"], "not_started");
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    let new = &sent[sent_before.len()..];
    assert_eq!(new.matches("Goal Manager observation").count(), 1, "{new}");
    assert!(
        new.contains("2 signs of context rot in main")
            && new.contains("$ cargo test")
            && new.contains("hgm handoff"),
        "{new}"
    );
    let log = bridge.ok(&["log", &goal]);
    let count = |kind: &str| {
        log.as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == kind)
            .count()
    };
    assert_eq!(count("rot_signal"), 2, "{log}");
    assert_eq!(count("wake"), 1, "{log}");
    // The same signs staying on screen neither wake again nor hand off.
    bridge.write(
        "target.screen",
        "You're absolutely right!\n$ cargo test\n$ cargo test\n$ cargo test\nmore\n› ",
    );
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert_eq!(bridge.ok(&["watch"])["handoff_due"], false);
    assert_eq!(bridge.ok(&["handoff-status"])["phase"], "not_started");
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert_eq!(
        sent[sent_before.len()..]
            .matches("Goal Manager observation")
            .count(),
        1
    );
}

/// Opens Goal Manager from `pane`, whose tab is labelled `label`.
fn open_from(bridge: &Bridge, pane: &str, label: &str) -> Output {
    bridge.write(
        "tab.json",
        &json!({"result":{"tab":{"label":label}}}).to_string(),
    );
    let context =
        json!({"focused_pane_id":pane,"workspace_id":"w1","focused_pane_cwd":bridge.workspace});
    bridge
        .command(&["herdr-open"])
        .env("HERDR_PLUGIN_CONTEXT_JSON", context.to_string())
        .output()
        .unwrap()
}

/// The Goal Manager's pane closed with its agent: only other panes remain.
fn close_manager(bridge: &Bridge) {
    for (name, list) in [("panes.json", "panes"), ("agents.json", "agents")] {
        let path = bridge.root.path().join(name);
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["result"][list]
            .as_array_mut()
            .unwrap()
            .retain(|pane| pane["pane_id"] != "w1:p2");
        std::fs::write(path, value.to_string()).unwrap();
    }
}

#[test]
fn goal_manager_opens_once_per_tab() {
    // Bridge::new opened a manager in w1:p2 for the tab of w1:p1.
    let bridge = Bridge::new();
    let read =
        |name: &str| std::fs::read_to_string(bridge.root.path().join(name)).unwrap_or_default();
    let panels = || read("opened.txt").matches("\npanel\n").count();
    // From the main tab, or the manager itself: switch to the running manager.
    for pane in ["w1:p1", "w1:p2"] {
        let output = open_from(&bridge, pane, "main");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(panels(), 0);
    assert_eq!(read("focused.txt"), "w1:t2\nw1:t2\n");
    // From the Goal watch tab or a helper's tab: refused.
    let refused = open_from(&bridge, "w1:p1", "Goal watch");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("Goal watch tab"));
    start_role(&bridge, "impl", "w1:p5", "claude");
    let refused = open_from(&bridge, "w1:p5", "impl");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("impl helper"));
    assert_eq!(panels(), 0);

    // A second manager started directly for the same tab stops before launching.
    let mut panes: Value = serde_json::from_str(&read("panes.json")).unwrap();
    let mut second = panes["result"]["panes"][1].clone();
    second["pane_id"] = json!("w1:p3");
    second["terminal_id"] = json!("manager-2");
    panes["result"]["panes"]
        .as_array_mut()
        .unwrap()
        .push(second.clone());
    bridge.write("panes.json", &panes.to_string());
    bridge.write(
        "agents.json",
        &json!({"result":{"agents":panes["result"]["panes"]}}).to_string(),
    );
    let launched = read("context");
    let output = bridge
        .command(&["manager", "--agent", "codex"])
        .env("HERDR_PANE_ID", "w1:p3")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already open"));
    assert_eq!(read("context"), launched, "no second manager agent started");

    // Once the manager has gone, opening starts a new one.
    close_manager(&bridge);
    assert!(open_from(&bridge, "w1:p1", "main").status.success());
    assert_eq!(panels(), 1);
}

#[test]
fn a_manager_still_starting_keeps_its_tab_and_a_refused_launch_leaves_no_context() {
    // Bridge::new registered the manager in w1:p2 moments ago; Herdr does not
    // show its agent yet. A second manager pane w1:p3 is about to launch.
    let bridge = Bridge::new();
    for (name, list) in [("panes.json", "panes"), ("agents.json", "agents")] {
        let path = bridge.root.path().join(name);
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entries = value["result"][list].as_array_mut().unwrap();
        let mut second = entries[1].clone();
        second["pane_id"] = json!("w1:p3");
        second["terminal_id"] = json!("manager-2");
        entries.push(second);
        if list == "agents" {
            entries.retain(|pane| pane["pane_id"] != "w1:p2");
        } else if let Some(first) = entries.iter_mut().find(|pane| pane["pane_id"] == "w1:p2") {
            first.as_object_mut().unwrap().remove("agent");
        }
        std::fs::write(path, value.to_string()).unwrap();
    }
    let read =
        |name: &str| std::fs::read_to_string(bridge.root.path().join(name)).unwrap_or_default();
    let contexts = || {
        std::fs::read_dir(manager_dir(&bridge))
            .unwrap()
            .flatten()
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with("m-") && name.len() == "m-.json".len() + 36
            })
            .count()
    };
    let launch = || {
        let mut command = bridge.command(&["manager", "--agent", "codex"]);
        command.env("HERDR_PANE_ID", "w1:p3");
        command
    };
    assert_eq!(contexts(), 1);
    // A second press of the shortcut switches to the starting manager.
    assert!(open_from(&bridge, "w1:p1", "main").status.success());
    assert_eq!(read("opened.txt").matches("\npanel\n").count(), 0);

    // A launch racing it waits for the tab's launch lease, then sees it.
    let lease = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(bridge.state.join("launch/t-original.lock"))
        .unwrap();
    lease.try_lock().unwrap();
    let mut racing = launch().stderr(Stdio::piped()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1000));
    assert!(racing.try_wait().unwrap().is_none(), "waits for the lease");
    drop(lease);
    let output = racing.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already open"));
    let launched = read("context");
    assert_eq!(contexts(), 1, "a refused launch leaves no manager context");

    // Past the start window, a pane that still shows no agent is no manager.
    let record = manager_dir(&bridge).join("t-original-manager.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    value["registered_at"] = json!(0);
    std::fs::write(&record, value.to_string()).unwrap();
    let output = launch().output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_ne!(read("context"), launched);
    assert_eq!(contexts(), 2);
}

#[test]
fn opening_goal_manager_names_only_an_unnamed_original_tab() {
    for (label, renamed) in [("1", true), ("", true), ("api", false)] {
        let bridge = Bridge::new();
        // The first open: no manager runs for the tab yet.
        close_manager(&bridge);
        let output = open_from(&bridge, "w1:p1", label);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let log =
            std::fs::read_to_string(bridge.root.path().join("renamed.txt")).unwrap_or_default();
        assert_eq!(
            log.contains("w1:t1 main"),
            renamed,
            "label {label:?}: {log}"
        );
    }
}

#[test]
fn provider_error_wakes_to_continue_but_a_usage_limit_goes_to_the_user_once() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.write(
        "target.screen",
        "Working on summary\n⎿ API Error: 529 overloaded_error\n› ",
    );
    bridge.ok(&["supervise", &goal]);
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(sent.contains("provider error"), "{sent}");
    assert!(sent.contains("short continue"), "{sent}");
    assert!(
        sent.contains("Now ") && sent.contains("Wake 1 of 100"),
        "{sent}"
    );
    assert!(kinds(&bridge, &goal).iter().any(|k| k == "provider_error"));

    bridge.write(
        "target.screen",
        "You've hit your usage limit. Try again at 3pm.\n› ",
    );
    let notified =
        || std::fs::read_to_string(bridge.root.path().join("notified.txt")).unwrap_or_default();
    for _ in 0..2 {
        assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    }
    assert_eq!(notified().matches("Original agent needs you").count(), 1);
    let watch = bridge.ok(&["watch"]);
    assert!(
        watch["held"].as_str().unwrap().contains("usage limit"),
        "{watch}"
    );
    let inbox = bridge.ok(&["inbox"]);
    assert_eq!(inbox[0]["kind"], "agent_needs_you", "{inbox}");
}

#[test]
fn user_and_project_config_override_prompts_and_workflow_defaults() {
    let bridge = Bridge::new();
    let user = bridge.root.path().join(".config/herdr-goal-manager");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(user.join("guide.md"), "CUSTOM GUIDE BODY").unwrap();
    std::fs::write(
        user.join("config.yaml"),
        "agent: codex\nmodels: {codex: test-model}\nprompts:\n  manager: guide.md\n  opener: CUSTOM OPENER\n  wake_append: USER WAKE RULE\nwatch: {idle_secs: 600, sheep: false}\nloop: {max_assignments: 5}\n",
    )
    .unwrap();
    let project = bridge.workspace.join(".hgm");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("team.md"), "Run make lint before reporting.").unwrap();
    std::fs::write(
        project.join("config.yaml"),
        "prompts:\n  manager_append: [team.md]\n  assignment_append: PROJECT PRACTICE\nloop: {patrol_minutes: 10}\n",
    )
    .unwrap();

    let output = bridge.call(&["manager", "--agent", "codex"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let arguments = std::fs::read_to_string(bridge.root.path().join("arguments")).unwrap();
    assert!(arguments.contains("CUSTOM GUIDE BODY"), "{arguments}");
    assert!(!arguments.contains("You are the user's Goal Manager"));
    assert!(arguments.contains("Run make lint before reporting."));
    assert!(arguments.contains("CUSTOM OPENER"));
    assert!(arguments.contains("test-model"));

    // A goal's own loop wins; unset fields come from user then project defaults.
    let goal = add_file(
        &bridge,
        json!({"objective":"o","acceptance":["a"],"checks":[{"name":"c","command":"true"}],"loop":{"max_assignments":2}}),
    );
    let shown = bridge.ok(&["show", &goal]);
    assert_eq!(shown["spec"]["loop"]["max_assignments"], 2, "{shown}");
    assert_eq!(shown["spec"]["loop"]["patrol_minutes"], 10);
    let manual = bridge.goal();
    assert_eq!(
        bridge.ok(&["show", &manual])["spec"]["loop"]["max_assignments"],
        5
    );

    bridge.ok(&["confirm", &goal, "--evidence", "User: go"]);
    bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "Write summary.json",
    ]);
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(sent.contains("PROJECT PRACTICE"), "{sent}");
    assert_eq!(bridge.ok(&["watch"])["idle_secs"], 600);
    assert_eq!(bridge.ok(&["config"])["watch"]["sheep"], false);
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(sent.contains("USER WAKE RULE"), "{sent}");

    // A project may add to the guide but not replace it.
    std::fs::write(project.join("config.yaml"), "prompts: {manager: team.md}\n").unwrap();
    let refused = bridge.call(&["config"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("only be set in the user config"));
}

fn correctable_goal(bridge: &Bridge) -> String {
    let goal = add_file(
        bridge,
        json!({"objective":"o","acceptance":["a"],"checks":[{"name":"c","command":"true"}],
               "tasks":[{"id":"work","instruction":"w"}],
               "loop":{"max_corrections":2,"check_on_settle":false}}),
    );
    bridge.ok(&["confirm", &goal, "--evidence", "User: go"]);
    goal
}

#[test]
fn corrections_stay_within_budget_and_never_type_into_drafts_or_busy_codex() {
    let bridge = Bridge::new();
    let goal = correctable_goal(&bridge);
    // Codex is not known to queue input typed during a turn: wait for it.
    bridge.inventory("working", "idle", "original");
    assert!(
        !bridge
            .call(&["correct", &goal, "--task", "work", "Use the CSV header"])
            .status
            .success()
    );
    bridge.inventory("idle", "idle", "original");
    bridge.write("target.screen", "› my unfinished note");
    assert!(
        !bridge
            .call(&["correct", &goal, "--task", "work", "x"])
            .status
            .success()
    );
    bridge.write("target.screen", "Done\n› ");
    let sent = bridge.ok(&[
        "correct",
        &goal,
        "--task",
        "work",
        "Totals must include tax",
    ]);
    assert_eq!(sent["status"], "admitted", "{sent}");
    let text = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(text.contains("Goal Manager correction for goal"), "{text}");
    assert!(text.contains("Totals must include tax"));
    assert!(kinds(&bridge, &goal).iter().any(|k| k == "correction"));
    bridge.ok(&["correct", &goal, "--task", "work", "second"]);
    let refused = bridge.call(&["correct", &goal, "--task", "work", "third"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("Correction budget exhausted"));
    let shown = bridge.ok(&["show", &goal]);
    assert!(
        shown["waiting"]["note"]
            .as_str()
            .unwrap()
            .contains("Correction budget"),
        "{shown}"
    );
}

#[test]
fn a_correction_reaches_a_working_claude_only_through_an_empty_input() {
    let bridge = Bridge::new();
    let origin = json!({"pane":"w1:p3","tab":"w1:t3","terminal":"claude-term","agent":"claude","session":null});
    let output = bridge
        .command(&["manager", "--agent", "codex"])
        .env("HGM_ORIGIN_JSON", origin.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let context = std::fs::read_to_string(bridge.root.path().join("context")).unwrap();
    let inventory = |status: &str| {
        let panes = json!([
            {"pane_id":"w1:p2","tab_id":"w1:t2","terminal_id":"manager","agent":"codex","agent_status":"idle","agent_session":{"value":"manager"}},
            {"pane_id":"w1:p3","tab_id":"w1:t3","terminal_id":"claude-term","agent":"claude","agent_status":status,"agent_session":{"value":"c1"}}
        ]);
        bridge.write("panes.json", &json!({"result":{"panes":panes}}).to_string());
        bridge.write(
            "agents.json",
            &json!({"result":{"agents":panes}}).to_string(),
        );
    };
    let run = |args: &[&str]| {
        bridge
            .command(args)
            .env("HGM_CONTEXT", &context)
            .output()
            .unwrap()
    };
    let goal = correctable_goal(&bridge);
    inventory("working");
    // Panes other than w1:p1 read manager.screen in the fixture.
    bridge.write("manager.screen", "Editing parser.rs\n❯ ");
    let steered = run(&[
        "correct",
        &goal,
        "--task",
        "work",
        "Fix the parser, not the test",
    ]);
    assert!(
        steered.status.success(),
        "{}",
        String::from_utf8_lossy(&steered.stderr)
    );
    let text = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(text.contains("w1:p3\nGoal Manager correction"), "{text}");
    bridge.write("manager.screen", "Do you want to run cargo test?\n❯ 1. Yes");
    assert!(
        !run(&["correct", &goal, "--task", "work", "again"])
            .status
            .success()
    );
    inventory("blocked");
    bridge.write("manager.screen", "Editing\n❯ ");
    assert!(
        !run(&["correct", &goal, "--task", "work", "again"])
            .status
            .success()
    );
}

#[test]
fn new_manager_rules_reach_claude_codex_and_copilot_launches() {
    const RULES: [&str; 4] = [
        "The user's verbatim words belong in evidence",
        "use send --goal GOAL --task TASK --interrupt, hgm correct, or hgm",
        "Run hgm reanchor GOAL.",
        "steer it\nback with hgm correct",
    ];
    let bridge = Bridge::new();
    let read = |name: &str| std::fs::read_to_string(bridge.root.path().join(name)).unwrap();
    let codex = read("arguments");
    let instructions = codex
        .lines()
        .find(|arg| arg.starts_with("developer_instructions="))
        .unwrap();
    let decoded: String =
        serde_json::from_str(instructions.trim_start_matches("developer_instructions=")).unwrap();
    for rule in RULES {
        assert!(decoded.contains(rule), "codex misses {rule}");
    }
    assert!(decoded.contains("reanchor") && decoded.contains("correct"));

    assert!(
        bridge
            .call(&["manager", "--agent", "claude"])
            .status
            .success()
    );
    let claude = read("arguments");
    let start = claude.find("--append-system-prompt\n").unwrap();
    for rule in RULES {
        assert!(claude[start..].contains(rule), "claude misses {rule}");
    }

    assert!(
        bridge
            .call(&["manager", "--agent", "copilot"])
            .status
            .success()
    );
    let directory = read("instruction-dirs");
    let copilot = std::fs::read_to_string(
        PathBuf::from(directory).join(".github/instructions/hgm-manager.instructions.md"),
    )
    .unwrap();
    for rule in RULES {
        assert!(copilot.contains(rule), "copilot misses {rule}");
    }
}

fn claude_origin(bridge: &Bridge) -> (String, impl Fn(&str)) {
    let origin = json!({"pane":"w1:p3","tab":"w1:t3","terminal":"claude-term","agent":"claude","session":null});
    let output = bridge
        .command(&["manager", "--agent", "codex"])
        .env("HGM_ORIGIN_JSON", origin.to_string())
        .output()
        .unwrap();
    assert!(output.status.success());
    let context = std::fs::read_to_string(bridge.root.path().join("context")).unwrap();
    let root = bridge.root.path().to_path_buf();
    let inventory = move |status: &str| {
        let panes = json!([
            {"pane_id":"w1:p2","tab_id":"w1:t2","terminal_id":"manager","agent":"codex","agent_status":"idle","agent_session":{"value":"manager"}},
            {"pane_id":"w1:p3","tab_id":"w1:t3","terminal_id":"claude-term","agent":"claude","agent_status":status,"agent_session":{"value":"c1"}}
        ]);
        std::fs::write(
            root.join("panes.json"),
            json!({"result":{"panes":panes}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join("agents.json"),
            json!({"result":{"agents":panes}}).to_string(),
        )
        .unwrap();
    };
    (context, inventory)
}

#[test]
fn interrupting_send_reaches_a_working_claude_counts_and_confirms_consumption() {
    let bridge = Bridge::new();
    let (context, inventory) = claude_origin(&bridge);
    let run = |args: &[&str]| {
        bridge
            .command(args)
            .env("HGM_CONTEXT", &context)
            .output()
            .unwrap()
    };
    let goal = correctable_goal(&bridge);
    inventory("working");
    bridge.write("manager.screen", "Editing parser.rs\n❯ ");
    // Without --interrupt a working agent is not disturbed.
    let plain = run(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "Switch to the CSV header",
    ]);
    assert!(!plain.status.success());
    assert!(String::from_utf8_lossy(&plain.stderr).contains("--interrupt"));
    let sent = run(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "--interrupt",
        "Switch to the CSV header",
    ]);
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    let sent: Value = serde_json::from_slice(&sent.stdout).unwrap();
    assert_eq!(sent["interrupted"], true, "{sent}");
    assert_eq!(sent["consumed"], true, "{sent}");
    let events = bridge.ok(&["log", &goal]);
    let assigned = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "assigned")
        .unwrap();
    assert_eq!(assigned["detail"]["interrupted"], true);
    assert_eq!(assigned["detail"]["consumed"], true);
    // A draft, a dialog, or a blocked agent is never typed into.
    for (status, screen) in [
        ("working", "Editing\n❯ my own note"),
        ("working", "Do you want to run cargo test?\n❯ 1. Yes"),
        ("blocked", "Editing\n❯ "),
    ] {
        inventory(status);
        bridge.write("manager.screen", screen);
        let refused = run(&[
            "send",
            "--goal",
            &goal,
            "--task",
            "work",
            "--interrupt",
            "x",
        ]);
        assert!(!refused.status.success(), "{status}: {screen}");
    }
}

#[test]
fn a_running_copilot_listed_settled_is_working_and_takes_only_an_interrupt() {
    let bridge = Bridge::new();
    let goal = correctable_goal(&bridge);
    start_role(&bridge, "impl", "w1:p5", "copilot");
    // Herdr lists the role settled while Copilot runs a tool; its input box is
    // empty and the status line offers ctrl+c interrupt.
    bridge.write(
        "manager.screen",
        "○ Shell Wait 120 seconds\n  sleep 120\n╻▄▄▄▄\n┃\n╹▀▀▀▀\n ◎ Running bash · 53s ctrl+c interrupt Copilot Fusion\n",
    );
    let send = |extra: &[&str]| {
        let mut args = vec!["send", "--to", "impl", "--goal", &goal, "--task", "work"];
        args.extend_from_slice(extra);
        bridge.call(&args)
    };
    let refused = send(&["Use the CSV header"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("is working"));
    let sent = send(&["--interrupt", "Use the CSV header"]);
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    let sent: Value = serde_json::from_slice(&sent.stdout).unwrap();
    assert_eq!(sent["interrupted"], true, "{sent}");
    let typed = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(typed.lines().any(|line| line == "w1:p5"), "{typed}");
}

#[test]
fn interrupting_send_waits_for_providers_that_do_not_queue_and_flags_unconsumed_text() {
    let bridge = Bridge::new();
    let goal = correctable_goal(&bridge);
    bridge.inventory("working", "idle", "original");
    let refused = bridge.call(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "--interrupt",
        "x",
    ]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("not known to queue input"));
    bridge.inventory("idle", "idle", "original");
    bridge.write("after-prompt.screen", "› Write summary.json");
    let sent = bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "Write summary.json",
    ]);
    assert_eq!(sent["consumed"], false, "{sent}");
    assert!(sent["warning"].as_str().unwrap().contains("do not resend"));
}

#[test]
fn compaction_on_screen_asks_the_manager_for_a_goal_check_and_reanchor_is_free() {
    let bridge = Bridge::new();
    let goal = correctable_goal(&bridge);
    bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "Write summary.json",
    ]);
    bridge.write(
        "target.screen",
        "✻ Conversation compacted (ctrl+o for history)\n› ",
    );
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(
        kinds(&bridge, &goal)
            .iter()
            .any(|k| k == "compaction_detected")
    );
    // The next tick sees the event and asks the manager (cooldown permitting).
    let watch = bridge.ok(&["watch"]);
    let pending = watch["pending"].as_str().unwrap_or_default().to_string();
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(
        pending.contains("goal check due") || sent.contains("goal check due") || {
            assert!(bridge.call(&["watch-loop", "--once"]).status.success());
            bridge.ok(&["watch"])["pending"]
                .as_str()
                .unwrap_or_default()
                .contains("goal check due")
        },
        "{watch}"
    );
    let before = bridge.ok(&["show", &goal]);
    bridge.write("target.screen", "Done\n› ");
    let reanchor = bridge.ok(&["reanchor", &goal, "--task", "work"]);
    assert_eq!(reanchor["status"], "admitted", "{reanchor}");
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(sent.contains("restate in at most 5 lines"), "{sent}");
    assert!(kinds(&bridge, &goal).iter().any(|k| k == "reanchor"));
    // Goal checks do not spend the assignment budget.
    for n in 0..9 {
        bridge.write("target.screen", "Done\n› ");
        bridge.ok(&["reanchor", &goal, "--task", "work", &format!("check {n}")]);
    }
    assert_eq!(bridge.ok(&["show", &goal])["waiting"], before["waiting"]);
}

#[test]
fn a_goal_checkout_is_a_herdr_worktree_where_work_and_checks_run() {
    let bridge = Bridge::new();
    let root = bridge.root.path();
    let git = |dir: &std::path::Path, args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "start"]);
    let checkout = root.join("checkout");
    let spec = |branch: &str, path: &std::path::Path| {
        json!({"objective":"Fix the copy","acceptance":["Copy fixed"],
            "checks":[{"name":"where","command":"pwd > where.txt"}],
            "checkout":{"repository":repo,"base":"HEAD","branch":branch,"path":path}})
    };
    let calls = || std::fs::read_to_string(root.join("worktree.txt")).unwrap_or_default();

    // A branch that exists without a worktree is the person's call: refused.
    git(&repo, &["branch", "taken"]);
    let taken = add_file(&bridge, spec("taken", &root.join("unused")));
    let refused = bridge.call(&["confirm", &taken, "--evidence", "User: go"]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success() && stderr.contains("already exists"),
        "{stderr}"
    );
    assert!(bridge.ok(&["show", &taken])["confirmed"].is_null());
    assert!(calls().is_empty());

    // Confirming creates the checkout as a Herdr worktree workspace.
    let goal = add_file(&bridge, spec("todo/copy", &checkout));
    let shown = bridge.ok(&["confirm", &goal, "--evidence", "User: go"]);
    assert_eq!(
        shown["checkout"]["path"],
        checkout.to_str().unwrap(),
        "{shown}"
    );
    assert_eq!(shown["checkout"]["branch"], "todo/copy");
    assert_eq!(shown["checkout"]["workspace_id"], "w9");
    assert!(calls().contains("worktree create --cwd") && calls().contains("--no-focus"));
    assert_eq!(
        git(&checkout, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "todo/copy"
    );
    // Another goal on the same branch reopens that worktree instead.
    let again = add_file(&bridge, spec("todo/copy", &root.join("elsewhere")));
    let reopened = bridge.ok(&["confirm", &again, "--evidence", "User: go"]);
    assert_eq!(
        std::fs::canonicalize(reopened["checkout"]["path"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&checkout).unwrap()
    );
    assert_eq!(reopened["checkout"]["workspace_id"], "w8");

    // Linked assignments say where to work; checks run there.
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Fix the copy"]);
    let sent = std::fs::read_to_string(root.join("sent.txt")).unwrap();
    assert!(
        sent.contains(&format!(
            "Work in {} (branch todo/copy)",
            checkout.display()
        )),
        "{sent}"
    );
    bridge.ok(&["complete", &goal, "work", "--evidence", "Copy fixed"]);
    assert_eq!(bridge.ok(&["run", &goal])["status"], "ready_for_review");
    let ran_in = std::fs::read_to_string(checkout.join("where.txt")).unwrap();
    assert_eq!(
        std::fs::canonicalize(ran_in.trim()).unwrap(),
        std::fs::canonicalize(&checkout).unwrap()
    );
    assert!(!bridge.workspace.join("where.txt").exists());

    // Removal refuses a running watch, then uncommitted changes; nothing goes.
    let remove = || bridge.call(&["checkout", "remove", &goal]);
    let refused = remove();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success() && stderr.contains("running watch"),
        "{stderr}"
    );
    bridge.ok(&["wait", &goal, "--reason", "Review the copy"]);
    let refused = remove();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success() && stderr.contains("1 uncommitted change"),
        "{stderr}"
    );
    assert!(checkout.exists() && !calls().contains("worktree remove"));
    // Clean, it goes through Herdr without --force, and the branch stays.
    std::fs::remove_file(checkout.join("where.txt")).unwrap();
    let removed = bridge.ok(&["checkout", "remove", &goal]);
    assert!(removed["checkout"]["removed_at"].is_string(), "{removed}");
    assert!(!checkout.exists());
    assert!(calls().contains("worktree remove --workspace w9") && !calls().contains("--force"));
    git(&repo, &["rev-parse", "--verify", "refs/heads/todo/copy"]);
    assert!(
        kinds(&bridge, &goal)
            .iter()
            .any(|k| k == "checkout_removed")
    );
    // Later checks say the checkout is gone, not that a folder is missing.
    let resumed = bridge.call(&["resume", &goal]);
    let stderr = String::from_utf8_lossy(&resumed.stderr);
    assert!(
        !resumed.status.success() && stderr.contains("was removed with hgm checkout remove"),
        "{stderr}"
    );
}

#[test]
fn goals_on_one_pane_share_a_watch_and_a_waiting_goal_gets_no_wake() {
    let bridge = Bridge::new();
    let spec = |objective: &str| {
        json!({"objective":objective,"acceptance":["Done"],
            "checks":[{"name":"ok","command":"true"}]})
    };
    let first = confirmed(&bridge, spec("First outcome"));
    let second = confirmed(&bridge, spec("Second outcome"));
    bridge.ok(&[
        "send",
        "--goal",
        &first,
        "--task",
        "work",
        "First assignment",
    ]);
    let joined = bridge.ok(&[
        "send",
        "--goal",
        &second,
        "--task",
        "work",
        "Second assignment",
    ]);
    assert_eq!(joined["supervision"], "armed", "{joined}");
    let watch = bridge.ok(&["watch"]);
    assert_eq!(watch["goals"], json!([first, second]), "{watch}");
    assert!(kinds(&bridge, &second).iter().any(|k| k == "watch_joined"));

    let watch_path = state_directory(&bridge).join("manager/t-original-watch.json");
    let sent = bridge.root.path().join("sent.txt");
    // Lets the worker settle on `screen` and returns the manager's wake, if any.
    let settle = |screen: &str| {
        let mut watch: Value =
            serde_json::from_slice(&std::fs::read(&watch_path).unwrap()).unwrap();
        watch["last_sent"] = json!(0);
        std::fs::write(&watch_path, watch.to_string()).unwrap();
        bridge.write("target.screen", screen);
        bridge.inventory("done", "idle", "original");
        let before = std::fs::read_to_string(&sent).unwrap().len();
        assert!(bridge.call(&["watch-loop", "--once"]).status.success());
        std::fs::read_to_string(&sent).unwrap()[before..].to_string()
    };
    // Wakes and settle checks follow the latest linked assignment to the pane.
    let wake = settle("Second assignment done\n› ");
    assert!(
        wake.contains(&second) && wake.contains("Second assignment") && !wake.contains(&first),
        "{wake}"
    );
    assert!(kinds(&bridge, &second).iter().any(|k| k == "settle_checks"));
    assert!(!kinds(&bridge, &first).iter().any(|k| k == "settle_checks"));

    // The first goal gets the newest assignment, then waits: it leaves the
    // watch, and the next wake is about the second goal only.
    bridge.write("target.screen", "Finished output\n› ");
    bridge.inventory("idle", "idle", "original");
    bridge.ok(&[
        "send",
        "--goal",
        &first,
        "--task",
        "work",
        "First follow-up",
    ]);
    bridge.ok(&["wait", &first, "--reason", "Ask the user"]);
    let watch = bridge.ok(&["watch"]);
    assert!(
        watch["active"] == true && watch["goal"] == second.as_str() && watch["goals"].is_null(),
        "{watch}"
    );
    assert!(kinds(&bridge, &first).iter().any(|k| k == "watch_ended"));
    let wake = settle("Second assignment checked again\n› ");
    assert!(wake.contains(&second) && !wake.contains(&first), "{wake}");

    // The last goal leaving ends the watch.
    bridge.ok(&["wait", &second, "--reason", "Ask the user"]);
    assert_eq!(bridge.ok(&["watch"])["active"], false);
}

#[test]
fn cancel_and_pause_end_watch_records_and_supervise_recovers_a_stale_one() {
    let bridge = Bridge::new();
    let first = bridge.goal();
    bridge.ok(&["supervise", &first]);
    assert_eq!(bridge.ok(&["watch"])["active"], true);
    bridge.ok(&["cancel", &first]);
    let watch = bridge.ok(&["watch"]);
    assert_eq!(watch["active"], false, "{watch}");
    assert_eq!(watch["status"], "stopped_by_goal");

    let second = bridge.goal();
    bridge.ok(&["supervise", &second]);
    bridge.ok(&["pause", &second]);
    assert_eq!(bridge.ok(&["watch"])["active"], false);

    // A just-armed watch may still be starting its process: not stale yet, so
    // another goal for the same pane joins it.
    let third = bridge.goal();
    let fourth = bridge.goal();
    bridge.ok(&["supervise", &third]);
    let joined = bridge.ok(&["supervise", &fourth]);
    assert_eq!(joined["goals"], json!([third, fourth]), "{joined}");
    // Later, no watch process holds the lease: the shared record is stale.
    age_watch(&bridge, 60);
    let watch = bridge.ok(&["supervise", &fourth]);
    assert_eq!(watch["goal"], fourth.as_str(), "{watch}");
    assert!(kinds(&bridge, &third).iter().any(|k| k == "watch_ended"));
}

/// Pins a helper role in the fixture: Herdr returns pane `pane` for the new
/// tab, and the pane runs `agent` once started.
fn start_role(bridge: &Bridge, kind: &str, pane: &str, agent: &str) -> Value {
    let tab = pane.replace(":p", ":t");
    bridge.write(
        "tab-create.json",
        &json!({"result":{"root_pane":{"pane_id":pane,"tab_id":tab},"tab":{"tab_id":tab}}})
            .to_string(),
    );
    let mut panes: Vec<Value> = serde_json::from_str::<Value>(
        &std::fs::read_to_string(bridge.root.path().join("panes.json")).unwrap(),
    )
    .unwrap()["result"]["panes"]
        .as_array()
        .unwrap()
        .clone();
    panes.push(json!({"pane_id":pane,"tab_id":tab,"terminal_id":format!("{kind}-term"),"agent":agent,"agent_status":"idle","agent_session":{"value":kind}}));
    bridge.write(
        "role-panes.json",
        &json!({"result":{"panes":panes,"agents":panes}}).to_string(),
    );
    // Panes other than w1:p1 read manager.screen in the fixture.
    bridge.write("manager.screen", "Ready\n❯ ");
    bridge.ok(&["role", "start", kind, "--agent", agent])
}

fn two_task_goal(bridge: &Bridge) -> String {
    let goal = add_file(
        bridge,
        json!({"objective":"o","acceptance":["a"],"checks":[{"name":"c","command":"true"}],
               "tasks":[{"id":"work","instruction":"w"},{"id":"other","instruction":"x"}],
               "loop":{"check_on_settle":false}}),
    );
    bridge.ok(&["confirm", &goal, "--evidence", "User: go"]);
    goal
}

#[test]
fn assignments_route_to_pinned_roles_one_owner_per_task() {
    let bridge = Bridge::new();
    let goal = two_task_goal(&bridge);
    let refused = bridge.call(&[
        "send", "--to", "impl", "--goal", &goal, "--task", "work", "x",
    ]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("not pinned"));

    let started = start_role(&bridge, "impl", "w1:p5", "claude");
    assert_eq!(started["pane"], "w1:p5", "{started}");
    let created = std::fs::read_to_string(bridge.root.path().join("created.txt")).unwrap();
    assert!(
        created.contains("--no-focus") && created.contains("HGM_ROLE=impl"),
        "{created}"
    );
    let launched = std::fs::read_to_string(bridge.root.path().join("started.txt")).unwrap();
    assert!(launched.contains("Bash(hgm report:*)"), "{launched}");
    // The multi-line guide reaches Claude through a file, never as an argument.
    let guide = launched
        .lines()
        .skip_while(|line| *line != "--append-system-prompt-file")
        .nth(1)
        .expect("guide file argument");
    assert!(
        std::fs::read_to_string(guide)
            .unwrap()
            .contains("hgm report")
    );
    assert!(
        launched
            .lines()
            .any(|s| s == "--dangerously-skip-permissions")
    );
    let roles = bridge.ok(&["role", "list"]);
    assert_eq!(roles[0]["role"], "main");
    assert_eq!(roles[1]["role"], "impl");

    bridge.ok(&[
        "send",
        "--to",
        "impl",
        "--goal",
        &goal,
        "--task",
        "work",
        "Implement the parser",
    ]);
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(
        sent.contains(&format!("w1:p5\nGoal {goal}, work, assigned to impl.")),
        "{sent}"
    );
    assert_eq!(bridge.ok(&["watch"])["role"], "impl");
    let taken = bridge.call(&["send", "--goal", &goal, "--task", "work", "x"]);
    assert!(String::from_utf8_lossy(&taken.stderr).contains("belongs to impl"));
    bridge.ok(&["send", "--goal", &goal, "--task", "other", "Write the docs"]);

    // An impl agent runs only show and report.
    let as_impl = |args: &[&str]| {
        bridge
            .command(args)
            .env("HGM_ROLE", "impl")
            .output()
            .unwrap()
    };
    assert!(!as_impl(&["status"]).status.success());
    assert!(
        as_impl(&["report", &goal, "--task", "work", "Parser done; tests pass"])
            .status
            .success()
    );
    assert!(kinds(&bridge, &goal).iter().any(|k| k == "report"));

    bridge.ok(&["role", "stop", "impl"]);
    let closed = std::fs::read_to_string(bridge.root.path().join("closed.txt")).unwrap();
    assert_eq!(closed.trim(), "w1:t5");
    assert!(!bridge.call(&["send", "--to", "impl", "x"]).status.success());
}

#[test]
fn codex_and_copilot_roles_start_with_single_line_arguments() {
    // The fixture refuses line breaks in agent arguments, as Herdr does.
    let bridge = Bridge::new();
    let codex = start_role(&bridge, "impl", "w1:p5", "codex");
    assert_eq!(codex["agent"], "codex");
    assert_eq!(
        start_role(&bridge, "reviewer", "w1:p6", "copilot")["agent"],
        "copilot"
    );
    // Herdr types only `codex`; the launcher first on the tab's PATH carries
    // the guide and environment, which exceed the shell's 1024-byte line.
    let launched = std::fs::read_to_string(bridge.root.path().join("started.txt")).unwrap();
    assert!(!launched.contains("developer_instructions"), "{launched}");
    let launcher = std::path::Path::new(codex["launcher"].as_str().unwrap());
    let script = std::fs::read_to_string(launcher).unwrap();
    assert!(
        script.starts_with("#!/bin/sh\nexec ")
            && script.contains("developer_instructions=")
            && script.contains("--dangerously-bypass-approvals-and-sandbox")
            && script.ends_with(" \"$@\"\n"),
        "{script}"
    );
    let created = std::fs::read_to_string(bridge.root.path().join("created.txt")).unwrap();
    let bin = launcher.parent().unwrap().display().to_string();
    assert!(created.contains(&format!("PATH={bin}:")), "{created}");
}

#[test]
fn a_role_that_never_starts_closes_its_tab_and_one_that_runs_is_not_started_twice() {
    let bridge = Bridge::new();
    // Copilot waits on its own folder-trust question: Herdr's start fails.
    bridge.write("fail-start", "");
    bridge.write(
        "tab-create.json",
        &json!({"result":{"root_pane":{"pane_id":"w1:p6","tab_id":"w1:t6"},"tab":{"tab_id":"w1:t6"}}})
            .to_string(),
    );
    let failed = bridge.call(&["role", "start", "reviewer", "--agent", "copilot"]);
    assert!(!failed.status.success());
    let message = String::from_utf8_lossy(&failed.stderr);
    assert!(
        message.contains("the tab is closed") && message.contains("trust"),
        "{message}"
    );
    let closed = std::fs::read_to_string(bridge.root.path().join("closed.txt")).unwrap();
    assert_eq!(closed, "w1:t6\n");
    // Only main is listed: the failed role was not registered.
    assert_eq!(
        bridge.ok(&["role", "list"]).as_array().map_or(0, Vec::len),
        1
    );
    // Started for real, the role is not started a second time.
    std::fs::remove_file(bridge.root.path().join("fail-start")).unwrap();
    start_role(&bridge, "reviewer", "w1:p6", "claude");
    let again = bridge.call(&["role", "start", "reviewer", "--agent", "claude"]);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("reviewer already runs in pane w1:p6"));
}

#[test]
fn reviews_are_requested_from_a_reviewer_and_recorded_apart_from_work() {
    let bridge = Bridge::new();
    let goal = two_task_goal(&bridge);
    start_role(&bridge, "impl", "w1:p5", "claude");
    start_role(&bridge, "reviewer", "w1:p6", "claude");
    let launched = std::fs::read_to_string(bridge.root.path().join("started.txt")).unwrap();
    assert!(launched.contains("Bash(hgm review-submit:*)"));
    assert!(
        !bridge
            .call(&["review", &goal, "--reviewer", "impl", "x"])
            .status
            .success()
    );
    let requested = bridge.ok(&["review", &goal, "Check summary.json against the acceptance"]);
    let review = requested["review"].as_str().unwrap().to_string();
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(
        sent.contains(&format!("w1:p6\nReview request {review}")),
        "{sent}"
    );
    assert!(sent.contains("hgm review-submit"));

    let submit = |role: &str, verdict: &str, findings: &str| {
        let mut child = bridge
            .command(&[
                "review-submit",
                &goal,
                "--review",
                &review,
                "--verdict",
                verdict,
            ])
            .env("HGM_ROLE", role)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(findings.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let blocking = r#"[{"severity":"blocking","where":"summary.json","issue":"total excludes tax","evidence":"1760 != 1936"}]"#;
    assert!(!submit("impl", "revise", blocking).status.success());
    assert!(!submit("reviewer", "approve", blocking).status.success());
    // No watch follows the goal: the verdict reaches the person and, with its
    // input free, the manager at once.
    bridge.write("manager.screen", "› ");
    let output = submit("reviewer", "revise", blocking);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let notified = std::fs::read_to_string(bridge.root.path().join("notified.txt")).unwrap();
    assert!(notified.contains("Review arrived"), "{notified}");
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(
        sent.contains(&format!(
            "w1:p2\nGoal Manager observation for {goal}: review {review}"
        )),
        "{sent}"
    );
    assert!(
        !submit("reviewer", "revise", blocking).status.success(),
        "submitted twice"
    );
    let log = bridge.ok(&["log", &goal]);
    let reviewed = log
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "reviewed")
        .unwrap();
    assert_eq!(reviewed["detail"]["verdict"], "revise");
    assert_eq!(reviewed["detail"]["current"], true);
    assert_eq!(reviewed["detail"]["reviewer"], "reviewer");
    assert!(!kinds(&bridge, &goal).iter().any(|k| k == "assigned"));
}

/// Writes the fixture's pane and agent inventory: worker `w1:p1`, manager `w1:p2`.
fn write_inventory(root: &std::path::Path, worker: &str, manager: &str, terminal: &str) {
    let agents = json!([
        {"pane_id":"w1:p1","tab_id":"w1:t1","terminal_id":terminal,"agent":"codex","agent_status":worker,"agent_session":{"value":"worker"},"focused":false},
        {"pane_id":"w1:p2","tab_id":"w1:t2","terminal_id":"manager","agent":"codex","agent_status":manager,"agent_session":{"value":"manager"},"focused":true}
    ]);
    std::fs::write(
        root.join("panes.json"),
        json!({"result":{"panes":agents}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        root.join("agents.json"),
        json!({"result":{"agents":agents}}).to_string(),
    )
    .unwrap();
}

/// A fake Herdr socket at the bridge's socket path. Every `events.subscribe`
/// is confirmed; the agent-state subscription is handed to `state`, which may
/// change the fixture, write events, or close the stream. Other connections
/// stay open and silent until the listener's lifetime ends.
fn fake_socket(
    bridge: &Bridge,
    state: impl FnOnce(&mut std::os::unix::net::UnixStream) + Send + 'static,
) -> std::thread::JoinHandle<Vec<String>> {
    use std::io::{BufRead, BufReader};
    let listener = std::os::unix::net::UnixListener::bind(bridge.socket()).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let mut state = Some(state);
        let mut requests = vec![];
        let mut open = vec![];
        let end = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while std::time::Instant::now() < end {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            stream
                .write_all(
                    b"{\"id\":\"hgm-watch\",\"result\":{\"type\":\"subscription_started\"}}\n",
                )
                .unwrap();
            if line.contains("pane.agent_status_changed")
                && let Some(state) = state.take()
            {
                let mut stream = stream;
                std::thread::spawn(move || state(&mut stream));
            } else {
                open.push(stream);
            }
            requests.push(line);
        }
        requests
    })
}

fn status_event(status: &str) -> Vec<u8> {
    format!(
        "{}\n",
        json!({"event":"pane.agent_status_changed","data":{"pane_id":"w1:p1","agent_status":status}})
    )
    .into_bytes()
}

/// How long a test waits for a state another process records; slow CI hosts
/// must not turn a late state into a failure.
const WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Polls `probe` until it returns a value, failing after [`WAIT`].
fn eventually<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Runs a state-changing command, retrying while the state is busy: a running
/// watch saving its record makes the CLI ask the caller to retry.
fn ok_when_free(bridge: &Bridge, args: &[&str]) -> Value {
    eventually(&args.join(" "), || {
        let output = bridge.call(args);
        if output.status.success() {
            return Some(serde_json::from_slice(&output.stdout).unwrap());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("is busy"), "{}: {stderr}", args.join(" "));
        None
    })
}

/// The Herdr sidebar calls the fixture recorded.
fn metadata(bridge: &Bridge) -> String {
    std::fs::read_to_string(bridge.root.path().join("metadata.txt")).unwrap_or_default()
}

/// Complete sidebar reports in `metadata`: each ends with the budget token.
fn sidebar_reports(metadata: &str) -> usize {
    metadata.matches("\n--token\nbudget=").count()
}

#[test]
fn event_watch_reads_the_screen_on_each_change_so_a_dialog_reported_done_is_held() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing files\n› ");
    bridge.ok(&["supervise", &goal]);
    let root = bridge.root.path().to_path_buf();
    let server = fake_socket(&bridge, move |stream| {
        std::thread::sleep(std::time::Duration::from_millis(700));
        // Herdr reports `done` while an approval dialog is on screen.
        std::fs::write(
            root.join("target.screen"),
            "Create file\nDo you want to create summary.json?\n❯ 1. Yes\n  2. No",
        )
        .unwrap();
        write_inventory(&root, "done", "idle", "original");
        stream.write_all(&status_event("done")).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(4));
    });
    // Polling would not look again within 3 s, and the reconcile waits 30 s:
    // only the event can lead to the second read.
    let output = bridge.call(&["watch-loop", "--run-for-secs", "3"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let watch = bridge.ok(&["watch"]);
    assert_eq!(watch["mode"], "events", "{watch}");
    assert!(
        watch["held"]
            .as_str()
            .is_some_and(|held| held.contains("approval")),
        "{watch}"
    );
    let notified = std::fs::read_to_string(bridge.root.path().join("notified.txt")).unwrap();
    assert_eq!(notified.matches("Original agent needs you").count(), 1);
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap_or_default();
    assert!(
        !sent.lines().any(|line| line == "w1:p1"),
        "nothing is typed into the dialog: {sent}"
    );
    // Held: the user was told; the manager is not woken while the dialog stays.
    assert!(
        !sent.lines().any(|line| line == "w1:p2"),
        "no wake while held: {sent}"
    );
    let requests = server.join().unwrap();
    assert!(
        requests
            .iter()
            .any(|r| r.contains("pane.output_matched") && r.contains("Do you want to")),
        "{requests:?}"
    );
}

#[test]
fn a_dialog_that_stays_while_the_state_moves_is_told_once() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    let dialog =
        "Bash command\nThis command requires approval\nDo you want to proceed?\n❯ 1. Yes\n  2. No";
    let observe = |worker: &str, screen: &str| {
        bridge.inventory(worker, "idle", "original");
        bridge.write("target.screen", screen);
        assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    };
    let attentions = || {
        bridge
            .ok(&["log", &goal])
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "attention")
            .count()
    };
    // Shown while working, then Herdr reports the pane blocked: one notice.
    observe("working", dialog);
    observe("blocked", dialog);
    assert_eq!(attentions(), 1);
    assert!(
        bridge.ok(&["watch"])["held"]
            .as_str()
            .is_some_and(|held| held.contains("blocked"))
    );
    // Answered: the worker runs again, so the next dialog is news.
    observe("working", "Running the check\n");
    observe("blocked", dialog);
    assert_eq!(attentions(), 2);
    let notified = std::fs::read_to_string(bridge.root.path().join("notified.txt")).unwrap();
    assert_eq!(notified.matches("Original agent needs you").count(), 2);
}

#[test]
fn dropped_events_fall_back_to_polling_with_a_full_re_read() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing files\n› ");
    bridge.ok(&["supervise", &goal]);
    let root = bridge.root.path().to_path_buf();
    let _server = fake_socket(&bridge, move |stream| {
        std::thread::sleep(std::time::Duration::from_millis(700));
        // The worker finishes, but the stream ends without reporting it.
        std::fs::write(root.join("target.screen"), "Finished output\n› ").unwrap();
        write_inventory(&root, "done", "idle", "original");
        stream.shutdown(std::net::Shutdown::Both).unwrap();
    });
    let output = bridge.call(&["watch-loop", "--run-for-secs", "3"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(bridge.ok(&["watch"])["mode"], "polling");
    let log = bridge.ok(&["log", &goal]);
    let modes: Vec<_> = log
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "watch_mode")
        .collect();
    assert_eq!(modes.len(), 1, "{log}");
    assert_eq!(modes[0]["detail"]["mode"], "polling");
    // The re-read right after the drop found the finished worker and woke the manager.
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(sent.starts_with("w1:p2\n"), "{sent}");
    let log = bridge.ok(&["log", &goal]);
    assert!(
        log.as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "wake" && e["detail"]["via"] == "prompt"),
        "{log}"
    );
}

#[test]
fn worker_pane_exit_ends_the_event_watch_and_tells_the_user() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing files\n› ");
    bridge.ok(&["supervise", &goal]);
    let root = bridge.root.path().to_path_buf();
    let _server = fake_socket(&bridge, move |stream| {
        std::thread::sleep(std::time::Duration::from_millis(700));
        // Only the manager remains.
        let manager = json!([{"pane_id":"w1:p2","tab_id":"w1:t2","terminal_id":"manager","agent":"codex","agent_status":"idle","agent_session":{"value":"manager"}}]);
        std::fs::write(
            root.join("panes.json"),
            json!({"result":{"panes":manager}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join("agents.json"),
            json!({"result":{"agents":manager}}).to_string(),
        )
        .unwrap();
        stream
            .write_all(b"{\"event\":\"pane_exited\",\"data\":{\"pane_id\":\"w1:p1\",\"type\":\"pane_exited\"}}\n")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(4));
    });
    let output = bridge.call(&["watch-loop", "--run-for-secs", "3"]);
    assert!(!output.status.success(), "the watch ends with the error");
    let watch = bridge.ok(&["watch"]);
    assert_eq!(watch["active"], false);
    assert!(
        watch["status"]
            .as_str()
            .is_some_and(|s| s.starts_with("target_exited")),
        "{watch}"
    );
    let ended = bridge
        .ok(&["log", &goal])
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "watch_ended")
        .cloned()
        .unwrap();
    assert_eq!(ended["detail"]["notify"], true, "{ended}");
}

#[test]
fn a_pause_ends_a_quiet_event_watch_within_seconds_and_clears_the_sidebar() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing files\n› ");
    bridge.ok(&["supervise", &goal]);
    // Herdr confirms the subscription and then stays quiet.
    let _server = fake_socket(&bridge, |_stream| {
        std::thread::sleep(std::time::Duration::from_secs(8));
    });
    let child = bridge
        .command(&["watch-loop", "--run-for-secs", "8"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let paused = std::time::Instant::now();
    bridge.ok(&["pause", &goal]);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    // No event and no 30 s reconcile were needed to notice the end.
    assert!(
        paused.elapsed() < std::time::Duration::from_secs(4),
        "{:?}",
        paused.elapsed()
    );
    let metadata = std::fs::read_to_string(bridge.root.path().join("metadata.txt")).unwrap();
    assert!(metadata.contains("--clear-title"), "{metadata}");
}

#[test]
fn a_pane_gone_behind_a_status_event_still_ends_as_target_exited() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing files\n› ");
    bridge.ok(&["supervise", &goal]);
    let root = bridge.root.path().to_path_buf();
    let _server = fake_socket(&bridge, move |stream| {
        std::thread::sleep(std::time::Duration::from_millis(700));
        // The pane closed; Herdr's status event arrives before its exit event.
        let manager = json!([{"pane_id":"w1:p2","tab_id":"w1:t2","terminal_id":"manager","agent":"codex","agent_status":"idle","agent_session":{"value":"manager"}}]);
        for (name, list) in [("panes.json", "panes"), ("agents.json", "agents")] {
            std::fs::write(
                root.join(name),
                json!({"result":{list:manager}}).to_string(),
            )
            .unwrap();
        }
        stream.write_all(&status_event("unknown")).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(4));
    });
    assert!(
        !bridge
            .call(&["watch-loop", "--run-for-secs", "3"])
            .status
            .success()
    );
    let watch = bridge.ok(&["watch"]);
    assert!(
        watch["status"]
            .as_str()
            .is_some_and(|s| s.starts_with("target_exited")),
        "{watch}"
    );
}

#[test]
fn watch_tab_and_sidebar_show_the_goal_and_the_end_clears_the_sidebar() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    // The pause below ends the watch at its next poll; the run limit only
    // bounds a watch that never ends.
    let child = bridge
        .command(&["watch-loop", "--polling", "--run-for-secs", "30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // While watching: the worker's sidebar carries the goal, state and budget.
    let shown = eventually("the sidebar report", || {
        let shown = metadata(&bridge);
        (sidebar_reports(&shown) > 0).then_some(shown)
    });
    assert!(
        shown.starts_with("pane\nreport-metadata\nw1:p1\n--source\nhgm\n"),
        "{shown}"
    );
    assert!(shown.contains("--token") && !shown.contains("--clear-title"));
    ok_when_free(&bridge, &["pause", &goal]);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let tab = String::from_utf8_lossy(&output.stdout);
    assert!(tab.contains(&goal) && tab.contains("w1:p1"), "{tab}");
    assert!(tab.contains("polling"), "{tab}");
    // Before the first assignment the whole budget is shown.
    assert!(tab.contains("assignments 0/"), "{tab}");
    // After the end: why it ended and how to rearm, and a cleared sidebar.
    assert!(
        tab.contains("stopped_by_goal") && tab.contains(&format!("hgm supervise {goal}")),
        "{tab}"
    );
    assert!(metadata(&bridge).contains("--clear-title"));
}

#[test]
fn a_reopened_manager_keeps_the_tab_watch_roles_and_pending_transfer() {
    let mut bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    start_role(&bridge, "impl", "w1:p5", "claude");
    bridge.write(
        "process.json",
        &json!({"result":{"process_info":{"foreground_processes":[{"argv":["codex"]}]}}})
            .to_string(),
    );
    let requested = bridge.ok(&["handoff"]);
    let transfer = requested["id"].as_str().unwrap().to_string();
    // State lives under the pinned terminal, not the manager context.
    let manager_dir = std::fs::read_dir(bridge.state.join("workspaces"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("manager");
    for suffix in ["-watch.json", "-roles.json", "-handoff.json"] {
        assert!(
            manager_dir.join(format!("t-original{suffix}")).exists(),
            "{suffix}"
        );
    }

    // The user reopens Goal Manager from the same tab (a new manager context).
    let output = bridge.call(&["manager", "--agent", "claude"]);
    assert!(output.status.success());
    let reopened = std::fs::read_to_string(bridge.root.path().join("context")).unwrap();
    assert_ne!(reopened, bridge.context);
    bridge.context = reopened;
    let watch = bridge.ok(&["watch"]);
    assert_eq!(watch["active"], true);
    assert_eq!(watch["goal"], goal.as_str());
    assert_eq!(
        watch["manager"]["agent"], "claude",
        "wakes go to the new manager"
    );
    assert!(kinds(&bridge, &goal).iter().any(|k| k == "watch_adopted"));
    let roles = bridge.ok(&["role", "list"]);
    assert!(
        roles
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["role"] == "impl")
    );
    assert_eq!(bridge.ok(&["handoff-status"])["id"], transfer.as_str());
}

fn manager_dir(bridge: &Bridge) -> PathBuf {
    std::fs::read_dir(bridge.state.join("workspaces"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("manager")
}

/// Holds the tab's watch lease like a running watch process.
fn hold_watch_lease(bridge: &Bridge) -> std::fs::File {
    let lease = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(manager_dir(bridge).join("t-original-watch.lock"))
        .unwrap();
    lease.try_lock().unwrap();
    lease
}

/// Moves the tab's watch record `secs` into the past, past the start window.
#[test]
fn a_handoff_continues_after_its_watch_ended_and_reopens_a_watch_when_none_runs() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    let monitors = || {
        std::fs::read_to_string(bridge.root.path().join("opened.txt"))
            .unwrap_or_default()
            .matches("\nmonitor\n")
            .count()
    };
    let before = monitors();
    // The goal is paused: its watch ends, and no watch process runs.
    bridge.ok(&["pause", &goal]);
    age_watch(&bridge, 60);
    assert_eq!(bridge.ok(&["watch"])["active"], false);
    bridge.write(
        "process.json",
        &json!({"result":{"process_info":{"foreground_processes":[{"argv":["codex"]}]}}})
            .to_string(),
    );
    let transfer = bridge.ok(&["handoff"]);
    assert_eq!(monitors(), before + 1, "the request opens a watch");
    std::fs::write(
        transfer["document"].as_str().unwrap(),
        "Goal: g. Next: check it.",
    )
    .unwrap();
    std::fs::write(
        transfer["ready"].as_str().unwrap(),
        json!({"transfer":transfer["id"],"session":"worker","safe_to_restart":true}).to_string(),
    )
    .unwrap();
    // The ended watch still advances the transfer.
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert_eq!(bridge.ok(&["handoff-status"])["phase"], "awaiting_exit");
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(sent.lines().any(|line| line == "/exit"), "{sent}");
}

#[test]
fn a_network_error_wakes_the_manager_to_continue_and_repeated_stops_reach_the_person() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    let observe = |screen: &str| {
        cool_watch(&bridge);
        bridge.write("target.screen", screen);
        bridge.inventory("done", "idle", "original");
        assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    };
    let sent = || std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap_or_default();
    // After a sleep the turn ended on a network error, not on finished work.
    observe("Writing summary.json\n⎿  API Error: getaddrinfo ENOTFOUND api.anthropic.com\n› ");
    let wake = sent();
    assert!(wake.contains("w1:p2\nGoal Manager observation"), "{wake}");
    assert!(
        wake.contains("provider error") && wake.contains("send a short continue"),
        "{wake}"
    );
    assert!(wake.contains("send the next step now"), "{wake}");
    // main keeps stopping and the manager sends no next step: the person hears.
    observe("Waiting for instructions\n› ");
    observe("Still waiting\n› ");
    let attentions: Vec<Value> = bridge
        .ok(&["log", &goal])
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "attention")
        .cloned()
        .collect();
    assert_eq!(attentions.len(), 1, "{attentions:?}");
    assert_eq!(attentions[0]["detail"]["cause"], "no_next_step");
    let notified = std::fs::read_to_string(bridge.root.path().join("notified.txt")).unwrap();
    assert!(notified.contains("Original agent needs you"), "{notified}");
}

#[test]
fn a_spent_wake_budget_leaves_the_goal_waiting_for_the_person() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal, "--max-wakes", "1"]);
    bridge.inventory("done", "idle", "original");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    bridge.write("target.screen", "Changed\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let watch = bridge.ok(&["watch"]);
    assert_eq!(watch["active"], false, "{watch}");
    let waiting = bridge
        .ok(&["log", &goal])
        .as_array()
        .unwrap()
        .iter()
        .any(|e| {
            e["kind"] == "waiting"
                && e["detail"]["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("wake_budget_exhausted"))
        });
    assert!(waiting, "the goal waits for the person");
}

/// Moves the tab's last wake out of the cooldown, so the next observation
/// may wake the manager again.
fn cool_watch(bridge: &Bridge) {
    let path = manager_dir(bridge).join("t-original-watch.json");
    let mut watch: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    watch["last_sent"] = json!(0);
    std::fs::write(path, watch.to_string()).unwrap();
}

fn age_watch(bridge: &Bridge, secs: i64) {
    let path = manager_dir(bridge).join("t-original-watch.json");
    let mut watch: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    watch["last_change"] = json!(watch["last_change"].as_i64().unwrap() - secs);
    std::fs::write(path, watch.to_string()).unwrap();
}

#[test]
fn an_unwatched_goal_stops_the_manager_once_and_supervise_reuses_a_starting_watch() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    let opened = || {
        std::fs::read_to_string(bridge.root.path().join("opened.txt"))
            .unwrap_or_default()
            .matches("monitor")
            .count()
    };
    assert_eq!(opened(), 1);
    // Re-arming while the watch process may still be starting opens no second tab.
    bridge.ok(&["supervise", &goal]);
    assert_eq!(opened(), 1);
    // Starting: the manager may stop.
    assert_eq!(bridge.hook(json!({"stop_hook_active":false})), json!({}));

    // The watch process is gone (closed tab, old build) but the record is active.
    age_watch(&bridge, 60);
    let blocked = bridge.hook(json!({"stop_hook_active":false}));
    assert_eq!(blocked["decision"], "block");
    assert!(
        blocked["reason"]
            .as_str()
            .unwrap()
            .contains(&format!("hgm supervise {goal}")),
        "{blocked}"
    );
    // Only once per stop.
    assert_eq!(bridge.hook(json!({"stop_hook_active":true})), json!({}));
    // Rearming opens a new watch tab.
    bridge.ok(&["supervise", &goal]);
    assert_eq!(opened(), 2);
}

#[test]
fn a_new_watch_closes_the_ended_goal_watch_tab_so_one_remains() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    let read =
        |name: &str| std::fs::read_to_string(bridge.root.path().join(name)).unwrap_or_default();
    for (tab, label, closed) in [("w1:t9", "Goal watch", true), ("w1:t8", "api", false)] {
        bridge.ok(&["supervise", &goal]);
        // The watch runs in its own tab, then a wait ends it; its tab stays on
        // the end view.
        let output = bridge
            .command(&["watch-loop", "--polling", "--run-for-secs", "1"])
            .env("HERDR_TAB_ID", tab)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        bridge.ok(&["wait", &goal, "--reason", "Ask the user"]);
        bridge.write(
            "tab.json",
            &json!({"result":{"tab":{"label":label}}}).to_string(),
        );
        let opened = read("opened.txt").matches("monitor").count();
        bridge.ok(&["supervise", &goal]);
        assert_eq!(read("opened.txt").matches("monitor").count(), opened + 1);
        // Only a tab that still shows the Goal watch is closed.
        assert_eq!(
            read("closed.txt").lines().any(|t| t == tab),
            closed,
            "{tab}"
        );
        bridge.ok(&["wait", &goal, "--reason", "Next round"]);
    }
}

#[test]
fn a_redeployed_build_takes_over_the_running_watch() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing files\n› ");
    bridge.ok(&["supervise", &goal]);
    // The watch runs from a deployed copy, replaced like a deploy: temp + rename.
    let deployed = bridge.root.path().join("deployed");
    std::fs::create_dir(&deployed).unwrap();
    let program = deployed.join("hgm");
    std::fs::copy(env!("CARGO_BIN_EXE_hgm"), &program).unwrap();
    let (subscribed, subscription) = std::sync::mpsc::channel();
    let (redeployed, redeploy) = std::sync::mpsc::channel::<()>();
    let _server = fake_socket(&bridge, move |stream| {
        let _ = subscribed.send(());
        if redeploy.recv_timeout(WAIT).is_ok() {
            // Any state change wakes the loop, which then notices the new build.
            stream.write_all(&status_event("working")).unwrap();
        }
        // The stream stays open until the test ends.
        let _ = redeploy.recv_timeout(WAIT);
    });
    // The pause below ends the watch; the run limit only bounds a watch that
    // never ends.
    let child = bridge
        .command_with(&program, &["watch-loop", "--run-for-secs", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A subscribed watch has already noted the build it runs from.
    subscription
        .recv_timeout(WAIT)
        .expect("the watch subscribes to Herdr events");
    let staged = deployed.join("hgm.new");
    std::fs::copy(env!("CARGO_BIN_EXE_hgm"), &staged).unwrap();
    std::fs::rename(&staged, &program).unwrap();
    redeployed.send(()).unwrap();
    eventually("the watch to reload", || {
        kinds(&bridge, &goal)
            .iter()
            .any(|kind| kind == "watch_reloaded")
            .then_some(())
    });
    // The new build shows the watch in the worker's sidebar again.
    eventually("the new build to report the sidebar", || {
        (sidebar_reports(&metadata(&bridge)) > 1).then_some(())
    });
    assert_eq!(
        bridge.ok(&["watch"])["active"],
        true,
        "the watch did not end"
    );
    ok_when_free(&bridge, &["pause", &goal]);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reloads: Vec<_> = bridge
        .ok(&["log", &goal])
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "watch_reloaded")
        .cloned()
        .collect();
    // Each replacement reloads once: the new build does not reload itself.
    assert_eq!(reloads.len(), 1, "{reloads:?}");
}

/// The user restarts the original agent in its tab: same pane, new session.
fn restart_worker_session(bridge: &Bridge, session: &str) {
    for name in ["panes.json", "agents.json"] {
        let path = bridge.root.path().join(name);
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let list = if name == "panes.json" {
            "panes"
        } else {
            "agents"
        };
        value["result"][list][0]["agent_session"]["value"] = json!(session);
        std::fs::write(path, value.to_string()).unwrap();
    }
}

#[test]
fn a_session_the_user_restarted_is_adopted_only_on_request() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    restart_worker_session(&bridge, "restarted-by-user");
    let refused = bridge.call(&["target"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("target --adopt"));
    let refused = bridge.call(&["supervise", &goal]);
    assert!(String::from_utf8_lossy(&refused.stderr).contains("target --adopt"));
    assert!(
        !bridge
            .call(&["send", "not to an unbound session"])
            .status
            .success()
    );
    assert!(!bridge.call(&["target", "--adopt", " "]).status.success());

    let adopted = bridge.ok(&["target", "--adopt", "User: continue the work in main"]);
    assert_eq!(adopted["adopted"]["goal"], goal.as_str());
    assert_eq!(adopted["binding"]["session"]["value"], "restarted-by-user");
    assert_eq!(
        bridge.ok(&["watch"])["origin"]["session"]["value"],
        "restarted-by-user"
    );
    let restarted = bridge
        .ok(&["log", &goal])
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "restarted")
        .cloned()
        .expect("restart recorded, so a goal check is due");
    assert_eq!(restarted["detail"]["via"], "adopt");
    assert_eq!(restarted["detail"]["previous"]["value"], "worker");
    assert_eq!(
        restarted["detail"]["evidence"],
        "User: continue the work in main"
    );
    // Adopting the bound session again changes nothing.
    assert!(
        bridge
            .ok(&["target", "--adopt", "again"])
            .get("adopted")
            .is_none()
    );
    bridge.ok(&["send", "Continue with the current step"]);

    // A session in another tab is never adopted.
    bridge.inventory("idle", "idle", "replacement");
    assert!(
        !bridge
            .call(&["target", "--adopt", "wrong tab"])
            .status
            .success()
    );
}

#[test]
fn adoption_waits_for_a_handoff_restart_in_progress() {
    let bridge = Bridge::new();
    bridge.write(
        "process.json",
        &json!({"result":{"process_info":{"foreground_processes":[{"argv":["codex"]}]}}})
            .to_string(),
    );
    bridge.ok(&["handoff"]);
    restart_worker_session(&bridge, "restarted-by-user");
    let refused = bridge.call(&["target", "--adopt", "User restarted it"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("hgm restart"));
}

#[test]
fn watch_state_saved_under_a_manager_context_moves_to_the_terminal() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    let manager_dir = std::fs::read_dir(bridge.state.join("workspaces"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("manager");
    // An earlier build kept the watch under the manager context.
    let legacy = manager_dir.join(format!("{}-watch.json", bridge.context));
    std::fs::rename(manager_dir.join("t-original-watch.json"), &legacy).unwrap();
    assert_eq!(bridge.ok(&["watch"])["active"], true);
    assert!(!legacy.exists());
    assert!(manager_dir.join("t-original-watch.json").exists());
}

/// A patrol and a settle wake for a contract and assignment the size of the
/// ones managers wrote on 2026-11-14; returns the two wake texts.
fn sample_wakes() -> (String, String) {
    let bridge = Bridge::new();
    let item = |n: usize| {
        format!(
            "Criterion {n}: hgm keeps the recorded evidence, names the observed cause and the next step for the manager, and a test covers the case described here"
        )
    };
    let goal = add_file(
        &bridge,
        json!({
            "objective":"Adopt the manager ideas that Herdr supports: an isolated checkout per goal, one watch per pane serving several goals, compact wakes, settle checks that only flag real regressions, and status without stale waits.",
            "acceptance":(1..=6).map(item).collect::<Vec<_>>(),
            "invariants":["Existing commands, flags, JSON fields and goal state transitions keep their meaning; new fields and options are additive.","No PR or merge automation, daemon process, Slack integration, formal specification or automatic task decomposition.","The main checkout stays unchanged until the publish; no version bump, tag, release or plugin reinstall."],
            "out_of_scope":["Merging the TODO branches into main","Codex native checks"],
            "checks":[{"name":"workspace-checks","command":"true"}],
            "loop":{"patrol_minutes":1,"stop_when":["a fix needs a Herdr change"]}
        }),
    );
    bridge.ok(&["confirm", &goal, "--evidence", "User: go"]);
    let assignment = "Implement the shared watch: add goals to Watch with serde default so old state files read as one goal, let a second goal on the same pane join the running watch, pick the goal and task of the latest linked assignment to that pane on every wake, keep the wake budget per goal, and remove only the paused, cancelled or waiting goal from the watch, ending it when none is left. Run settle checks only for the chosen goal. Add tests for two goals on one pane and for one of them waiting. Then run fmt, clippy -D warnings, the tests and a release build, commit with a conventional message, and report briefly.";
    bridge.ok(&["send", "--goal", &goal, "--task", "work", assignment]);
    let screen: String = (1..=30)
        .map(|n| {
            format!("⏺ Step {n}: edited src/watch.rs and ran cargo test --locked; 116 passed\n")
        })
        .collect();
    let sent = bridge.root.path().join("sent.txt");
    let wake = |bridge: &Bridge| {
        let before = std::fs::read_to_string(&sent).unwrap().len();
        assert!(bridge.call(&["watch-loop", "--once"]).status.success());
        let text = std::fs::read_to_string(&sent).unwrap()[before..].to_string();
        text.strip_prefix("w1:p2\n")
            .unwrap_or(&text)
            .trim_end()
            .to_string()
    };
    bridge.write("target.screen", &format!("{screen}› "));
    bridge.inventory("working", "idle", "original");
    let watch_path = state_directory(&bridge).join("manager/t-original-watch.json");
    let mut watch: Value = serde_json::from_slice(&std::fs::read(&watch_path).unwrap()).unwrap();
    watch["last_patrol"] = json!(0);
    std::fs::write(&watch_path, watch.to_string()).unwrap();
    let patrol = wake(&bridge);
    bridge.write("target.screen", &format!("{screen}⏺ Done: committed.\n› "));
    bridge.inventory("done", "idle", "original");
    let mut watch: Value = serde_json::from_slice(&std::fs::read(&watch_path).unwrap()).unwrap();
    watch["last_sent"] = json!(0);
    std::fs::write(&watch_path, watch.to_string()).unwrap();
    let settle = wake(&bridge);
    (patrol, settle)
}
/// Wakes carry what changed: the assignment in full only on its first wake,
/// and new screen lines only. docs/verification.md records the sizes
/// (`cargo test --test bridge wakes_carry -- --nocapture` prints them).
#[test]
fn wakes_carry_the_full_assignment_once_and_only_new_lines() {
    let (patrol, settle) = sample_wakes();
    println!(
        "patrol wake: {} bytes, settle wake: {} bytes",
        patrol.len(),
        settle.len()
    );
    let full = "Implement the shared watch: add goals to Watch with serde default";
    assert!(
        patrol.contains("New assignment (full text):") && patrol.contains("and report briefly."),
        "{patrol}"
    );
    assert!(
        settle.contains(&format!("Assignment: {full}"))
            && settle.contains("(full text: hgm log")
            && !settle.contains("and report briefly."),
        "{settle}"
    );
    // The settle wake quotes only the line added since the patrol.
    assert!(
        settle.contains("⏺ Done: committed.") && !settle.contains("Step 30:"),
        "{settle}"
    );
    for wake in [&patrol, &settle] {
        assert!(wake.contains("Contract: hgm show g-") && !wake.contains("Criterion 1"));
    }
    assert!(settle.contains("every command check passes"), "{settle}");
    assert!(
        patrol.len() < 2_500 && settle.len() < 1_600,
        "patrol {} / settle {} bytes",
        patrol.len(),
        settle.len()
    );
}
