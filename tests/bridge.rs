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
        add_file(
            self,
            json!({"objective":"Useful result","acceptance":["Recipient can proceed"],
            "checks":[{"name":"check-1","command":"true"}]}),
        )
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
    assert!(sent.contains("Contract: hgm show g-"), "{sent}");
    assert!(!sent.contains("idle/done is not acceptance"));
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
    assert_eq!(bridge.ok(&["restart"])["phase"], "awaiting_successor");
    assert!(!sent().contains("fresh-session handoff"));
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
    assert_eq!(bridge.ok(&["restart"])["phase"], "awaiting_acknowledgement");
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
            "loop":{"max_assignments":1,"autonomy":"ask","stop_checks":[
                {"name":"gate","command":"echo checked >> checks-run; exit 1"}
            ]}
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
    let checks = bridge.workspace.join("checks-run");
    assert!(!checks.exists(), "refused sends ran a stop check");
    assert!(send(&["--user-approval", "User: send it"]).status.success());

    let spent = send(&["--user-approval", "User: try again"]);
    assert!(!spent.status.success());
    assert!(String::from_utf8_lossy(&spent.stderr).contains("budget exhausted"));
    assert_eq!(std::fs::read_to_string(checks).unwrap(), "checked\n");
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
fn a_patrol_that_finds_progress_and_nothing_else_is_recorded_without_a_wake() {
    let bridge = Bridge::new();
    let spec = json!({"objective":"Long report","acceptance":["Report covers Q3"],
        "checks":[{"name":"report","command":"true"}],"loop":{"patrol_minutes":1}});
    let goal = add_file(&bridge, spec.clone());
    bridge.ok(&["confirm", &goal, "--evidence", "User: go"]);
    bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "Draft the Q3 report",
    ]);
    bridge.write("target.screen", "Writing section 1\n› ");
    bridge.inventory("working", "idle", "original");
    let observations = || {
        sent_text(&bridge)
            .matches("Goal Manager observation")
            .count()
    };
    edit_watch(&bridge, |w| w["last_patrol"] = json!(0));
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let patrols: Vec<_> = bridge
        .ok(&["log", &goal])
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "patrol")
        .cloned()
        .collect();
    assert_eq!(patrols.len(), 1);
    assert_eq!(patrols[0]["detail"]["quiet"], true, "{patrols:?}");
    assert_eq!(observations(), 0, "{}", sent_text(&bridge));

    let now = chrono::Utc::now().timestamp();
    edit_watch(&bridge, |w| {
        w["last_patrol"] = json!(now - 70);
        w["last_change"] = json!(now - 100);
        w["last_sent"] = json!(0);
    });
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert_eq!(observations(), 1);
    assert!(sent_text(&bridge).contains("1-minute check of main"));
    assert!(wakes(&bridge, &goal).len() == 1);

    std::fs::create_dir_all(bridge.workspace.join(".hgm")).unwrap();
    std::fs::write(
        bridge.workspace.join(".hgm/config.yaml"),
        "loop:\n  patrol_wake: always\n",
    )
    .unwrap();
    let always = add_file(&bridge, spec);
    assert_eq!(
        bridge.ok(&["show", &always])["spec"]["loop"]["patrol_wake"],
        "always"
    );
    assert!(
        bridge.ok(&["show", &goal])["spec"]["loop"]
            .get("patrol_wake")
            .is_none()
    );
}

#[test]
fn watcher_patrols_a_working_agent_and_escalates_a_spent_time_budget() {
    let bridge = Bridge::new();
    let goal = add_file(
        &bridge,
        json!({
            "objective":"Long report","acceptance":["Report covers Q3"],"checks":[{"name":"report","command":"true"}],
            "invariants":["Keep the Q2 numbers"],
            "loop":{"max_hours":1,"patrol_minutes":1,"patrol_wake":"always","stop_when":["Q3 data is unavailable"]}
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
    assert!(sent.contains("1-minute check of main"), "{sent}");
    let wake = &sent[sent.rfind("Goal Manager observation").unwrap()..];
    assert!(wake.contains("New assignment (full text):\nDraft the Q3 report"));
    assert!(wake.contains(&format!("Contract: hgm show {goal}")));
    assert!(!wake.contains("Keep the Q2 numbers"), "{wake}");
    assert!(kinds(&bridge, &goal).contains(&"patrol".to_string()));

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
        "`hgm send [<TEXT>] [--goal <GOAL>] [--task <TASK>] [--no-supervise] [--user-approval <USER_APPROVAL>] [--plan] [--interrupt] [--to <TO>] [--prepare] [--withdraw] [--step <STEP>]`",
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
    let bridge = Bridge::new();
    let read =
        |name: &str| std::fs::read_to_string(bridge.root.path().join(name)).unwrap_or_default();
    let panels = || read("opened.txt").matches("\npanel\n").count();
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
    let refused = open_from(&bridge, "w1:p1", "Goal watch");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("Goal watch tab"));
    start_role(&bridge, "impl", "w1:p5", "claude");
    let refused = open_from(&bridge, "w1:p5", "impl");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("impl helper"));
    assert_eq!(panels(), 0);

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

    close_manager(&bridge);
    assert!(open_from(&bridge, "w1:p1", "main").status.success());
    assert_eq!(panels(), 1);
}

#[test]
fn a_manager_still_starting_keeps_its_tab_and_a_refused_launch_leaves_no_context() {
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
    assert!(open_from(&bridge, "w1:p1", "main").status.success());
    assert_eq!(read("opened.txt").matches("\npanel\n").count(), 0);

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
    assert!(
        sent.starts_with("w1:p1\nContinue the current assignment"),
        "{sent}"
    );
    let kinds_now = kinds(&bridge, &goal);
    assert!(kinds_now.iter().any(|k| k == "provider_error"));
    assert!(kinds_now.iter().any(|k| k == "provider_continue"));

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
    bridge.ok(&["note", &goal, "Still waiting on this limit"]);
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
        "which belong in evidence (--evidence, --user-approval, note)",
        "send --interrupt, correct or reanchor",
        "Run hgm reanchor GOAL:",
        "correct GOAL --task TASK TEXT says what is off",
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
    assert!(
        sent.contains("report ") && sent.contains("\"Goal restatement: ...\""),
        "{sent}"
    );
    assert!(kinds(&bridge, &goal).iter().any(|k| k == "reanchor"));
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

    git(&repo, &["branch", "taken"]);
    let taken = add_file(&bridge, spec("taken", &root.join("unused")));
    refused_with(&bridge, &["run", &taken], "confirm the goal");
    assert_eq!(bridge.ok(&["show", &taken])["status"], "ready");
    let refused = bridge.call(&["confirm", &taken, "--evidence", "User: go"]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success() && stderr.contains("already exists"),
        "{stderr}"
    );
    assert!(bridge.ok(&["show", &taken])["confirmed"].is_null());
    assert!(calls().is_empty());

    let goal = add_file(&bridge, spec("todo/copy", &checkout));
    let gate = root.join("hold-worktree");
    std::fs::write(&gate, "").unwrap();
    let confirming = bridge
        .command(&["confirm", &goal, "--evidence", "User: go"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while calls().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "confirmation never reached checkout creation"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let replacing = bridge
        .command(&[
            "add",
            "--replace",
            &goal,
            "Different contract",
            "--criterion",
            "Different result",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    std::fs::remove_file(gate).unwrap();
    let confirmed = confirming.wait_with_output().unwrap();
    assert!(confirmed.status.success());
    assert!(!replacing.wait_with_output().unwrap().status.success());
    let shown: Value = serde_json::from_slice(&confirmed.stdout).unwrap();
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
    let again = add_file(&bridge, spec("todo/copy", &root.join("elsewhere")));
    let reopened = bridge.ok(&["confirm", &again, "--evidence", "User: go"]);
    assert_eq!(
        std::fs::canonicalize(reopened["checkout"]["path"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&checkout).unwrap()
    );
    assert_eq!(reopened["checkout"]["workspace_id"], "w8");

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
    let mut second_spec = spec("Second outcome");
    second_spec["loop"] = json!({"max_assignments": 7});
    let second = confirmed(&bridge, second_spec);
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
    let wake = settle("Second assignment done\n› ");
    assert!(
        wake.contains(&second) && wake.contains("Second assignment") && !wake.contains(&first),
        "{wake}"
    );
    assert!(wake.contains("Assignments for work: 1 of 7."), "{wake}");
    assert!(
        wake.contains(&format!("observation for {second}, task work")),
        "{wake}"
    );
    assert!(kinds(&bridge, &second).iter().any(|k| k == "settle_checks"));
    assert!(!kinds(&bridge, &first).iter().any(|k| k == "settle_checks"));

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

    let third = bridge.goal();
    let fourth = bridge.goal();
    bridge.ok(&["supervise", &third]);
    let joined = bridge.ok(&["supervise", &fourth]);
    assert_eq!(joined["goals"], json!([third, fourth]), "{joined}");
    age_watch(&bridge, 60);
    let watch = bridge.ok(&["supervise", &fourth]);
    assert_eq!(watch["goal"], fourth.as_str(), "{watch}");
    assert!(kinds(&bridge, &third).iter().any(|k| k == "watch_ended"));
}

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
    let bridge = Bridge::new();
    let codex = start_role(&bridge, "impl", "w1:p5", "codex");
    assert_eq!(codex["agent"], "codex");
    assert_eq!(
        start_role(&bridge, "reviewer", "w1:p6", "copilot")["agent"],
        "copilot"
    );
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
    assert_eq!(
        bridge.ok(&["role", "list"]).as_array().map_or(0, Vec::len),
        1
    );
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
    bridge.write("manager.screen", "› ");
    let output = std::thread::scope(|scope| {
        let first = scope.spawn(|| submit("reviewer", "revise", blocking));
        let second = submit("reviewer", "revise", blocking);
        let first = first.join().unwrap();
        assert_ne!(
            first.status.success(),
            second.status.success(),
            "exactly one submission must succeed"
        );
        if first.status.success() {
            first
        } else {
            second
        }
    });
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

#[test]
fn require_review_holds_run_until_another_provider_approves_the_current_state() {
    let bridge = Bridge::new();
    let goal = add_file(
        &bridge,
        json!({"objective":"o","acceptance":["a"],"checks":[{"name":"c","command":"true"}],
               "tasks":[{"id":"work","instruction":"w"}],
               "loop":{"check_on_settle":false,"require_review":true}}),
    );
    bridge.ok(&["confirm", &goal, "--evidence", "User: go"]);
    std::fs::write(bridge.workspace.join("lib.rs"), "fn one() {}").unwrap();
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Build it"]);
    bridge.ok(&["complete", &goal, "work", "--evidence", "Inspected lib.rs"]);
    let held = |command: &str| {
        let output = bridge.call(&[command, &goal]);
        assert!(!output.status.success(), "{command} moved it to review");
        let shown = bridge.ok(&["show", &goal]);
        assert_eq!(shown["status"], "needs_input");
        shown["message"].as_str().unwrap().to_string()
    };
    let message = held("run");
    assert!(
        message.contains("no reviewer has approved this goal")
            && message.contains("other than codex")
            && message.contains(&format!("hgm review {goal} TEXT"))
            && message.contains(&format!("hgm resume {goal}")),
        "{message}"
    );
    assert!(
        bridge.ok(&["show", &goal])["verifications"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let review_by = |reviewer: &str, verdict: &str| {
        let prompt = if reviewer == "reviewer" {
            "› "
        } else {
            "Ready\n❯ "
        };
        bridge.write("manager.screen", prompt);
        let review =
            bridge.ok(&["review", &goal, "--reviewer", reviewer, "Check lib.rs"])["review"]
                .as_str()
                .unwrap()
                .to_string();
        let args = [
            "review-submit",
            &goal,
            "--review",
            &review,
            "--verdict",
            verdict,
        ];
        let output = as_role_with_input(&bridge, reviewer, &args, "[]");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        review
    };
    start_role(&bridge, "reviewer", "w1:p6", "codex");
    let same = review_by("reviewer", "approve");
    assert!(held("resume").contains(&format!("approving review {same} came from codex")));

    let mut panes: Vec<Value> = serde_json::from_str::<Value>(
        &std::fs::read_to_string(bridge.root.path().join("panes.json")).unwrap(),
    )
    .unwrap()["result"]["panes"]
        .as_array()
        .unwrap()
        .clone();
    panes.push(json!({"pane_id":"w1:p7","tab_id":"w1:t7","terminal_id":"second-term","agent":"claude","agent_status":"idle","agent_session":{"value":"second"}}));
    bridge.write(
        "role-panes.json",
        &json!({"result":{"panes":panes,"agents":panes}}).to_string(),
    );
    bridge.write(
        "tab-create.json",
        &json!({"result":{"root_pane":{"pane_id":"w1:p7","tab_id":"w1:t7"},"tab":{"tab_id":"w1:t7"}}})
            .to_string(),
    );
    bridge.write("manager.screen", "Ready\n❯ ");
    bridge.ok(&[
        "role", "start", "reviewer", "--name", "second", "--agent", "claude",
    ]);
    let other = review_by("second", "approve");

    std::fs::write(bridge.workspace.join("lib.rs"), "fn one() { two() }").unwrap();
    let packet = bridge.ok(&["packet", &goal]);
    let fresh: Vec<&str> = packet["reviews"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["fresh"].as_str().unwrap())
        .collect();
    assert_eq!(fresh, ["stale", "stale"], "{packet}");
    assert!(
        packet["not_verified"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["what"]
                == format!("review {other} (approve) judged an earlier state of the work dir")),
        "{packet}"
    );
    assert_eq!(bridge.ok(&["show", &goal])["reviews"][1]["fresh"], "stale");
    let status = bridge.ok(&["status"]);
    let row = status
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == goal.as_str())
        .unwrap();
    assert_eq!(row["review"]["review"], other.as_str());
    assert_eq!(row["review"]["fresh"], "stale");
    assert!(held("resume").contains(&format!("approving review {other} is stale")));

    let fresh_review = review_by("second", "approve");
    let ready = bridge.ok(&["resume", &goal]);
    assert_eq!(ready["status"], "ready_for_review", "{ready}");
    let shown = bridge.ok(&["show", &goal]);
    assert_eq!(shown["reviews"][2]["review"], fresh_review.as_str());
    assert_eq!(shown["reviews"][2]["fresh"], "current");
    assert!(shown["acceptance"].is_null());
}

#[test]
fn writes_outside_a_tasks_write_paths_are_recorded_and_wake_the_manager_without_blocking() {
    let bridge = Bridge::new();
    std::fs::create_dir(bridge.workspace.join("src")).unwrap();
    std::fs::write(bridge.workspace.join("src/lib.rs"), "fn a() {}").unwrap();
    std::fs::write(bridge.workspace.join("README.md"), "readme").unwrap();
    let goal = confirmed_goal(
        &bridge,
        "true",
        json!({"tasks":[{"id":"work","instruction":"w","write_paths":["src"]},
                         {"id":"free","instruction":"f"}],
               "loop":{"check_on_settle":false}}),
    );
    bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "Change the parser",
    ]);
    let sent = sent_text(&bridge);
    assert!(
        sent.contains(
            "Write only in: src. hgm records changes elsewhere during this assignment and tells the Goal Manager; it does not block them.\nChange the parser"
        ),
        "{sent}"
    );
    std::fs::write(bridge.workspace.join("src/lib.rs"), "fn a() { b() }").unwrap();
    std::fs::write(bridge.workspace.join("README.md"), "readme, edited").unwrap();
    let before = sent.len();
    cool_watch(&bridge);
    bridge.write("target.screen", "Parser changed\n› ");
    bridge.inventory("done", "idle", "original");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let wake = sent_text(&bridge)[before..].to_string();
    assert!(
        wake.contains(
            "main changed files outside task work's write_paths (src): README.md. hgm does not block writes"
        ),
        "{wake}"
    );
    let log = bridge.ok(&["log", &goal]);
    let outside: Vec<&Value> = log
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "scope_exceeded")
        .collect();
    assert_eq!(outside.len(), 1);
    assert_eq!(outside[0]["detail"]["paths"], json!(["README.md"]));
    let packet = bridge.ok(&["packet", &goal]);
    assert_eq!(
        packet["outside_write_paths"][0]["paths"],
        json!(["README.md"])
    );
    assert_eq!(packet["outside_write_paths"][0]["task"], "work");

    bridge.ok(&["complete", &goal, "work", "--evidence", "Parser done"]);
    let before = sent_text(&bridge).len();
    bridge.inventory("idle", "idle", "original");
    bridge.ok(&["send", "--goal", &goal, "--task", "free", "Write the notes"]);
    let free = sent_text(&bridge)[before..].to_string();
    assert!(
        free.contains(&format!(
            "Goal {goal}, free, assigned to main.\nWrite the notes"
        )),
        "{free}"
    );
    std::fs::write(bridge.workspace.join("notes.md"), "n").unwrap();
    cool_watch(&bridge);
    bridge.write("target.screen", "Notes written\n› ");
    bridge.inventory("done", "idle", "original");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert_eq!(
        kinds(&bridge, &goal)
            .iter()
            .filter(|k| *k == "scope_exceeded")
            .count(),
        1
    );
}

#[test]
fn queue_next_gives_each_goal_its_own_checkout_in_a_git_repository() {
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
    git(&bridge.workspace, &["init", "-q"]);
    git(
        &bridge.workspace,
        &["commit", "-q", "--allow-empty", "-m", "start"],
    );
    let base = git(&bridge.workspace, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let config = |yaml: &str| {
        let directory = root.join(".config/herdr-goal-manager");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("config.yaml"), yaml).unwrap();
    };
    config("factory: {enabled: true, max_active: 5}\n");
    let queue = |name: &str, extra: &str| -> String {
        let file = root.join(format!("{name}.yaml"));
        std::fs::write(
            &file,
            format!("objective: {name}\nacceptance: [done]\nchecks:\n  - {{name: c, command: 'true'}}\n{extra}"),
        )
        .unwrap();
        bridge.ok(&[
            "queue",
            "add",
            "--file",
            file.to_str().unwrap(),
            "--evidence",
            "User: queue it",
        ])["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let branch_of = |item: &str| format!("hgm/{}", &item[..10]);
    let first = queue("First", "");
    let second = queue("Second", "");
    let own = root.join("own");
    let third = queue(
        "Third",
        &format!(
            "checkout: {{base: HEAD, branch: contract/own, path: '{}'}}\n",
            own.display()
        ),
    );

    git(&bridge.workspace, &["branch", &branch_of(&first)]);
    let failed = bridge.call(&["queue", "next"]);
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        !failed.status.success()
            && stderr.contains("already exists")
            && stderr.contains("marked failed"),
        "{stderr}"
    );
    let listed = bridge.ok(&["queue", "list"]);
    assert_eq!(listed[0]["state"], "failed", "{listed}");

    let next = bridge.ok(&["queue", "next"]);
    let goal = next["goal"]["id"].as_str().unwrap().to_string();
    let checkout = &next["goal"]["checkout"];
    assert_eq!(checkout["branch"], branch_of(&second).as_str(), "{next}");
    assert_eq!(next["item"]["checkout"]["branch"], checkout["branch"]);
    assert!(next.get("checkout_skipped").is_none());
    let path = std::path::PathBuf::from(checkout["path"].as_str().unwrap());
    assert_eq!(
        git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        branch_of(&second)
    );
    let calls = std::fs::read_to_string(root.join("worktree.txt")).unwrap();
    assert!(
        calls.contains(&format!("--branch {} --base {base}", branch_of(&second))),
        "{calls}"
    );
    assert_eq!(
        bridge.ok(&["packet", &goal])["checkout"]["branch"],
        branch_of(&second).as_str()
    );
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Do it"]);
    assert!(
        sent_text(&bridge).contains(&format!(
            "Work in {} (branch {})",
            path.display(),
            branch_of(&second)
        )),
        "{}",
        sent_text(&bridge)
    );

    let named = bridge.ok(&["queue", "next"]);
    assert_eq!(named["item"]["id"], third.as_str());
    assert_eq!(named["goal"]["checkout"]["branch"], "contract/own");
    assert_eq!(
        std::fs::canonicalize(named["goal"]["checkout"]["path"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&own).unwrap()
    );
    let listed = bridge.ok(&["queue", "list"]);
    let branches: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["checkout"]["branch"].as_str().unwrap_or("-"))
        .collect();
    assert_eq!(branches, ["-", branch_of(&second).as_str(), "contract/own"]);

    config("factory: {enabled: true, max_active: 5, checkout: false}\n");
    queue("Fourth", "");
    let plain = bridge.ok(&["queue", "next"]);
    assert!(plain["goal"]["checkout"].is_null(), "{plain}");
    assert!(plain["item"].get("checkout").is_none());
}

fn as_role_with_input(bridge: &Bridge, role: &str, args: &[&str], stdin: &str) -> Output {
    let mut child = bridge
        .command(args)
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
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn every_refusal_names_its_next_step() {
    let bridge = Bridge::new();
    let spec = |extra: Value| {
        let mut spec = json!({"objective":"o","acceptance":["a"],
            "checks":[{"name":"c","command":"true"}]});
        spec.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        spec
    };
    let unconfirmed = add_file(&bridge, spec(json!({})));
    let ask = confirmed(&bridge, spec(json!({"loop":{"autonomy":"ask"}})));
    let plan = confirmed(&bridge, spec(json!({"loop":{"plan_first":true}})));
    let finished = confirmed(&bridge, spec(json!({})));
    bridge.ok(&["cancel", &finished]);
    std::fs::write(
        bridge.workspace.join("empty.yaml"),
        "objective: \"\"\nacceptance: [a]\n",
    )
    .unwrap();
    std::fs::write(
        bridge.workspace.join("unchecked.yaml"),
        "objective: o\nacceptance: [a]\n",
    )
    .unwrap();
    let unchecked = bridge.ok(&["add", "--file", "unchecked.yaml"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let long = "word ".repeat(401);
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec!["send", "--goal", &unconfirmed, "--task", "work", "x"],
            "hgm confirm",
        ),
        (vec!["send", "--interrupt", "x"], "--goal <GOAL>"),
        (
            vec!["send", "--goal", &ask, "--task", "work", "x"],
            "--user-approval",
        ),
        (
            vec!["send", "--goal", &plan, "--task", "work", "x"],
            "send --plan",
        ),
        (
            vec!["send", "--goal", &plan, "--task", "work", "--plan", &long],
            "point to files",
        ),
        (
            vec!["send", "--goal", &finished, "--task", "work", "x"],
            "register a new goal",
        ),
        (
            vec!["add", "--file", "empty.yaml"],
            "hgm add --file PATH --validate",
        ),
        (
            vec!["confirm", &unchecked, "--evidence", "User: go"],
            "Register the goal again with a check",
        ),
        (
            vec!["add", "--file", "unchecked.yaml", "--replace", &ask],
            "register a new one",
        ),
    ];
    for (args, next) in cases {
        refused_with(&bridge, &args, next);
    }
}

#[test]
fn a_refused_send_is_recorded_and_attention_accounting_reaches_packet_status_and_summary() {
    let bridge = Bridge::new();
    let goal = add_file(
        &bridge,
        json!({"objective":"Count attention","acceptance":["a"],
            "checks":[{"name":"c","command":"true"}]}),
    );
    refused_with(
        &bridge,
        &["send", "--goal", &goal, "--task", "work", "Build it"],
        "hgm confirm",
    );
    assert!(!sent_text(&bridge).contains("Build it"));
    let refused = kinds(&bridge, &goal);
    assert_eq!(
        refused.iter().filter(|k| *k == "refused").count(),
        1,
        "{refused:?}"
    );
    bridge.ok(&["confirm", &goal, "--evidence", "User: go"]);
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Build it"]);
    let packet = bridge.ok(&["packet", &goal]);
    assert_eq!(packet["attention"]["refused"], 1, "{packet}");
    assert_eq!(packet["attention"]["wakes"], 0);
    let status = bridge.ok(&["status"]);
    assert_eq!(status[0]["attention"]["refused"], 1, "{status}");
    let summary = bridge.ok(&["attention"]);
    assert_eq!(summary[0]["id"], goal.as_str());
    assert_eq!(summary[0]["attention"]["corrections"], 0);
    assert_eq!(
        bridge.ok(&["show", &goal])["tasks"][0]["manual_evidence"],
        Value::Null
    );
}

#[test]
fn main_files_its_full_report_and_the_manager_reads_it_in_log_and_the_packet() {
    let bridge = Bridge::new();
    let goal = two_task_goal(&bridge);
    start_role(&bridge, "reviewer", "w1:p6", "claude");
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Build it"]);
    let sent = sent_text(&bridge);
    assert!(
        sent.contains(&format!(
            "report {goal} --task work \"TEXT\" (or --file PATH)"
        )),
        "{sent}"
    );
    let main = |args: &[&str]| {
        bridge
            .command(args)
            .env_remove("HGM_CONTEXT")
            .output()
            .unwrap()
    };
    let body = format!(
        "Changed parser.rs and its tests.\n{}\nAC1: verified with cargo test.\nUnresolved: none.",
        "Detail line about the change.\n".repeat(60)
    );
    let file = bridge.workspace.join("report.md");
    std::fs::write(&file, &body).unwrap();
    let filed = main(&[
        "report",
        &goal,
        "--task",
        "work",
        "--file",
        file.to_str().unwrap(),
    ]);
    assert!(
        filed.status.success(),
        "{}",
        String::from_utf8_lossy(&filed.stderr)
    );
    let short = main(&["report", &goal, "Starting the docs next."]);
    assert!(
        short.status.success(),
        "{}",
        String::from_utf8_lossy(&short.stderr)
    );
    let events = bridge.ok(&["log", &goal]);
    let reports: Vec<_> = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "report")
        .collect();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0]["detail"]["from"], "main");
    assert_eq!(reports[0]["detail"]["text"], body.trim());
    let packet = bridge.ok(&["packet", &goal]);
    assert_eq!(packet["reports"][0]["text"], body.trim());
    assert_eq!(packet["reports"][1]["task"], Value::Null);
    assert_eq!(packet["tasks"][0]["done"], false);

    let reviewer = bridge
        .command(&["report", &goal, "Looks fine"])
        .env("HGM_ROLE", "reviewer")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&reviewer.stderr).contains("may not run hgm report"));
    refused_with(&bridge, &["report", &goal, "--task", "nope", "x"], "nope");
    refused_with(
        &bridge,
        &["report", &goal, "x", "--file", file.to_str().unwrap()],
        "cannot be used with",
    );
    refused_with(
        &bridge,
        &["report", &goal, &"word ".repeat(3_001)],
        "a report takes 1 to 3000 words",
    );
    let after = bridge.ok(&["log", &goal]);
    assert_eq!(
        after
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "report")
            .count(),
        2
    );

    bridge.write("target.screen", "…the end of the report\n› ");
    bridge.write(
        "target.recent",
        "Report, first line\nmiddle\n…the end of the report\n› ",
    );
    let visible = bridge.ok(&["target"]);
    assert!(!visible["screen"].as_str().unwrap().contains("first line"));
    assert!(visible.get("lines").is_none());
    let recent = bridge.ok(&["target", "--lines", "800"]);
    assert!(
        recent["screen"]
            .as_str()
            .unwrap()
            .contains("Report, first line"),
        "{recent}"
    );
    assert_eq!(recent["lines"], 4);
    refused_with(&bridge, &["target", "--lines", "0"], "--lines");
}

#[test]
fn workers_record_decisions_and_follow_ups_and_the_packet_gathers_them_apart_from_blockers() {
    let bridge = Bridge::new();
    let goal = two_task_goal(&bridge);
    start_role(&bridge, "impl", "w1:p5", "claude");
    start_role(&bridge, "reviewer", "w1:p6", "claude");
    let launched = std::fs::read_to_string(bridge.root.path().join("started.txt")).unwrap();
    assert!(
        launched.contains("Bash(hgm decision:*)") && launched.contains("Bash(hgm follow-up:*)"),
        "{launched}"
    );

    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Build it"]);
    bridge.ok(&[
        "send", "--to", "impl", "--goal", &goal, "--task", "other", "Docs",
    ]);
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    let to_main = sent.split("w1:p5\n").next().unwrap();
    let state = bridge.state.display().to_string();
    assert!(
        to_main.contains(&format!(
            "' --workspace '{}' --state-dir '{state}' decision {goal} --task work \"WHAT\" --why \"WHY\"",
            bridge
                .workspace
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
                .replace('\'', "'\\''")
        )) && to_main.contains(&format!("follow-up {goal} --task work \"WHAT\"")),
        "{to_main}"
    );
    assert!(
        sent.contains(&format!("hgm decision {goal} --task other \"WHAT\""))
            && sent.contains(&format!("hgm follow-up {goal} --task other \"WHAT\"")),
        "{sent}"
    );

    let as_main = bridge
        .command(&[
            "decision",
            &goal,
            "--task",
            "work",
            "Parse with serde_json",
            "--why",
            "Already a dependency",
        ])
        .env_remove("HGM_CONTEXT")
        .output()
        .unwrap();
    assert!(
        as_main.status.success(),
        "{}",
        String::from_utf8_lossy(&as_main.stderr)
    );
    bridge.ok(&["follow-up", &goal, "Rename the helper later"]);
    let impl_follow_up = bridge
        .command(&["follow-up", &goal, "--task", "other", "Docs miss one flag"])
        .env("HGM_ROLE", "impl")
        .output()
        .unwrap();
    assert!(impl_follow_up.status.success());
    let refused = bridge
        .command(&["decision", &goal, "x", "--why", "y"])
        .env("HGM_ROLE", "reviewer")
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("may not run hgm decision"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    for (args, message) in [
        (
            vec!["decision", &goal, "--task", "nope", "x", "--why", "y"],
            "unknown task ID nope",
        ),
        (
            vec!["decision", &goal, "x", "--why", " "],
            "--why takes 1..150 words",
        ),
    ] {
        let output = bridge.call(&args);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut reviews = vec![];
    for (verdict, findings) in [
        (
            "revise",
            r#"[{"severity":"blocking","where":"src/a.rs:3","issue":"panics on empty input","evidence":"cargo test empty fails"},{"severity":"advisory","where":"README.md","issue":"example is dated","evidence":""}]"#,
        ),
        ("approve", "[]"),
    ] {
        let review = bridge.ok(&["review", &goal, "Check the parser"])["review"]
            .as_str()
            .unwrap()
            .to_string();
        let args = [
            "review-submit",
            &goal,
            "--review",
            &review,
            "--verdict",
            verdict,
        ];
        let output = as_role_with_input(&bridge, "reviewer", &args, findings);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        reviews.push(review);
        let packet = bridge.ok(&["packet", &goal]);
        let blockers = packet["blocking_findings"].as_array().unwrap().len();
        assert_eq!(blockers, usize::from(verdict == "revise"), "{packet}");
    }

    let packet = bridge.ok(&["packet", &goal]);
    assert_eq!(packet["counts"]["assignments"], 2, "{packet}");
    assert_eq!(packet["counts"]["corrections"], 0);
    assert_eq!(packet["acceptance"][0]["id"], "AC1");
    let decisions = packet["decisions"].as_array().unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["by"], "main");
    assert_eq!(decisions[0]["task"], "work");
    assert_eq!(decisions[0]["why"], "Already a dependency");
    let follow_ups: Vec<(String, String)> = packet["follow_ups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["by"].as_str().unwrap().to_string(),
                f["source"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        follow_ups,
        [
            ("manager".to_string(), "worker".to_string()),
            ("impl".into(), "worker".into()),
            ("reviewer".into(), "review".into())
        ]
    );
    let verdicts: Vec<&str> = packet["reviews"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["verdict"].as_str().unwrap())
        .collect();
    assert_eq!(verdicts, ["revise", "approve"]);
    assert_eq!(packet["checks"][0]["result"], "not_run");
    assert!(
        packet["not_verified"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["what"] == "check c has no recorded result"),
        "{packet}"
    );

    let log = bridge.ok(&["log", &goal]);
    assert!(
        log.as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "decision" && e["detail"]["what"] == "Parse with serde_json")
    );
    assert_eq!(bridge.ok(&["show", &goal])["status"], "ready");
}

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

const WAIT: std::time::Duration = std::time::Duration::from_secs(30);

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

fn metadata(bridge: &Bridge) -> String {
    std::fs::read_to_string(bridge.root.path().join("metadata.txt")).unwrap_or_default()
}

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
        std::fs::write(
            root.join("target.screen"),
            "Create file\nDo you want to create summary.json?\n❯ 1. Yes\n  2. No",
        )
        .unwrap();
        write_inventory(&root, "done", "idle", "original");
        stream.write_all(&status_event("done")).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(4));
    });
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
    observe("working", dialog);
    observe("blocked", dialog);
    assert_eq!(attentions(), 1);
    assert!(
        bridge.ok(&["watch"])["held"]
            .as_str()
            .is_some_and(|held| held.contains("blocked"))
    );
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
    let child = bridge
        .command(&["watch-loop", "--polling", "--run-for-secs", "30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
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
    assert!(tab.contains("assignments 0/"), "{tab}");
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
    observe("Writing summary.json\n⎿  API Error: Can't reach the API server (ENOTFOUND)\n› ");
    let typed = sent();
    assert!(
        typed.starts_with("w1:p1\nContinue the current assignment from where it stopped"),
        "{typed}"
    );
    assert!(
        !typed.contains("Goal Manager observation"),
        "no wake for it now: {typed}"
    );
    let log = bridge.ok(&["log", &goal]);
    let continued = log
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "provider_continue")
        .unwrap();
    assert_eq!(continued["detail"]["consumed"], true, "{continued}");
    assert!(
        continued["detail"]["line"]
            .as_str()
            .unwrap()
            .contains("ENOTFOUND")
    );
    observe("Continuing summary.json\nWrote summary.json\n› ");
    let wake = sent();
    let wake = &wake[typed.len()..];
    assert!(wake.contains("w1:p2\nGoal Manager observation"), "{wake}");
    assert!(wake.contains("provider_continue"), "{wake}");
    observe("Waiting for instructions\n› ");
    let attentions = || -> Vec<Value> {
        bridge
            .ok(&["log", &goal])
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "attention")
            .cloned()
            .collect()
    };
    assert!(attentions().is_empty(), "not idle long enough yet");
    idle_watch(&bridge, 200);
    observe("Still waiting\n› ");
    observe("Still waiting, again\n› ");
    let told = attentions();
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0]["detail"]["cause"], "idle_no_next_step");
    let notified = std::fs::read_to_string(bridge.root.path().join("notified.txt")).unwrap();
    assert!(
        notified.contains("Worker idle short of completion"),
        "{notified}"
    );
}

fn idle_watch(bridge: &Bridge, secs: i64) {
    let path = manager_dir(bridge).join("t-original-watch.json");
    let mut watch: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    watch["idle_since"] = json!(watch["idle_since"].as_i64().unwrap() - secs);
    std::fs::write(path, watch.to_string()).unwrap();
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
    bridge.ok(&["supervise", &goal]);
    assert_eq!(opened(), 1);
    assert_eq!(bridge.hook(json!({"stop_hook_active":false})), json!({}));

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
    assert_eq!(bridge.hook(json!({"stop_hook_active":true})), json!({}));
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
    let deployed = bridge.root.path().join("deployed");
    std::fs::create_dir(&deployed).unwrap();
    let program = deployed.join("hgm");
    std::fs::copy(env!("CARGO_BIN_EXE_hgm"), &program).unwrap();
    let (subscribed, subscription) = std::sync::mpsc::channel();
    let (redeployed, redeploy) = std::sync::mpsc::channel::<()>();
    let _server = fake_socket(&bridge, move |stream| {
        let _ = subscribed.send(());
        if redeploy.recv_timeout(WAIT).is_ok() {
            stream.write_all(&status_event("working")).unwrap();
        }
        let _ = redeploy.recv_timeout(WAIT);
    });
    let child = bridge
        .command_with(&program, &["watch-loop", "--run-for-secs", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
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
    assert_eq!(reloads.len(), 1, "{reloads:?}");
}

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
    edit_watch(
        &bridge,
        |w| w["native_goal"] = json!({"goal":goal,"assigned_at":"2026-11-18T00:00:00Z","set_at":1}),
    );
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
    assert!(bridge.ok(&["watch"])["native_goal"].is_null());
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
    assert!(
        bridge
            .ok(&["target", "--adopt", "again"])
            .get("adopted")
            .is_none()
    );
    bridge.ok(&["send", "Continue with the current step"]);

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
    let legacy = manager_dir.join(format!("{}-watch.json", bridge.context));
    std::fs::rename(manager_dir.join("t-original-watch.json"), &legacy).unwrap();
    assert_eq!(bridge.ok(&["watch"])["active"], true);
    assert!(!legacy.exists());
    assert!(manager_dir.join("t-original-watch.json").exists());
}

fn sample_wakes() -> (String, String, String) {
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
            "loop":{"patrol_minutes":1,"patrol_wake":"always","stop_when":["a fix needs a Herdr change"]}
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
    let mut watch: Value = serde_json::from_slice(&std::fs::read(&watch_path).unwrap()).unwrap();
    watch["last_sent"] = json!(0);
    watch["last_change"] = json!(chrono::Utc::now().timestamp() - 400);
    std::fs::write(&watch_path, watch.to_string()).unwrap();
    let unchanged = wake(&bridge);
    (patrol, settle, unchanged)
}
#[test]
fn wakes_carry_the_full_assignment_once_and_only_new_lines() {
    let (patrol, settle, unchanged) = sample_wakes();
    println!(
        "patrol wake: {} bytes, settle wake: {} bytes, unchanged-state wake: {} bytes",
        patrol.len(),
        settle.len(),
        unchanged.len()
    );
    assert!(
        unchanged.contains("no observed target change"),
        "{unchanged}"
    );
    assert!(
        unchanged.len() <= 1_000,
        "{} bytes: {unchanged}",
        unchanged.len()
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
    assert!(
        settle.contains("⏺ Done: committed.") && !settle.contains("Step 30:"),
        "{settle}"
    );
    for wake in [&patrol, &settle] {
        assert!(wake.contains("Contract: hgm show g-") && !wake.contains("Criterion 1"));
    }
    assert!(settle.contains("every command check passes"), "{settle}");
    assert!(
        patrol.len() < 2_100 && settle.len() < 1_200,
        "patrol {} / settle {} bytes",
        patrol.len(),
        settle.len()
    );
}

fn manager_inventory(bridge: &Bridge, worker: &str, agent: &str, state: &str) {
    let agents = json!([
        {"pane_id":"w1:p1","tab_id":"w1:t1","terminal_id":"original","agent":"codex","agent_status":worker,"agent_session":{"value":"worker"},"focused":false},
        {"pane_id":"w1:p2","tab_id":"w1:t2","terminal_id":"manager","agent":agent,"agent_status":state,"agent_session":{"value":"manager"},"focused":true}
    ]);
    bridge.write(
        "panes.json",
        &json!({"result":{"panes":agents}}).to_string(),
    );
    bridge.write(
        "agents.json",
        &json!({"result":{"agents":agents}}).to_string(),
    );
}
fn edit_watch(bridge: &Bridge, edit: impl FnOnce(&mut Value)) {
    let path = manager_dir(bridge).join("t-original-watch.json");
    let mut watch: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut watch);
    std::fs::write(path, watch.to_string()).unwrap();
}
fn wakes(bridge: &Bridge, goal: &str) -> Vec<Value> {
    bridge
        .ok(&["log", goal])
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "wake")
        .cloned()
        .collect()
}

#[test]
fn a_stop_after_new_work_reaches_a_busy_claude_manager_at_once_but_never_a_draft() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    manager_inventory(&bridge, "working", "claude", "working");
    bridge.write("manager.screen", "Thinking…\n❯ ");
    bridge.ok(&["supervise", &goal]);
    bridge.write("target.screen", "Editing summary.json\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(wakes(&bridge, &goal).is_empty(), "main is still working");
    edit_watch(&bridge, |w| w["last_sent"] = json!(chrono_now()));
    manager_inventory(&bridge, "done", "claude", "working");
    bridge.write("target.screen", "Wrote summary.json\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let sent = std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap();
    assert!(
        sent.starts_with("w1:p2\nGoal Manager observation"),
        "{sent}"
    );
    let delivered = wakes(&bridge, &goal);
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0]["detail"]["via"], "queued", "{delivered:?}");

    manager_inventory(&bridge, "working", "claude", "working");
    bridge.write("target.screen", "Checking totals\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    bridge.write("manager.screen", "Thinking…\n❯ half-typed note");
    manager_inventory(&bridge, "done", "claude", "working");
    bridge.write("target.screen", "Totals checked\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert_eq!(
        std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap(),
        sent
    );
    assert_eq!(bridge.ok(&["watch"])["status"], "waiting_for_manager");
}

#[test]
fn a_busy_codex_manager_keeps_waiting_for_its_turn_to_end() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.inventory("done", "working", "original");
    bridge.ok(&["supervise", &goal]);
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(!bridge.root.path().join("sent.txt").exists());
    assert_eq!(bridge.ok(&["watch"])["status"], "waiting_for_manager");
    assert!(wakes(&bridge, &goal).is_empty());
}

#[test]
fn idle_main_is_told_shown_and_supervise_retunes_the_running_watch() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    let armed = bridge.ok(&["supervise", &goal, "--idle-notice-secs", "60"]);
    assert_eq!(armed["idle_notice_secs"], 60);
    assert_eq!(armed["idle_secs"], 180, "the three-minute default");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert_eq!(wakes(&bridge, &goal).len(), 1, "main settled");
    idle_watch(&bridge, 120);
    bridge.write("target.screen", "Finished output\nidle\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(kinds(&bridge, &goal).contains(&"attention".to_string()));
    let notified = std::fs::read_to_string(bridge.root.path().join("notified.txt")).unwrap();
    assert!(
        notified.contains("Worker idle short of completion"),
        "{notified}"
    );

    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(manager_dir(&bridge).join("t-original-watch.lock"))
        .unwrap();
    lease.try_lock().unwrap();
    let status = bridge.ok(&["status"]);
    assert_eq!(status[0]["idle"]["role"], "main", "{status}");
    assert!(
        status[0]["idle"]["secs"].as_i64().unwrap() >= 120,
        "{status}"
    );
    let inbox = bridge.ok(&["inbox"]);
    let row = inbox
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "worker_idle")
        .unwrap();
    assert!(
        row["reason"].as_str().unwrap().contains("main idle 2m"),
        "{row}"
    );

    let retuned = bridge.ok(&[
        "supervise",
        &goal,
        "--idle-secs",
        "30",
        "--cooldown-secs",
        "40",
        "--patrol-minutes",
        "7",
        "--max-wakes",
        "1",
    ]);
    let pacing = &retuned["pacing"];
    assert_eq!(pacing["applied"]["idle_secs"], 30, "{pacing}");
    assert_eq!(pacing["applied"]["cooldown_secs"], 40);
    assert_eq!(pacing["applied"]["patrol_minutes"], 7);
    assert!(
        pacing["not_applied"]["max_wakes"]
            .as_str()
            .unwrap()
            .contains("1 wakes are already used"),
        "{pacing}"
    );
    let watch: Value = serde_json::from_slice(
        &std::fs::read(manager_dir(&bridge).join("t-original-watch.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        (watch["idle_secs"].clone(), watch["patrol_minutes"].clone()),
        (json!(30), json!(7))
    );
    assert!(kinds(&bridge, &goal).contains(&"watch_pacing".to_string()));
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn confirmed_goal(bridge: &Bridge, check: &str, extra: Value) -> String {
    let mut spec = json!({"objective":"Fix login copy","acceptance":["Copy updated"],
        "checks":[{"name":"copy","command":check}]});
    spec.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let goal = add_file(bridge, spec);
    bridge.ok(&["confirm", &goal, "--evidence", "User: go"]);
    goal
}
fn refused_with(bridge: &Bridge, args: &[&str], needle: &str) {
    let output = bridge.call(args);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "expected a refusal: {stderr}");
    assert!(stderr.contains(needle), "{needle:?} not in: {stderr}");
}
fn sent_text(bridge: &Bridge) -> String {
    std::fs::read_to_string(bridge.root.path().join("sent.txt")).unwrap_or_default()
}

#[test]
fn a_prepared_step_is_checked_at_once_and_can_be_replaced_or_withdrawn() {
    let bridge = Bridge::new();
    let prepare = |goal: &str, text: &str| -> Vec<String> {
        ["send", "--goal", goal, "--task", "work", "--prepare", text]
            .map(String::from)
            .to_vec()
    };
    fn args(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }
    let unconfirmed = add_file(
        &bridge,
        json!({"objective":"o","acceptance":["a"],"checks":[{"name":"c","command":"true"}]}),
    );
    refused_with(
        &bridge,
        &args(&prepare(&unconfirmed, "Next")),
        "no confirmed loop contract",
    );
    let planned = confirmed_goal(&bridge, "true", json!({"loop":{"plan_first":true}}));
    refused_with(
        &bridge,
        &args(&prepare(&planned, "Next")),
        &format!("hgm approve-plan {planned} --task work --evidence TEXT"),
    );
    let ask = add_file(
        &bridge,
        json!({"objective":"o","acceptance":["a"],"loop":{"autonomy":"ask"}}),
    );
    bridge.ok(&["confirm", &ask, "--evidence", "User: go"]);
    refused_with(&bridge, &args(&prepare(&ask, "Next")), "--user-approval");
    let goal = confirmed_goal(&bridge, "true", json!({}));
    refused_with(
        &bridge,
        &args(&prepare(&goal, &"word ".repeat(401))),
        "budget 400",
    );
    refused_with(
        &bridge,
        &args(&prepare(&goal, "Next")),
        "no watch runs for this tab",
    );
    bridge.ok(&["supervise", &goal]);
    let first = bridge.ok(&args(&prepare(&goal, "Write the totals")));
    assert_eq!(
        (first["status"].clone(), first["replaced"].clone()),
        (json!("prepared"), json!(false))
    );
    let second = bridge.ok(&args(&prepare(&goal, "Write the totals and the tax")));
    assert_eq!(
        (second["replaced"].clone(), second["place"].clone()),
        (json!(false), json!(2))
    );
    let one = bridge.ok(&["send", "--withdraw", "--step", "1"]);
    assert_eq!(
        (one["withdrawn"].clone(), one["left"].clone()),
        (json!(1), json!(1))
    );
    refused_with(
        &bridge,
        &["send", "--withdraw", "--step", "2"],
        "--step takes 1 to 1",
    );
    for n in 2..=5 {
        bridge.ok(&args(&prepare(&goal, &format!("Step {n}"))));
    }
    refused_with(
        &bridge,
        &args(&prepare(&goal, "Step 6")),
        "already has 5 prepared steps",
    );
    let all = bridge.ok(&["send", "--withdraw"]);
    assert_eq!(
        (all["status"].clone(), all["withdrawn"].clone()),
        (json!("withdrawn"), json!(5))
    );
    refused_with(
        &bridge,
        &["send", "--withdraw"],
        "no step is prepared for main",
    );
    let kinds = kinds(&bridge, &goal);
    assert_eq!(kinds.iter().filter(|k| *k == "prepared").count(), 6);
    assert_eq!(
        kinds.iter().filter(|k| *k == "prepared_withdrawn").count(),
        6
    );
    assert!(
        !sent_text(&bridge).contains("Write the totals"),
        "nothing typed"
    );
}

#[test]
fn the_watch_delivers_a_prepared_step_when_main_settles_and_still_wakes_the_manager() {
    let bridge = Bridge::new();
    let goal = confirmed_goal(&bridge, "true", json!({}));
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing the copy\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "--prepare",
        "Now update the tests",
    ]);
    bridge.inventory("done", "idle", "original");
    bridge.write("target.screen", "Copy updated\n› ");
    let before = sent_text(&bridge);
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let new = sent_text(&bridge)[before.len()..].to_string();
    assert!(
        new.contains(&format!(
            "w1:p1\nGoal {goal}, work, assigned to main.\nNow update the tests"
        )),
        "{new}"
    );
    assert!(new.contains("w1:p2\nGoal Manager observation"), "{new}");
    assert!(new.contains("delivered your prepared step"), "{new}");
    let log = bridge.ok(&["log", &goal]);
    let assigned: Vec<&Value> = log
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "assigned")
        .collect();
    assert_eq!(assigned.len(), 2, "it counts against max_assignments");
    assert_eq!(assigned[1]["detail"]["via"], "watch");
    refused_with(&bridge, &["send", "--withdraw"], "no step is prepared");
}

#[test]
fn queued_steps_are_delivered_in_order_one_each_time_main_settles() {
    let bridge = Bridge::new();
    let goal = confirmed_goal(&bridge, "true", json!({"loop":{"max_assignments":5}}));
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing the copy\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    for text in ["Now update the tests", "Then update the changelog"] {
        bridge.ok(&["send", "--goal", &goal, "--task", "work", "--prepare", text]);
    }
    let settle = |screen: &str| {
        cool_watch(&bridge);
        bridge.inventory("done", "idle", "original");
        bridge.write("target.screen", screen);
        let before = sent_text(&bridge).len();
        assert!(bridge.call(&["watch-loop", "--once"]).status.success());
        let new = sent_text(&bridge)[before..].to_string();
        let worker_inputs = sent_text(&bridge).matches("w1:p1\nGoal").count();
        assert!(bridge.call(&["watch-loop", "--once"]).status.success());
        assert_eq!(
            sent_text(&bridge).matches("w1:p1\nGoal").count(),
            worker_inputs,
            "a second observation of the same settle consumed a step"
        );
        bridge.inventory("working", "idle", "original");
        bridge.write(
            "target.screen",
            &format!("{screen}\nWorking on the next step\n› "),
        );
        assert!(bridge.call(&["watch-loop", "--once"]).status.success());
        new
    };
    let first = settle("Copy updated\n› ");
    assert!(
        first.contains("Now update the tests") && !first.contains("changelog"),
        "{first}"
    );
    let second = settle("Tests updated\n› ");
    assert!(second.contains("Then update the changelog"), "{second}");
    let third = settle("Changelog updated\n› ");
    assert!(
        !third.contains("w1:p1\nGoal"),
        "the queue is empty: {third}"
    );
    let assigned: Vec<Value> = bridge
        .ok(&["log", &goal])
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "assigned" && e["detail"]["via"] == "watch")
        .cloned()
        .collect();
    assert_eq!(assigned.len(), 2);

    let file = manager_dir(&bridge).join("t-original-prepared-main.json");
    std::fs::write(
        &file,
        json!({"version":1,"goal":goal,"task":"work","to":"main","text":"Old step",
               "plan":false,"user_approval":null,"prepared_at":"2026-11-15T00:00:00Z"})
        .to_string(),
    )
    .unwrap();
    let withdrawn = bridge.ok(&["send", "--withdraw"]);
    assert_eq!(withdrawn["withdrawn"], 1);
    assert!(!file.exists());
}

#[test]
fn a_prepared_step_waits_while_a_check_regressed_and_a_direct_send_supersedes_it() {
    let bridge = Bridge::new();
    std::fs::write(bridge.workspace.join("ok"), "").unwrap();
    let goal = confirmed_goal(&bridge, "test -f ok", json!({}));
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing the copy\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    bridge.inventory("done", "idle", "original");
    bridge.write("target.screen", "Copy updated\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Polish the copy"]);
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Polishing\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "--prepare",
        "Now update the tests",
    ]);
    std::fs::remove_file(bridge.workspace.join("ok")).unwrap();
    bridge.inventory("done", "idle", "original");
    bridge.write("target.screen", "Polished\n› ");
    cool_watch(&bridge);
    let before = sent_text(&bridge);
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let new = sent_text(&bridge)[before.len()..].to_string();
    assert!(
        !new.contains("w1:p1\n"),
        "kept, nothing typed into main: {new}"
    );
    assert!(
        new.contains("kept, not delivered: checks that passed before now fail (copy)"),
        "{new}"
    );
    assert!(kinds(&bridge, &goal).contains(&"prepared_held".to_string()));
    bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "Restore the ok file",
    ]);
    let log = bridge.ok(&["log", &goal]);
    let withdrawn = log
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "prepared_withdrawn")
        .unwrap();
    assert_eq!(withdrawn["detail"]["reason"], "superseded by a direct send");
}

#[test]
fn approving_a_plan_without_task_unlocks_the_task_whose_plan_is_pending() {
    let bridge = Bridge::new();
    let goal = confirmed_goal(&bridge, "true", json!({"loop":{"plan_first":true}}));
    refused_with(
        &bridge,
        &["send", "--goal", &goal, "--task", "work", "Implement it"],
        &format!("hgm approve-plan {goal} --task work --evidence TEXT"),
    );
    bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "--plan",
        "Plan the change",
    ]);
    bridge.ok(&["approve-plan", &goal, "--evidence", "Reviewed the plan"]);
    let log = bridge.ok(&["log", &goal]);
    let approved = log
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "plan_approved")
        .unwrap();
    assert_eq!(approved["detail"]["task"], "work");
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Implement it"]);
}

#[test]
fn main_working_without_progress_wakes_the_manager_and_tells_the_person_once() {
    const TOP: &str = "⏺ Running unit tests after the gate changes\n  ⎿  $ cargo test --locked --bin hgm 2>&1 | grep -E \"test\n     result|FAILED|panicked|^error\" -A8 | head -60\n\n";
    let thinking = |time: &str, tokens: &str| {
        format!(
            "{TOP}✽ Nebulizing… ({time} · ↓ {tokens} tokens · thinking with xhigh effort)\n  ⎿  Tip: Use /clear to start fresh when switching topics and free up context\n"
        )
    };
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", &thinking("50m 01s", "28.0k"));
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(!kinds(&bridge, &goal).contains(&"no_progress".to_string()));
    age_watch(&bridge, 200);
    for (time, tokens) in [("53m 40s", "30.2k"), ("55m 22s", "31.4k")] {
        cool_watch(&bridge);
        bridge.write("target.screen", &thinking(time, tokens));
        assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    }
    let kinds = kinds(&bridge, &goal);
    assert_eq!(
        kinds.iter().filter(|k| *k == "no_progress").count(),
        1,
        "{kinds:?}"
    );
    let wake = sent_text(&bridge);
    assert!(
        wake.contains("is working but has shown no progress for 3m"),
        "{wake}"
    );
    assert!(
        wake.contains("its working directory last changed at"),
        "{wake}"
    );
    let log = bridge.ok(&["log", &goal]);
    let stalled = log
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "no_progress")
        .unwrap();
    assert!(
        stalled["detail"]["work_dir_changed_at"].is_i64(),
        "{stalled}"
    );
    let notified = std::fs::read_to_string(bridge.root.path().join("notified.txt")).unwrap();
    assert_eq!(
        notified.matches("Worker idle short of completion").count(),
        1,
        "{notified}"
    );
}

struct ClaudeWorker {
    bridge: Bridge,
    context: String,
    origin: String,
    manager: std::cell::RefCell<(&'static str, &'static str)>,
}
impl ClaudeWorker {
    fn new() -> Self {
        let bridge = Bridge::new();
        let origin = json!({"pane":"w1:p1","tab":"w1:t1","terminal":"original","agent":"claude","session":{"value":"claude-1"}}).to_string();
        let output = bridge
            .command(&["manager", "--agent", "codex"])
            .env("HGM_ORIGIN_JSON", &origin)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let context = std::fs::read_to_string(bridge.root.path().join("context")).unwrap();
        let worker = Self {
            bridge,
            context,
            origin,
            manager: std::cell::RefCell::new(("codex", "idle")),
        };
        worker.state("idle", "Ready\n❯ ");
        worker
    }
    fn state(&self, status: &str, screen: &str) {
        let (manager, manager_status) = *self.manager.borrow();
        let panes = json!([
            {"pane_id":"w1:p1","tab_id":"w1:t1","terminal_id":"original","agent":"claude","agent_status":status,"agent_session":{"value":"claude-1"},"focused":false},
            {"pane_id":"w1:p2","tab_id":"w1:t2","terminal_id":"manager","agent":manager,"agent_status":manager_status,"agent_session":{"value":"manager"},"focused":true}
        ]);
        self.bridge
            .write("panes.json", &json!({"result":{"panes":panes}}).to_string());
        self.bridge.write(
            "agents.json",
            &json!({"result":{"agents":panes}}).to_string(),
        );
        self.bridge.write("target.screen", screen);
    }
    fn run(&self, args: &[&str]) -> Output {
        self.bridge
            .command(args)
            .env("HGM_CONTEXT", &self.context)
            .env("HGM_ORIGIN_JSON", &self.origin)
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn step(&self, status: &str, screen: &str) {
        self.state(status, screen);
        let output = self.run(&["watch-loop", "--once"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn goal_lines(&self) -> usize {
        sent_text(&self.bridge)
            .matches("w1:p1\n/goal hgm goal")
            .count()
    }
}

#[test]
fn a_new_contract_queues_the_native_goal_with_the_assignment_and_reads_it_back() {
    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(&worker.bridge, "true", json!({}));
    assert_eq!(
        worker.ok(&["show", &goal])["spec"]["loop"]["native_goal"],
        "assignment"
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    let sent = sent_text(&worker.bridge);
    let assignment = sent.find("Update the copy").unwrap();
    let typed = sent
        .find("w1:p1\n/goal hgm goal")
        .expect("queued with the assignment");
    assert!(typed > assignment, "{sent}");
    assert_eq!(worker.events(&goal, "native_goal_queued").len(), 1);
    assert!(
        worker.events(&goal, "native_goal_set").is_empty(),
        "pending until read"
    );
    worker.step(
        "working",
        &format!(
            "  ⎿  Goal set: hgm goal {goal} task work: the assigned work is done\n✻ Working…\n❯ "
        ),
    );
    let set = worker.events(&goal, "native_goal_set");
    assert_eq!(set[0]["detail"]["via"], "queued", "{set:?}");
    worker.step("done", "✔ Goal achieved (9s · 1 turn · 924 tokens)\n❯ ");
    assert!(!worker.events(&goal, "settle_checks").is_empty());
    assert_eq!(
        worker.events(&goal, "native_goal")[0]["detail"]["outcome"],
        "met"
    );
    let wakes = sent_text(&worker.bridge);
    assert!(wakes.contains("the native goal reports met"), "{wakes}");
    assert_eq!(worker.goal_lines(), 1, "never typed twice");

    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(&worker.bridge, "true", json!({}));
    worker.bridge.write("goal-pasted", "");
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    assert_eq!(worker.events(&goal, "native_goal_queued").len(), 1);
    worker.step("done", "Updated the copy\n❯ ");
    assert!(worker.events(&goal, "native_goal_set").is_empty());
    let now = chrono::Utc::now().timestamp();
    edit_watch(&worker.bridge, |w| {
        w["native_goal"]["set_at"] = json!(now - 60);
        w["last_change"] = json!(now - 30);
    });
    worker.step("done", "Updated the copy\n❯ ");
    let set = worker.events(&goal, "native_goal_set");
    assert_eq!(set[0]["detail"]["delivery"], "uncertain", "{set:?}");
    worker.step("done", "Updated the copy\n❯ ");
    assert_eq!(
        worker.goal_lines(),
        1,
        "an uncertain set is never typed again"
    );

    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(&worker.bridge, "true", json!({"loop":{"plan_first":true}}));
    worker.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "--plan",
        "Plan the copy",
    ]);
    assert_eq!(worker.goal_lines(), 0);
}

#[test]
fn a_dialog_that_opens_while_the_settle_checks_run_gets_no_native_goal() {
    let worker = ClaudeWorker::new();
    worker.bridge.write(
        "dialog.screen",
        "   Teach auto mode about your environment?\n   ❯ How you use Claude here     Mixed\n     Continue\n   ←/→ to change · Enter to continue · Esc to cancel\n",
    );
    let root = worker.bridge.root.path();
    let check = format!(
        "test -e '{shown}' || {{ cp '{dialog}' '{screen}' && touch '{shown}'; }}",
        shown = root.join("dialog-shown").display(),
        dialog = root.join("dialog.screen").display(),
        screen = root.join("target.screen").display(),
    );
    let goal = confirmed_goal(
        &worker.bridge,
        &check,
        json!({"loop":{"native_goal":"settle"}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    worker.step("done", "Updated the copy\n❯ ");
    assert!(root.join("dialog-shown").exists(), "the settle check ran");
    assert_eq!(
        worker.goal_lines(),
        0,
        "nothing typed into the dialog: {}",
        sent_text(&worker.bridge)
    );
    assert!(!kinds(&worker.bridge, &goal).contains(&"native_goal_set".to_string()));
    worker.step("done", "Updated the copy\n❯ ");
    assert_eq!(worker.goal_lines(), 1, "{}", sent_text(&worker.bridge));
}

const DIALOG_AFTER_READ: &str = "   Teach auto mode about your environment?\n   ❯ How you use Claude here     Mixed\n     Continue\n   ←/→ to change · Enter to continue · Esc to cancel\n";

#[test]
fn the_watch_reads_the_worker_again_before_typing_a_continue_or_a_clear() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    cool_watch(&bridge);
    bridge.write(
        "target.screen",
        "Writing summary.json\n⎿  API Error: Can't reach the API server (ENOTFOUND)\n› ",
    );
    bridge.write(
        "after-read.screen",
        "⎿  API Error: Can't reach the API server (ENOTFOUND)\n  Allow this command?\n› 1. Yes\n",
    );
    bridge.inventory("done", "idle", "original");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let sent = sent_text(&bridge);
    assert!(!sent.contains("w1:p1\nContinue"), "{sent}");
    let log = bridge.ok(&["log", &goal]);
    assert!(
        !log.as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "provider_continue"),
        "{log}"
    );

    let (worker, goal) = ClaudeWorker::with_native_goal(
        json!({"loop":{"stop_checks":[{"name":"stop-file","command":"test -f STOP"}]}}),
    );
    std::fs::write(worker.bridge.workspace.join("STOP"), "").unwrap();
    cool_watch(&worker.bridge);
    worker.state("done", "Still updated\n❯ ");
    worker.bridge.write("after-read.screen", DIALOG_AFTER_READ);
    assert!(worker.run(&["watch-loop", "--once"]).status.success());
    assert_eq!(worker.clears_typed(), 0, "{}", sent_text(&worker.bridge));
    assert!(worker.cleared(&goal).is_empty());
    assert!(!worker.events(&goal, "stop_condition").is_empty());
}

#[test]
fn input_that_the_worker_or_manager_did_not_take_is_recorded_as_uncertain() {
    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"native_goal":"settle"}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    worker.bridge.write(
        "after-prompt.screen",
        "Updated the copy\n❯ /goal hgm goal (still in the input)\n",
    );
    worker.step("done", "Updated the copy\n❯ ");
    let set = worker.events(&goal, "native_goal_set");
    assert_eq!(
        set[0]["detail"]["why"], "the input line did not empty",
        "{set:?}"
    );
    assert_eq!(set[0]["detail"]["delivery"], "uncertain", "{set:?}");
    std::fs::remove_file(worker.bridge.root.path().join("after-prompt.screen")).unwrap();
    worker.state("done", "Updated the copy\n❯ ");
    worker.ok(&["wait", &goal, "--reason", "User decides"]);
    assert_eq!(worker.clears_typed(), 0, "{}", sent_text(&worker.bridge));
    let typed = worker.goal_lines();
    worker.ok(&["supervise", &goal]);
    worker.step("done", "Updated the copy\n❯ ");
    assert_eq!(
        worker.goal_lines(),
        typed,
        "uncertain input was replayed after rearming"
    );

    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"native_goal":"settle"}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    worker.bridge.write("goal-pasted", "");
    worker.step("done", "Updated the copy\n❯ ");
    assert_eq!(worker.goal_lines(), 1, "{}", sent_text(&worker.bridge));
    let set = worker.events(&goal, "native_goal_set");
    assert_eq!(
        set[0]["detail"]["why"], "no answer to it appeared on screen",
        "{set:?}"
    );
    assert_eq!(set[0]["detail"]["delivery"], "uncertain", "{set:?}");

    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    cool_watch(&bridge);
    bridge.write("target.screen", "Wrote summary.json\n› ");
    bridge.write(
        "after-prompt-manager.screen",
        "› Goal Manager observation (still in the input)",
    );
    bridge.inventory("done", "idle", "original");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let log = bridge.ok(&["log", &goal]);
    let wakes: Vec<_> = log
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "wake")
        .collect();
    assert_eq!(wakes.len(), 1, "{log}");
    assert_eq!(wakes[0]["detail"]["via"], "prompt_unconfirmed", "{log}");
}

#[test]
fn a_native_goal_is_set_after_an_assignment_read_as_an_observation_and_cleared_on_wait() {
    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"native_goal":"settle"}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(worker.goal_lines(), 0, "never typed into a working worker");
    worker.step("done", "Updated the copy\n❯ ");
    assert_eq!(worker.goal_lines(), 1, "{}", sent_text(&worker.bridge));
    let sent = sent_text(&worker.bridge);
    let line = sent
        .lines()
        .find(|l| l.starts_with("/goal hgm goal"))
        .unwrap();
    assert!(
        line.contains(&format!("hgm goal {goal} task work")) && line.contains("`true`"),
        "{line}"
    );
    assert!(
        !line.contains("Update the copy"),
        "no assignment text in the condition"
    );
    assert!(line.chars().count() < 4_000);
    assert!(kinds(&worker.bridge, &goal).contains(&"native_goal_set".to_string()));
    worker.step(
        "working",
        "Working toward the goal\n◎ /goal active (12s)\n❯ ",
    );
    worker.step("done", "✔ Goal achieved (59s · 1 turn · 1.9k tokens)\n❯ ");
    let log = worker.ok(&["log", &goal]);
    let verdict = log
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "native_goal")
        .unwrap();
    assert_eq!(verdict["detail"]["outcome"], "met");
    let status = worker.ok(&["status", "--all"]);
    assert_eq!(
        status[0]["native_goal"],
        json!({"set": false, "outcome": "met"}),
        "{status}"
    );
    assert_eq!(status[0]["status"], "ready", "a verdict accepts nothing");
    assert!(worker.ok(&["show", &goal])["native_goal"]["outcome"] == "met");
    let wakes = sent_text(&worker.bridge);
    assert!(wakes.contains("the native goal reports met"), "{wakes}");
    worker.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "--plan",
        "Plan the polish",
    ]);
    worker.step("working", "Planning\n❯ ");
    worker.step("done", "Plan written\n❯ ");
    assert_eq!(worker.goal_lines(), 1);
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Polish the copy"]);
    worker.step("working", "Polishing\n❯ ");
    worker.step("done", "Polished\n❯ ");
    assert_eq!(worker.goal_lines(), 2);
    worker.ok(&["wait", &goal, "--reason", "Which tone should it use?"]);
    assert!(
        sent_text(&worker.bridge).ends_with("w1:p1\n/goal clear\n"),
        "{}",
        sent_text(&worker.bridge)
    );
    assert!(kinds(&worker.bridge, &goal).contains(&"native_goal_cleared".to_string()));
}

#[test]
fn an_unavailable_native_goal_is_recorded_once_and_work_goes_on_without_it() {
    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"native_goal":"settle"}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing\n❯ ");
    worker.step("done", "Updated\n❯ ");
    worker.step("idle", "Updated\nUnknown command: /goal\n❯ ");
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Polish the copy"]);
    worker.step("working", "Polishing\n❯ ");
    worker.step("done", "Polished\n❯ ");
    assert_eq!(worker.goal_lines(), 1, "not set again");
    let kinds = kinds(&worker.bridge, &goal);
    assert_eq!(
        kinds
            .iter()
            .filter(|k| *k == "native_goal_unavailable")
            .count(),
        1
    );
}

#[test]
fn no_native_goal_for_codex_or_when_the_contract_turns_it_off() {
    let bridge = Bridge::new();
    let goal = confirmed_goal(&bridge, "true", json!({}));
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    bridge.inventory("working", "idle", "original");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    bridge.inventory("done", "idle", "original");
    bridge.write("target.screen", "Updated\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(!sent_text(&bridge).contains("/goal"), "Codex stays off");
    let worker = ClaudeWorker::new();
    let off = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"native_goal":false}}),
    );
    worker.ok(&["send", "--goal", &off, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing\n❯ ");
    worker.step("done", "Updated\n❯ ");
    assert!(!sent_text(&worker.bridge).contains("/goal"), "turned off");
}

impl ClaudeWorker {
    fn with_native_goal(mut extra: Value) -> (Self, String) {
        let worker = Self::new();
        let policy = extra
            .as_object_mut()
            .unwrap()
            .entry("loop")
            .or_insert_with(|| json!({}));
        policy["native_goal"] = json!("settle");
        let goal = confirmed_goal(&worker.bridge, "true", extra);
        worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
        worker.step("working", "Editing the copy\n❯ ");
        worker.step("done", "Updated the copy\n❯ ");
        assert_eq!(worker.goal_lines(), 1, "{}", sent_text(&worker.bridge));
        (worker, goal)
    }
    fn events(&self, goal: &str, kind: &str) -> Vec<Value> {
        self.ok(&["log", goal])
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == kind)
            .cloned()
            .collect()
    }
    fn cleared(&self, goal: &str) -> Vec<Value> {
        self.events(goal, "native_goal_cleared")
    }
    fn clears_typed(&self) -> usize {
        sent_text(&self.bridge)
            .matches("w1:p1\n/goal clear\n")
            .count()
    }
}

#[test]
fn pausing_or_cancelling_a_goal_clears_its_native_goal() {
    for (command, reason) in [
        ("pause", "the goal is paused"),
        ("cancel", "the goal is cancelled"),
    ] {
        let (worker, goal) = ClaudeWorker::with_native_goal(json!({}));
        worker.ok(&[command, &goal]);
        assert_eq!(worker.clears_typed(), 1, "{command}");
        let cleared = worker.cleared(&goal);
        assert_eq!(cleared.len(), 1, "{command}");
        assert_eq!(cleared[0]["detail"]["reason"], reason);
        assert_eq!(cleared[0]["detail"]["consumed"], true);
    }
}

#[test]
fn a_handoff_clears_the_native_goal_before_anything_else() {
    let (worker, goal) = ClaudeWorker::with_native_goal(json!({}));
    let before = sent_text(&worker.bridge);
    worker.run(&["handoff"]);
    let after = sent_text(&worker.bridge)[before.len()..].to_string();
    assert!(after.starts_with("w1:p1\n/goal clear\n"), "{after}");
    assert_eq!(
        worker.cleared(&goal)[0]["detail"]["reason"],
        "before a handoff"
    );
}

#[test]
fn a_stop_condition_protected_path_or_spent_budget_clears_the_native_goal_first() {
    let kinds_after = |worker: &ClaudeWorker, goal: &str| -> Vec<String> {
        let all = kinds(&worker.bridge, goal);
        let from = all.iter().position(|k| k == "native_goal_set").unwrap();
        all[from..].to_vec()
    };
    let (worker, goal) = ClaudeWorker::with_native_goal(
        json!({"loop":{"stop_checks":[{"name":"stop-file","command":"test -f STOP"}]}}),
    );
    std::fs::write(worker.bridge.workspace.join("STOP"), "").unwrap();
    cool_watch(&worker.bridge);
    worker.step("done", "Still updated\n❯ ");
    assert_eq!(
        worker.cleared(&goal)[0]["detail"]["reason"],
        "a stop condition holds"
    );
    let order = kinds_after(&worker, &goal);
    let cleared = order
        .iter()
        .position(|k| k == "native_goal_cleared")
        .unwrap();
    let stopped = order.iter().position(|k| k == "stop_condition").unwrap();
    assert!(cleared < stopped, "{order:?}");

    let worker = ClaudeWorker::new();
    std::fs::create_dir_all(worker.bridge.workspace.join("auth")).unwrap();
    std::fs::write(worker.bridge.workspace.join("auth/keys.txt"), "v1").unwrap();
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"protected_paths":["auth"],"loop":{"native_goal":"settle"}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing\n❯ ");
    worker.step("done", "Updated\n❯ ");
    assert_eq!(worker.goal_lines(), 1);
    std::fs::write(worker.bridge.workspace.join("auth/keys.txt"), "v2").unwrap();
    worker.step("done", "Touched auth\n❯ ");
    assert_eq!(
        worker.cleared(&goal)[0]["detail"]["reason"],
        "protected paths changed"
    );
    let order = kinds_after(&worker, &goal);
    let cleared = order
        .iter()
        .position(|k| k == "native_goal_cleared")
        .unwrap();
    let crossed = order.iter().position(|k| k == "boundary_crossed").unwrap();
    assert!(cleared < crossed, "{order:?}");

    let (worker, goal) = ClaudeWorker::with_native_goal(json!({}));
    edit_watch(&worker.bridge, |w| w["max_wakes"] = w["wakes"].clone());
    worker.step("done", "Still updated\n❯ ");
    assert_eq!(
        worker.cleared(&goal)[0]["detail"]["reason"],
        "wake_budget_exhausted"
    );
    assert!(worker.ok(&["show", &goal])["waiting"].is_object());
}

#[test]
fn a_working_worker_gets_the_clear_as_queued_input_but_never_into_a_draft() {
    let (worker, goal) = ClaudeWorker::with_native_goal(json!({}));
    worker.state(
        "working",
        "Working toward the goal\n◎ /goal active (12s)\n❯ ",
    );
    worker.ok(&["pause", &goal]);
    assert_eq!(worker.clears_typed(), 1);
    assert_eq!(worker.cleared(&goal).len(), 1);

    let (worker, goal) = ClaudeWorker::with_native_goal(json!({}));
    worker.state(
        "working",
        "Working toward the goal\n◎ /goal active (12s)\n❯ half-typed note",
    );
    worker.ok(&["pause", &goal]);
    assert_eq!(worker.clears_typed(), 0, "nothing typed into a draft");
    assert!(worker.cleared(&goal).is_empty(), "the clear waits");
}

#[test]
fn an_uncertain_set_or_clear_is_recorded_and_never_replayed() {
    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"native_goal":"settle"}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing\n❯ ");
    worker.bridge.write("fail-send-w1:p1", "");
    worker.step("done", "Updated\n❯ ");
    std::fs::remove_file(worker.bridge.root.path().join("fail-send-w1:p1")).unwrap();
    cool_watch(&worker.bridge);
    worker.step("done", "Updated, again\n❯ ");
    assert_eq!(
        worker.goal_lines(),
        1,
        "attempted once: {}",
        sent_text(&worker.bridge)
    );
    let set = worker.events(&goal, "native_goal_set");
    assert_eq!(set.len(), 1);
    assert_eq!(set[0]["detail"]["delivery"], "uncertain");

    let (worker, goal) = ClaudeWorker::with_native_goal(json!({}));
    worker.bridge.write("fail-send-w1:p1", "");
    worker.ok(&["pause", &goal]);
    std::fs::remove_file(worker.bridge.root.path().join("fail-send-w1:p1")).unwrap();
    worker.step("done", "Paused\n❯ ");
    assert_eq!(worker.clears_typed(), 1, "attempted once");
    let cleared = worker.cleared(&goal);
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0]["detail"]["delivery"], "uncertain");
    assert_eq!(worker.ok(&["show", &goal])["control"], "pause");
}

fn manager_goal_lines(bridge: &Bridge) -> usize {
    sent_text(bridge)
        .matches("w1:p2\n/goal hgm Goal Manager (")
        .count()
}
fn manager_clears(bridge: &Bridge) -> usize {
    sent_text(bridge).matches("w1:p2\n/goal clear").count()
}
fn status_row(worker: &ClaudeWorker, goal: &str) -> Value {
    worker
        .ok(&["status"])
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == goal)
        .cloned()
        .unwrap()
}
fn claude_manager_with_goal() -> (ClaudeWorker, String) {
    let worker = ClaudeWorker::new();
    *worker.manager.borrow_mut() = ("claude", "working");
    worker.bridge.write("manager.screen", "✻ Thinking…\n❯ ");
    worker.state("idle", "Ready\n❯ ");
    let goal = confirmed_goal(&worker.bridge, "true", json!({}));
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    worker.step("working", "Editing the copy\n❯ ");
    (worker, goal)
}

#[test]
fn a_claude_manager_gets_its_own_goal_while_a_confirmed_goal_runs() {
    let worker = ClaudeWorker::new();
    *worker.manager.borrow_mut() = ("claude", "working");
    worker.bridge.write("manager.screen", "✻ Thinking…\n❯ ");
    worker.state("idle", "Ready\n❯ ");
    let goal = confirmed_goal(&worker.bridge, "true", json!({}));
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(
        manager_goal_lines(&worker.bridge),
        1,
        "{}",
        sent_text(&worker.bridge)
    );
    let set = worker.events(&goal, "manager_goal_set");
    assert_eq!(set[0]["detail"]["via"], "queued", "{set:?}");
    assert_eq!(set[0]["detail"]["pending"], true);
    let condition = set[0]["detail"]["condition"].as_str().unwrap();
    assert!(
        condition.contains("ready for review, waiting, completed or cancelled")
            && condition.contains("only the user can decide")
            && condition.len() < 700,
        "{condition}"
    );
    assert_eq!(
        status_row(&worker, &goal)["manager_goal"],
        json!({"set": true, "outcome": "pending", "guarded": false})
    );
    worker.step("working", "Editing the copy\n❯ ");
    let set = worker.events(&goal, "manager_goal_set");
    assert_eq!(set.len(), 2, "{set:?}");
    assert_eq!(set[1]["detail"]["consumed"], true);
    let row = status_row(&worker, &goal);
    assert_eq!(
        row["manager_goal"],
        json!({"set": true, "outcome": null, "guarded": false})
    );
    assert_eq!(row["native_goal"]["set"], true, "{row}");
    assert_eq!(worker.ok(&["show", &goal])["manager_goal"]["set"], true);
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(manager_goal_lines(&worker.bridge), 1, "set once");

    worker.ok(&["wait", &goal, "--reason", "Which audience?"]);
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(manager_clears(&worker.bridge), 1);
    let cleared = worker.events(&goal, "manager_goal_cleared");
    assert_eq!(
        cleared[0]["detail"]["reason"],
        "no confirmed goal of this tab is in progress"
    );
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(manager_clears(&worker.bridge), 1, "cleared once");

    let worker = ClaudeWorker::new();
    *worker.manager.borrow_mut() = ("claude", "working");
    worker.bridge.write("manager.screen", "✻ Thinking…\n❯ ");
    std::fs::create_dir_all(worker.bridge.workspace.join(".hgm")).unwrap();
    std::fs::write(
        worker.bridge.workspace.join(".hgm/config.yaml"),
        "manager:\n  native_goal: false\n",
    )
    .unwrap();
    worker.state("idle", "Ready\n❯ ");
    let goal = confirmed_goal(&worker.bridge, "true", json!({}));
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(manager_goal_lines(&worker.bridge), 0);
    assert!(status_row(&worker, &goal).get("manager_goal").is_none());

    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(&worker.bridge, "true", json!({}));
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    assert!(!sent_text(&worker.bridge).contains("w1:p2\n/goal"));

    let worker = ClaudeWorker::new();
    *worker.manager.borrow_mut() = ("claude", "working");
    worker
        .bridge
        .write("manager.screen", "✻ Thinking…\n❯ user's half-typed note");
    worker.state("idle", "Ready\n❯ ");
    let goal = confirmed_goal(&worker.bridge, "true", json!({}));
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(manager_goal_lines(&worker.bridge), 0);
}

#[test]
fn the_spin_guard_clears_a_manager_goal_that_continues_with_no_await_once() {
    let (worker, goal) = claude_manager_with_goal();
    let lock_path = manager_dir(&worker.bridge).join("t-original-await.lock");
    let spinning = "◯ Goal not yet met… continuing\n⏺ Nothing new.\n◯ Goal not yet met… continuing\n⏺ Nothing new.\n◯ Goal not yet met… continuing\n✻ Thinking…\n❯ ";
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .unwrap();
    lock.try_lock().unwrap();
    worker.bridge.write("manager.screen", spinning);
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(manager_clears(&worker.bridge), 0);
    drop(lock);
    worker.bridge.write(
        "manager.screen",
        &format!("{spinning}\n  ⎿  ◯ Goal not yet met… continuing\n❯ "),
    );
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(manager_clears(&worker.bridge), 0);
    worker.bridge.write(
        "manager.screen",
        &format!("{spinning}\n{spinning}\n  ⎿  quoted\n❯ "),
    );
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(
        manager_clears(&worker.bridge),
        1,
        "{}",
        sent_text(&worker.bridge)
    );
    let cleared = worker.events(&goal, "manager_goal_cleared");
    assert_eq!(cleared[0]["detail"]["spin_guard"], true, "{cleared:?}");
    let told = worker.events(&goal, "attention");
    assert_eq!(
        told.last().unwrap()["detail"]["cause"],
        "manager_goal_spinning"
    );
    assert_eq!(status_row(&worker, &goal)["manager_goal"]["guarded"], true);
    worker.ok(&["correct", &goal, "--task", "work", "Keep to the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(manager_goal_lines(&worker.bridge), 1);
    assert_eq!(manager_clears(&worker.bridge), 1);

    let (worker, goal) = claude_manager_with_goal();
    worker.bridge.write("manager.screen", spinning);
    worker.ok(&["correct", &goal, "--task", "work", "Keep to the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(manager_clears(&worker.bridge), 0);
}

#[test]
fn a_running_await_takes_the_wake_past_a_draft_and_prints_it() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    bridge.write("manager.screen", "› user's unfinished draft");
    let waiting = bridge
        .command(&["await", "--timeout", "30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let lock_path = manager_dir(&bridge).join("t-original-await.lock");
    let held = (0..50).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::File::open(&lock_path).is_ok_and(|file| file.try_lock().is_err())
    });
    assert!(held, "the await holds its lock");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let output = waiting.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed["goal"], goal.as_str());
    let notice = printed["notice"].as_str().unwrap();
    assert!(notice.contains("Contract: hgm show g-"), "{notice}");
    assert!(
        !sent_text(&bridge).contains("w1:p2"),
        "nothing typed into the manager: {}",
        sent_text(&bridge)
    );
    let delivered = wakes(&bridge, &goal);
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0]["detail"]["via"], "await");

    edit_watch(&bridge, |w| {
        w["last_observation_at"] = json!(0);
        w["last_change"] = json!(chrono_now());
    });
    let timeout = bridge.ok(&["await", "--timeout", "1"]);
    assert_eq!(timeout["ended"], "timeout", "{timeout}");
    bridge.ok(&["wait", &goal, "--reason", "Which audience?"]);
    let ended = bridge.ok(&["await", "--timeout", "30"]);
    assert_eq!(ended["ended"], "watch_ended", "{ended}");
    assert!(ended["text"].as_str().unwrap().contains("waiting_for_user"));
}

#[test]
fn a_manager_goal_that_ended_where_hgm_did_not_see_it_is_read_from_the_indicator() {
    let (worker, goal) = claude_manager_with_goal();
    *worker.manager.borrow_mut() = ("claude", "done");
    let past = chrono_now() - 60;
    edit_watch(&worker.bridge, |w| {
        w["manager_goal"]["set_at"] = json!(past)
    });
    worker.bridge.write(
        "manager.screen",
        "⏺ 待機\n✻ Crunched for 2s · done 3:05 AM\n    ◎ /goal active (12s)\n────\n❯ \n────\n",
    );
    worker.step("working", "Editing the copy\n❯ ");
    assert!(worker.events(&goal, "manager_goal").is_empty());
    worker.bridge.write(
        "manager.screen",
        "⏺ 待機\n✔ Goal achieved (4s · 1 turn · 647 tokens)\n✻ Sautéed for 5s · done 3:05 AM\n❯ Goal set: hgm Goal Manager (03:05:34): done when hgm status shows\n⏺ 待機\n✻ Crunched for 2s · done 3:05 AM\n────\n❯ \n────\n",
    );
    worker.step("working", "Editing the copy\n❯ ");
    let ended = worker.events(&goal, "manager_goal");
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0]["detail"]["outcome"], "met");
    assert_eq!(
        status_row(&worker, &goal)["manager_goal"]["set"],
        false,
        "the display follows"
    );
    worker.step("working", "Editing the copy\n❯ ");
    assert_eq!(manager_goal_lines(&worker.bridge), 1);
}

#[test]
fn a_worker_goal_met_at_the_stop_that_ran_it_is_read_from_the_indicator() {
    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(&worker.bridge, "true", json!({}));
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    let past = chrono_now() - 60;
    let met = format!(
        "⏺ Updated the copy.\n✔ Goal achieved (4s · 1 turn · 647 tokens)\n✻ Sautéed for 5s · done 3:05 AM\n❯ Goal set: hgm goal {goal} task work: the assigned work is done\n⏺ Done.\n────\n❯ \n────\n"
    );
    worker.step(
        "done",
        &met.replace("⏺ Done.\n", "⏺ Done.\n    ◎ /goal active (4s)\n"),
    );
    assert_eq!(
        worker.events(&goal, "native_goal_set")[0]["detail"]["via"],
        "queued"
    );
    edit_watch(&worker.bridge, |w| w["native_goal"]["set_at"] = json!(past));
    worker.step(
        "done",
        &met.replace("⏺ Done.\n", "⏺ Done.\n    ◎ /goal active (4s)\n"),
    );
    assert!(worker.events(&goal, "native_goal").is_empty());
    worker.step("done", &met);
    let ended = worker.events(&goal, "native_goal");
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0]["detail"]["outcome"], "met");
    assert_eq!(ended[0]["detail"]["read"], "indicator");
    let row = status_row(&worker, &goal);
    assert_eq!(row["native_goal"]["set"], false, "{row}");
    worker.step("done", &met);
    assert_eq!(
        worker.events(&goal, "native_goal").len(),
        1,
        "recorded once"
    );
    assert_eq!(worker.goal_lines(), 1, "never typed again");
}

fn hold_operation_lock(bridge: &Bridge) -> std::fs::File {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(manager_dir(bridge).join("t-original-operation.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    lock
}

#[test]
fn a_quiet_patrol_still_runs_the_stop_checks() {
    let bridge = Bridge::new();
    let stop = bridge.root.path().join("STOP");
    let spec = json!({"objective":"Long report","acceptance":["Report covers Q3"],
        "checks":[{"name":"report","command":"true"}],
        "loop":{"patrol_minutes":1,
            "stop_checks":[{"name":"stop-file","command":format!("test -e '{}'", stop.display())}]}});
    let goal = add_file(&bridge, spec);
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
    bridge.write("target.screen", "Writing section 1\n› ");
    edit_watch(&bridge, |w| w["last_patrol"] = json!(0));
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(!kinds(&bridge, &goal).contains(&"stop_condition".to_string()));
    std::fs::write(&stop, "").unwrap();
    bridge.write("target.screen", "Writing section 2\n› ");
    edit_watch(&bridge, |w| w["last_patrol"] = json!(0));
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(
        kinds(&bridge, &goal).contains(&"stop_condition".to_string()),
        "{:?}",
        kinds(&bridge, &goal)
    );
    assert!(bridge.ok(&["show", &goal])["waiting"].is_object());
}

#[test]
fn a_native_goal_a_draft_kept_from_clearing_is_cleared_after_the_watch_ends() {
    let (worker, goal) = ClaudeWorker::with_native_goal(json!({}));
    worker.state("done", "Updated the copy\n❯ half-typed note");
    let paused = worker.run(&["pause", &goal]);
    assert!(paused.status.success());
    let said = String::from_utf8_lossy(&paused.stderr);
    assert!(said.contains("input is not free"), "{said}");
    assert_eq!(worker.clears_typed(), 0);
    worker.step("done", "Updated the copy\n❯ half-typed note");
    assert_eq!(worker.clears_typed(), 0, "never into a draft");
    worker.step("done", "Updated the copy\n❯ ");
    assert_eq!(worker.clears_typed(), 1);
    let cleared = worker.cleared(&goal);
    assert_eq!(
        cleared.last().unwrap()["detail"]["reason"],
        "the watch ended"
    );
    worker.step("done", "Updated the copy\n❯ ");
    assert_eq!(worker.clears_typed(), 1, "cleared once");
}

#[test]
fn a_native_goal_is_not_cleared_in_a_session_the_user_started_since() {
    let (worker, goal) = ClaudeWorker::with_native_goal(json!({}));
    let panes = json!([
        {"pane_id":"w1:p1","tab_id":"w1:t1","terminal_id":"replacement","agent":"claude","agent_status":"idle","focused":false},
        {"pane_id":"w1:p2","tab_id":"w1:t2","terminal_id":"manager","agent":"codex","agent_status":"idle","agent_session":{"value":"manager"},"focused":true}
    ]);
    worker
        .bridge
        .write("panes.json", &json!({"result":{"panes":panes}}).to_string());
    worker.bridge.write(
        "agents.json",
        &json!({"result":{"agents":panes}}).to_string(),
    );
    worker.bridge.write("target.screen", "Fresh session\n❯ ");
    assert!(worker.run(&["pause", &goal]).status.success());
    assert_eq!(worker.clears_typed(), 0);
    let cleared = worker.cleared(&goal);
    assert_eq!(cleared[0]["detail"]["delivery"], "skipped", "{cleared:?}");
}

#[test]
fn a_stop_reads_the_watch_again_under_the_lease() {
    let (worker, goal) = ClaudeWorker::with_native_goal(json!({}));
    let lock = hold_operation_lock(&worker.bridge);
    let pausing = worker
        .bridge
        .command(&["pause", &goal])
        .env("HGM_CONTEXT", &worker.context)
        .env("HGM_ORIGIN_JSON", &worker.origin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    edit_watch(&worker.bridge, |w| w["wakes"] = json!(7));
    drop(lock);
    let output = pausing.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(worker.clears_typed(), 1);
    let watch: Value = serde_json::from_slice(
        &std::fs::read(manager_dir(&worker.bridge).join("t-original-watch.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(watch["wakes"], 7, "not overwritten by an earlier read");
}

#[test]
fn a_direct_send_waits_while_a_watch_step_holds_the_lease() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    let lock = hold_operation_lock(&bridge);
    let sending = bridge
        .command(&["send", "Direct note while the watch works"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(
        !sent_text(&bridge).contains("Direct note"),
        "typed under a held lease"
    );
    drop(lock);
    let output = sending.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(sent_text(&bridge).contains("Direct note while the watch works"));
}

#[test]
fn a_prepared_step_the_watch_was_typing_when_it_stopped_is_never_typed_again() {
    let bridge = Bridge::new();
    let goal = confirmed_goal(&bridge, "true", json!({}));
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "First step"]);
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing\n› ");
    bridge.ok(&[
        "send",
        "--goal",
        &goal,
        "--task",
        "work",
        "--prepare",
        "Second step",
    ]);
    let file = manager_dir(&bridge).join("t-original-prepared-main.json");
    let mut steps: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    steps[0]["sending_at"] = json!("2026-11-16T00:00:00Z");
    std::fs::write(&file, steps.to_string()).unwrap();
    bridge.inventory("done", "idle", "original");
    bridge.write("target.screen", "First step done\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(
        !delivered(&bridge, &goal, "Second step"),
        "never typed into main again: {}",
        sent_text(&bridge)
    );
    let uncertain: Vec<Value> = bridge
        .ok(&["log", &goal])
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "prepared_delivery_uncertain")
        .cloned()
        .collect();
    assert_eq!(
        uncertain[0]["detail"]["error"], "the watch stopped while delivering it",
        "{uncertain:?}"
    );
    assert!(!file.exists(), "taken off the queue");
}

#[test]
fn a_native_goal_a_stop_left_set_is_cleared_by_the_next_watch_of_the_pane() {
    let (worker, a) = ClaudeWorker::with_native_goal(json!({}));
    worker.state("done", "Updated the copy\n❯ half-typed note");
    assert!(worker.run(&["pause", &a]).status.success());
    assert_eq!(worker.clears_typed(), 0);
    let b = confirmed_goal(&worker.bridge, "true", json!({}));
    worker.state("done", "Updated the copy\n❯ ");
    worker.ok(&["supervise", &b]);
    worker.step("done", "Updated the copy\n❯ ");
    assert_eq!(worker.clears_typed(), 1, "{}", sent_text(&worker.bridge));
    let cleared = worker.cleared(&a);
    assert!(
        cleared.last().unwrap()["detail"]["reason"]
            .as_str()
            .unwrap()
            .contains(&a),
        "{cleared:?}"
    );
}

#[test]
fn the_ending_watch_waits_for_the_operation_lease_before_the_watch_lease() {
    let (worker, goal) = ClaudeWorker::with_native_goal(json!({}));
    worker.state("done", "Updated the copy\n❯ half-typed note");
    assert!(worker.run(&["pause", &goal]).status.success());
    let lock = hold_operation_lock(&worker.bridge);
    let draining = worker
        .bridge
        .command(&["watch-loop", "--once"])
        .env("HGM_CONTEXT", &worker.context)
        .env("HGM_ORIGIN_JSON", &worker.origin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let watch_lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(manager_dir(&worker.bridge).join("t-original-watch.lock"))
        .unwrap();
    assert!(watch_lock.try_lock().is_ok(), "the watch lease stays free");
    drop(watch_lock);
    drop(lock);
    let _ = draining.wait_with_output().unwrap();
}

#[test]
fn after_an_uncertain_prepared_step_the_next_waits_for_the_manager() {
    let bridge = Bridge::new();
    let goal = confirmed_goal(&bridge, "true", json!({}));
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "First step"]);
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let prepare = |text: &str| {
        bridge.ok(&["send", "--goal", &goal, "--task", "work", "--prepare", text]);
    };
    prepare("Second step");
    let file = manager_dir(&bridge).join("t-original-prepared-main.json");
    bridge.write("snapshot-prepared", file.to_str().unwrap());
    bridge.inventory("done", "idle", "original");
    bridge.write("target.screen", "First step done\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(
        delivered(&bridge, &goal, "Second step"),
        "{}",
        sent_text(&bridge)
    );
    let at_prompt: Value = serde_json::from_slice(
        &std::fs::read(bridge.root.path().join("prepared-at-prompt.json")).unwrap(),
    )
    .unwrap();
    assert!(
        at_prompt[0]["sending_at"].is_string(),
        "marked before typing: {at_prompt}"
    );
    std::fs::remove_file(bridge.root.path().join("snapshot-prepared")).unwrap();
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Working on the second\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    prepare("Third step");
    prepare("Fourth step");
    let mut steps: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    steps[0]["sending_at"] = json!("2026-11-16T00:00:00Z");
    std::fs::write(&file, steps.to_string()).unwrap();
    bridge.inventory("done", "idle", "original");
    bridge.write("target.screen", "Second step done\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    bridge.write("target.screen", "Second step done, still\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let sent = sent_text(&bridge);
    assert!(!delivered(&bridge, &goal, "Third step"), "{sent}");
    assert!(!delivered(&bridge, &goal, "Fourth step"), "waits: {sent}");
    let held: Vec<Value> = bridge
        .ok(&["log", &goal])
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "prepared_held")
        .cloned()
        .collect();
    assert!(
        held.last().unwrap()["detail"]["reason"]
            .as_str()
            .unwrap()
            .contains("may not have arrived"),
        "{held:?}"
    );
}

#[test]
fn a_direct_send_is_admitted_under_the_lease_it_waits_for() {
    let bridge = Bridge::new();
    let goal = confirmed_goal(&bridge, "true", json!({"loop":{"max_assignments":2}}));
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "First step"]);
    let events = state_directory(&bridge)
        .join("goals")
        .join(format!("{goal}.events.ndjson"));
    let lock = hold_operation_lock(&bridge);
    let sending = bridge
        .command(&["send", "--goal", &goal, "--task", "work", "Second step"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let text = std::fs::read_to_string(&events).unwrap();
    let assigned = text
        .lines()
        .find(|line| line.contains("\"kind\":\"assigned\""))
        .unwrap()
        .to_string();
    std::fs::write(&events, format!("{text}{assigned}\n")).unwrap();
    drop(lock);
    let output = sending.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "admitted past the budget");
    assert!(said.contains("Assignment budget exhausted"), "{said}");
    assert!(!sent_text(&bridge).contains("Second step"));
}

#[test]
fn a_shared_goals_prepared_step_is_held_at_its_own_boundary() {
    let bridge = Bridge::new();
    let a = confirmed_goal(&bridge, "true", json!({}));
    std::fs::create_dir_all(bridge.workspace.join("secret")).unwrap();
    std::fs::write(bridge.workspace.join("secret/key.txt"), "v1").unwrap();
    let b = confirmed_goal(&bridge, "true", json!({"protected_paths":["secret"]}));
    bridge.ok(&["send", "--goal", &a, "--task", "work", "Work on A"]);
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing A\n› ");
    bridge.ok(&[
        "send",
        "--goal",
        &b,
        "--task",
        "work",
        "--prepare",
        "Work on B",
    ]);
    std::fs::write(bridge.workspace.join("secret/key.txt"), "v2").unwrap();
    bridge.inventory("done", "idle", "original");
    bridge.write("target.screen", "A done\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(
        !delivered(&bridge, &b, "Work on B"),
        "{}",
        sent_text(&bridge)
    );
    let held: Vec<Value> = bridge
        .ok(&["log", &b])
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "prepared_held")
        .cloned()
        .collect();
    assert!(
        held[0]["detail"]["reason"]
            .as_str()
            .unwrap()
            .contains("protected paths changed"),
        "{held:?}"
    );
}

#[test]
fn a_stop_condition_comes_before_a_native_goal_set_at_settle() {
    let worker = ClaudeWorker::new();
    let stop = worker.bridge.root.path().join("STOP");
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"native_goal":"settle",
            "stop_checks":[{"name":"stop-file","command":format!("test -e '{}'", stop.display())}]}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    std::fs::write(&stop, "").unwrap();
    worker.step("done", "Updated the copy\n❯ ");
    assert_eq!(worker.goal_lines(), 0, "{}", sent_text(&worker.bridge));
    assert!(worker.events(&goal, "stop_condition").len() == 1);
}

fn delivered(bridge: &Bridge, goal: &str, text: &str) -> bool {
    sent_text(bridge).contains(&format!(
        "w1:p1\nGoal {goal}, work, assigned to main.\n{text}"
    ))
}

#[test]
fn a_native_goal_is_not_carried_to_another_session_in_the_pane() {
    let (worker, a) = ClaudeWorker::with_native_goal(json!({}));
    worker.state("done", "Updated the copy\n❯ half-typed note");
    assert!(worker.run(&["pause", &a]).status.success());
    let origin =
        json!({"pane":"w1:p1","tab":"w1:t1","terminal":"original","agent":"codex","session":null})
            .to_string();
    let panes = json!([
        {"pane_id":"w1:p1","tab_id":"w1:t1","terminal_id":"original","agent":"codex","agent_status":"idle","focused":false},
        {"pane_id":"w1:p2","tab_id":"w1:t2","terminal_id":"manager","agent":"codex","agent_status":"idle","agent_session":{"value":"manager"},"focused":true}
    ]);
    worker
        .bridge
        .write("panes.json", &json!({"result":{"panes":panes}}).to_string());
    worker.bridge.write(
        "agents.json",
        &json!({"result":{"agents":panes}}).to_string(),
    );
    worker.bridge.write("target.screen", "Codex ready\n› ");
    let opened = worker
        .bridge
        .command(&["manager", "--agent", "codex"])
        .env("HGM_ORIGIN_JSON", &origin)
        .output()
        .unwrap();
    assert!(opened.status.success());
    let context = std::fs::read_to_string(worker.bridge.root.path().join("context")).unwrap();
    let run = |args: &[&str]| {
        worker
            .bridge
            .command(args)
            .env("HGM_CONTEXT", &context)
            .env("HGM_ORIGIN_JSON", &origin)
            .output()
            .unwrap()
    };
    let b = confirmed_goal(&worker.bridge, "true", json!({}));
    assert!(run(&["supervise", &b]).status.success());
    assert!(run(&["watch-loop", "--once"]).status.success());
    assert_eq!(worker.clears_typed(), 0, "{}", sent_text(&worker.bridge));
}

#[test]
fn a_stopped_goals_prepared_step_with_a_removed_checkout_does_not_end_the_watch() {
    let bridge = Bridge::new();
    let a = confirmed_goal(&bridge, "true", json!({}));
    std::fs::create_dir_all(bridge.workspace.join("secret")).unwrap();
    std::fs::write(bridge.workspace.join("secret/key.txt"), "v1").unwrap();
    let b = confirmed_goal(&bridge, "true", json!({"protected_paths":["secret"]}));
    bridge.ok(&["send", "--goal", &a, "--task", "work", "Work on A"]);
    bridge.inventory("working", "idle", "original");
    bridge.write("target.screen", "Editing A\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    bridge.ok(&[
        "send",
        "--goal",
        &b,
        "--task",
        "work",
        "--prepare",
        "Work on B",
    ]);
    bridge.ok(&["pause", &b]);
    let file = state_directory(&bridge)
        .join("goals")
        .join(format!("{b}.json"));
    let mut record: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    record["checkout"] = json!({"path": bridge.root.path().join("gone"), "branch":"b","base":"main",
        "workspace_id":"w9","removed_at":"2026-11-16T00:00:00Z"});
    std::fs::write(&file, record.to_string()).unwrap();
    bridge.inventory("done", "idle", "original");
    bridge.write("target.screen", "A done\n› ");
    let output = bridge.call(&["watch-loop", "--once"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(bridge.ok(&["watch"])["active"], true, "A is still watched");
    assert!(!delivered(&bridge, &b, "Work on B"));
}

#[test]
fn another_providers_check_runs_at_settle_and_wakes_the_manager_with_its_verdict() {
    let worker = ClaudeWorker::new();
    worker.bridge.write(
        "bin/codex",
        "#!/bin/sh\nout=; prev=\nfor a in \"$@\"; do [ \"$prev\" = -o ] && out=$a; prev=$a; done\nprintf '%s\\n' \"$@\" > \"$FIXTURE_ROOT/cross-args\"\ncat > \"$FIXTURE_ROOT/cross-prompt\"\nprintf 'VERDICT: changes_requested\\n- notes.txt misses world\\n' > \"$out\"\n",
    );
    let off = confirmed_goal(&worker.bridge, "true", json!({}));
    worker.ok(&["send", "--goal", &off, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    worker.step("done", "Updated the copy\n❯ ");
    assert!(worker.events(&off, "cross_check_started").is_empty());

    let worker = ClaudeWorker::new();
    let codex = worker.bridge.root.path().join("bin/codex");
    std::fs::write(
        &codex,
        "#!/bin/sh\nout=; prev=\nfor a in \"$@\"; do [ \"$prev\" = -o ] && out=$a; prev=$a; done\nprintf '%s\\n' \"$@\" > \"$FIXTURE_ROOT/cross-args\"\ncat > \"$FIXTURE_ROOT/cross-prompt\"\nprintf 'VERDICT: changes_requested\\n- notes.txt misses world\\n' > \"$out\"\n",
    )
    .unwrap();
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"cross_check":"auto"}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    worker.step("done", "Updated the copy\n❯ ");
    let started = worker.events(&goal, "cross_check_started");
    assert_eq!(started[0]["detail"]["reviewer"], "codex", "{started:?}");
    let mut checked = vec![];
    for _ in 0..30 {
        worker.step("done", "Updated the copy\n❯ ");
        checked = worker.events(&goal, "cross_checked");
        if !checked.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert_eq!(
        checked[0]["detail"]["verdict"], "changes_requested",
        "{checked:?}"
    );
    assert_eq!(
        checked[0]["detail"]["findings"][0],
        "- notes.txt misses world"
    );
    let args = std::fs::read_to_string(worker.bridge.root.path().join("cross-args")).unwrap();
    assert!(args.contains("read-only"), "{args}");
    let prompt = std::fs::read_to_string(worker.bridge.root.path().join("cross-prompt")).unwrap();
    assert!(
        prompt.contains("Copy updated") && prompt.contains("VERDICT"),
        "{prompt}"
    );
    assert!(
        sent_text(&worker.bridge).contains(&format!(
            "the codex check of main's work for goal {goal} task work says changes_requested"
        )),
        "{}",
        sent_text(&worker.bridge)
    );
    assert_eq!(
        worker.events(&goal, "task_completed").len(),
        0,
        "decides nothing"
    );
}

fn fake_codex(worker: &ClaudeWorker, body: &str) {
    std::fs::write(
        worker.bridge.root.path().join("bin/codex"),
        format!("#!/bin/sh\nout=; prev=\nfor a in \"$@\"; do [ \"$prev\" = -o ] && out=$a; prev=$a; done\nprintf '%s\\n' \"$@\" > \"$FIXTURE_ROOT/cross-args\"\ncat > \"$FIXTURE_ROOT/cross-prompt\"\n{body}\n"),
    )
    .unwrap();
}
fn step_until(worker: &ClaudeWorker, goal: &str, kind: &str) -> Vec<Value> {
    for _ in 0..30 {
        worker.step("done", "Updated the copy\n❯ ");
        let found = worker.events(goal, kind);
        if !found.is_empty() {
            return found;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    vec![]
}

#[test]
fn a_cross_check_runs_without_settle_checks_and_a_failed_reviewer_has_no_verdict() {
    let worker = ClaudeWorker::new();
    fake_codex(&worker, "printf 'VERDICT: approve\\n' > \"$out\"\nexit 3");
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"cross_check":"auto","check_on_settle":false},
               "tasks":[{"id":"work","instruction":"Update the login copy only","write_paths":["copy"]}]}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    let checked = step_until(&worker, &goal, "cross_checked");
    assert_eq!(checked[0]["detail"]["verdict"], "failed", "{checked:?}");
    let args = std::fs::read_to_string(worker.bridge.root.path().join("cross-args")).unwrap();
    assert!(args.contains("--skip-git-repo-check"), "{args}");
    let prompt = std::fs::read_to_string(worker.bridge.root.path().join("cross-prompt")).unwrap();
    for part in [
        "Update the login copy only",
        "- copy",
        "Update the copy",
        "not a Git repository",
    ] {
        assert!(prompt.contains(part), "{part:?} missing: {prompt}");
    }
    assert!(
        sent_text(&worker.bridge).contains(&format!("work for goal {goal} task work says failed")),
        "{}",
        sent_text(&worker.bridge)
    );
}

#[test]
fn a_cross_check_still_running_when_the_watch_ends_is_stopped_and_recorded() {
    let worker = ClaudeWorker::new();
    fake_codex(&worker, "sleep 30");
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"cross_check":"codex"}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    let started = step_until(&worker, &goal, "cross_check_started");
    assert_eq!(started.len(), 1);
    assert!(worker.run(&["pause", &goal]).status.success());
    worker.step("done", "Updated the copy\n❯ ");
    let checked = worker.events(&goal, "cross_checked");
    assert_eq!(checked[0]["detail"]["verdict"], "stopped", "{checked:?}");
}

#[test]
fn a_native_goal_the_watch_was_typing_when_it_stopped_is_never_typed_again() {
    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"native_goal":"settle"}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    let assigned = worker.events(&goal, "assigned")[0]["at"].clone();
    edit_watch(&worker.bridge, |w| {
        w["native_goal"] = json!({"goal": goal, "task": "work", "assigned_at": assigned,
            "set_at": chrono_now(), "outcome": "sending"});
    });
    worker.step("done", "Updated the copy\n❯ ");
    worker.step("done", "Updated the copy\n❯ ");
    assert_eq!(worker.goal_lines(), 0, "{}", sent_text(&worker.bridge));
    let set = worker.events(&goal, "native_goal_set");
    assert_eq!(set[0]["detail"]["delivery"], "uncertain", "{set:?}");
    assert_eq!(set[0]["detail"]["why"], "the watch stopped while typing it");
}

#[test]
fn a_linked_send_is_refused_once_a_stop_condition_holds() {
    let bridge = Bridge::new();
    let stop = bridge.root.path().join("STOP");
    let goal = confirmed_goal(
        &bridge,
        "true",
        json!({"loop":{"stop_checks":[{"name":"stop-file","command":format!("test -e '{}'", stop.display())}]}}),
    );
    bridge.ok(&["send", "--goal", &goal, "--task", "work", "First step"]);
    std::fs::write(&stop, "").unwrap();
    refused_with(
        &bridge,
        &["send", "--goal", &goal, "--task", "work", "Second step"],
        "stop condition stop-file holds; nothing was sent",
    );
    assert!(!sent_text(&bridge).contains("Second step"));
}

#[test]
fn a_dialog_on_the_final_read_defers_the_native_goal() {
    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"native_goal":"settle"}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing\n❯ ");
    let path = manager_dir(&worker.bridge).join("t-original-watch.json");
    worker
        .bridge
        .write("dialog-when-sending", path.to_str().unwrap());
    worker.bridge.write("dialog.screen", DIALOG_AFTER_READ);
    worker.step("done", "Finished\n❯ ");
    assert_eq!(worker.goal_lines(), 0);
    std::fs::remove_file(worker.bridge.root.path().join("dialog-when-sending")).unwrap();
    worker.step("done", "Finished\n❯ ");
    assert_eq!(worker.goal_lines(), 1);
}

#[test]
fn a_reserved_provider_continue_survives_a_watch_restart() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    let path = manager_dir(&bridge).join("t-original-watch.json");
    bridge.write("snapshot-prepared", path.to_str().unwrap());
    bridge.inventory("done", "idle", "original");
    bridge.write(
        "target.screen",
        "⎿  API Error: Can't reach the API server (ENOTFOUND)\n› ",
    );
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    std::fs::copy(bridge.root.path().join("prepared-at-prompt.json"), &path).unwrap();
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert_eq!(sent_text(&bridge).matches("w1:p1\nContinue").count(), 1);
}

#[test]
fn a_legacy_cross_check_needs_an_exit_record_to_confirm_termination() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    let dir = state_directory(&bridge).join("runs/cross-legacy");
    std::fs::create_dir(&dir).unwrap();
    edit_watch(&bridge, |w| {
        w["cross_check"] = json!({"id":"cross-legacy","goal":goal,
        "reviewer":"codex","started_at":1,"pid":u32::MAX})
    });
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let watch = bridge.ok(&["watch"]);
    assert_eq!(watch["cross_check"]["stop_unconfirmed"], true);
    assert!(!kinds(&bridge, &goal).iter().any(|k| k == "cross_checked"));
    std::fs::write(dir.join("exit"), "1\n").unwrap();
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    assert!(kinds(&bridge, &goal).iter().any(|k| k == "cross_checked"));
}

#[test]
fn assignments_require_an_open_goal_and_dependency_ready_task() {
    let bridge = Bridge::new();
    let goal = confirmed_goal(
        &bridge,
        "true",
        json!({"tasks":[
        {"id":"first","instruction":"First"},{"id":"second","instruction":"Second","depends_on":["first"]}]}),
    );
    let send = |task: &str, reason: &str| {
        refused_with(
            &bridge,
            &["send", "--goal", &goal, "--task", task, "Work"],
            reason,
        )
    };
    send("second", "unfinished dependencies");
    bridge.ok(&["complete", &goal, "first", "--evidence", "Finished"]);
    send("first", "is complete");
    bridge.ok(&["wait", &goal, "--reason", "Clarify"]);
    send("second", "waiting");
    assert!(sent_text(&bridge).is_empty());
}

#[test]
fn concurrent_role_changes_preserve_both_results() {
    let bridge = Bridge::new();
    start_role(&bridge, "impl", "w1:p5", "claude");
    start_role(&bridge, "reviewer", "w1:p6", "claude");
    std::thread::scope(|scope| {
        let first = scope.spawn(|| bridge.ok(&["role", "stop", "impl"]));
        bridge.ok(&["role", "stop", "reviewer"]);
        first.join().unwrap();
    });
    let roles = bridge.ok(&["role", "list"]);
    assert_eq!(roles.as_array().unwrap().len(), 1);
    assert_eq!(roles[0]["role"], "main");
}

#[test]
fn one_goals_stop_condition_keeps_its_neighbor_watched() {
    let bridge = Bridge::new();
    let first = confirmed_goal(&bridge, "true", json!({}));
    let second = confirmed_goal(
        &bridge,
        "true",
        json!({"loop":{"stop_checks":[{"name":"stop","command":"test -f STOP"}]}}),
    );
    for goal in [&first, &second] {
        bridge.ok(&["send", "--goal", goal, "--task", "work", "Begin"]);
    }
    std::fs::write(bridge.workspace.join("STOP"), "").unwrap();
    bridge.inventory("done", "idle", "original");
    bridge.write("target.screen", "Finished\n› ");
    assert!(bridge.call(&["watch-loop", "--once"]).status.success());
    let record = bridge.ok(&["watch"]);
    assert_eq!(record["active"], true);
    assert_eq!(record["goal"], first);
    assert!(!bridge.ok(&["show", &second])["waiting"].is_null());
}

#[test]
fn stop_checks_use_exit_status_and_surface_execution_errors() {
    let bridge = Bridge::new();
    for (command, expected) in [
        ("head -c 262145 /dev/zero", "stop condition boundary holds"),
        ("printf '\\377'", "stop condition boundary holds"),
        ("sleep 10", "timed out"),
    ] {
        let goal = confirmed_goal(
            &bridge,
            "true",
            json!({"loop":{"stop_checks":[{"name":"boundary","command":command,"timeout_secs":1}]}}),
        );
        refused_with(&bridge, &["send", "--goal", &goal, "Next step"], expected);
        assert!(!sent_text(&bridge).contains("Next step"));
    }
}

#[test]
fn releasing_a_watch_without_a_native_goal_waits_for_the_watch_writer() {
    let bridge = Bridge::new();
    let goal = bridge.goal();
    bridge.ok(&["supervise", &goal]);
    let lock = hold_operation_lock(&bridge);
    let file = manager_dir(&bridge).join("t-original-watch.json");
    let mut in_flight: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let mut pausing = bridge
        .command(&["pause", &goal])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while bridge.ok(&["show", &goal])["control"] != "pause" {
        assert!(
            std::time::Instant::now() < deadline,
            "pause never reached the goal"
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    in_flight["wakes"] = json!(7);
    std::fs::write(&file, in_flight.to_string()).unwrap();
    drop(lock);
    assert!(pausing.wait().unwrap().success());
    let watch: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(watch["active"], false);
    assert_eq!(watch["wakes"], 7);
}

#[test]
fn a_native_goal_is_saved_as_being_typed_before_it_is_typed() {
    let worker = ClaudeWorker::new();
    let goal = confirmed_goal(
        &worker.bridge,
        "true",
        json!({"loop":{"native_goal":"settle"}}),
    );
    worker.ok(&["send", "--goal", &goal, "--task", "work", "Update the copy"]);
    worker.step("working", "Editing the copy\n❯ ");
    let file = manager_dir(&worker.bridge).join("t-original-watch.json");
    worker
        .bridge
        .write("snapshot-prepared", file.to_str().unwrap());
    worker.step("done", "Updated the copy\n❯ ");
    assert_eq!(worker.goal_lines(), 1, "{}", sent_text(&worker.bridge));
    let at_prompt: Value = serde_json::from_slice(
        &std::fs::read(worker.bridge.root.path().join("prepared-at-prompt.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        at_prompt["native_goal"]["outcome"], "sending",
        "{at_prompt}"
    );
}
