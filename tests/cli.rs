use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Command, Output},
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(30);

struct Sandbox {
    root: tempfile::TempDir,
    workspace: PathBuf,
    state: PathBuf,
}
impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("work space 'quoted'");
        std::fs::create_dir(&workspace).unwrap();
        let state = root.path().join("state");
        Self {
            root,
            workspace,
            state,
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hgm"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("LANG", "C.UTF-8")
            .current_dir(&self.workspace)
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--state-dir")
            .arg(&self.state)
            .arg("--json")
            .args(args);
        command
    }
    fn call(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn success(&self, args: &[&str]) -> Value {
        let output = self.call(args);
        assert!(
            output.status.success(),
            "{}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn add(&self, spec: Value) -> String {
        std::fs::write(
            self.workspace.join("goal.yaml"),
            serde_json::to_vec(&spec).unwrap(),
        )
        .unwrap();
        self.success(&["add", "--file", "goal.yaml"])["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn show(&self, id: &str) -> Value {
        self.success(&["show", id])
    }
}

#[test]
fn manual_and_command_work_share_evidence_without_replaying_success() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({
        "objective":"Produce a reviewed report",
        "acceptance":["The report is correct and reviewed"],
        "runners":{"build":{"program":"sh","args":["-c","printf 'built\\n' >> report.txt"]}},
        "tasks":[
            {"id":"review","instruction":"Read report.txt","depends_on":["build"]},
            {"id":"build","instruction":"Produce report.txt","runner":"build"}
        ],
        "checks":[{"name":"content","command":"test -s report.txt"}]
    }));
    assert!(
        !sandbox
            .call(&["complete", &id, "review", "--evidence", "Reviewed"])
            .status
            .success()
    );
    let pending = sandbox.call(&["run", &id]);
    assert!(!pending.status.success());
    assert_eq!(sandbox.show(&id)["status"], "needs_input");
    sandbox.success(&[
        "complete",
        &id,
        "review",
        "--evidence",
        "Read report.txt and confirmed content",
    ]);
    let result = sandbox.success(&["run", &id]);
    assert_eq!(result["status"], "ready_for_review");
    assert_eq!(result["tasks"][1]["attempts"].as_array().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_to_string(sandbox.workspace.join("report.txt")).unwrap(),
        "built\n"
    );
    let result = sandbox.success(&["accept", &id, "--evidence", "All stated criteria checked"]);
    assert_eq!(result["status"], "completed");
    assert!(
        result["acceptance"]["note"]
            .as_str()
            .unwrap()
            .contains("criteria")
    );
    assert!(!sandbox.workspace.join(".git").exists());
    assert!(!sandbox.workspace.join(".ccswarm").exists());
    assert!(!sandbox.call(&["resume", &id]).status.success());
}

#[test]
fn failed_dependency_does_not_block_independent_work_and_resume_is_bounded() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({
        "objective":"Build dependent outputs", "acceptance":["Both outputs exist"],
        "runners":{
            "first":{"program":"sh","args":["-c","test -f allow && touch first"]},
            "second":{"program":"sh","args":["-c","test -f first && touch second"]},
            "independent":{"program":"sh","args":["-c","printf 'once\\n' >> independent"]}
        },
        "tasks":[
            {"id":"second","instruction":"Second","runner":"second","depends_on":["first"]},
            {"id":"first","instruction":"First","runner":"first"},
            {"id":"independent","instruction":"Independent","runner":"independent"}
        ],
        "checks":[{"name":"outputs","command":"test -f first && test -f second"}]
    }));
    assert!(
        !sandbox
            .call(&["run", &id, "--max-attempts", "1"])
            .status
            .success()
    );
    assert!(!sandbox.workspace.join("second").exists());
    assert!(sandbox.workspace.join("independent").exists());
    std::fs::write(sandbox.workspace.join("allow"), "").unwrap();
    assert!(
        !sandbox
            .call(&["resume", &id, "--max-attempts", "1"])
            .status
            .success()
    );
    let result = sandbox.success(&["resume", &id, "--max-attempts", "2"]);
    assert_eq!(result["status"], "ready_for_review");
    assert_eq!(result["tasks"][1]["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(
        std::fs::read_to_string(sandbox.workspace.join("independent")).unwrap(),
        "once\n"
    );
}

#[test]
fn repair_history_is_retained_and_acceptance_rechecks_current_state() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({
        "objective":"Create a checked artifact", "acceptance":["artifact exists"],
        "runners":{
            "work":{"program":"sh","args":["-c","true"]},
            "repair":{"program":"sh","args":["-c","touch artifact"]}
        },
        "repair":"repair",
        "tasks":[{"id":"work","instruction":"Do work","runner":"work"}],
        "checks":[{"name":"exists","command":"test -f artifact"}]
    }));
    let goal = sandbox.success(&["run", &id]);
    assert_eq!(goal["repairs"].as_array().unwrap().len(), 1);
    assert_eq!(goal["verifications"].as_array().unwrap().len(), 2);
    assert_eq!(
        goal["verifications"][0]["results"][0]["attempt"]["status"],
        "failed"
    );
    std::fs::remove_file(sandbox.workspace.join("artifact")).unwrap();
    assert!(
        !sandbox
            .call(&["accept", &id, "--evidence", "Looks fine"])
            .status
            .success()
    );
    assert_eq!(sandbox.show(&id)["status"], "needs_input");
    assert!(sandbox.show(&id)["acceptance"].is_null());
}

#[test]
fn planner_cannot_inject_a_command_or_unknown_runner() {
    let sandbox = Sandbox::new();
    for plan in [
        json!([{"id":"work","instruction":"Do work","runner":"unconfigured"}]),
        json!([{"id":"work","instruction":"Do work","runner":"configured","command":"true"}]),
    ] {
        let id = sandbox.add(json!({
        "objective":"Plan only trusted commands", "acceptance":["Only configured runners execute"],
        "planner":"planner",
        "runners":{"planner":{"program":"printf","args":["%s",plan.to_string()]},
                   "configured":{"program":"true"}}
    }));
        assert!(!sandbox.call(&["run", &id]).status.success());
        let goal = sandbox.show(&id);
        assert_eq!(goal["status"], "needs_input");
        assert_eq!(goal["plans"][0]["status"], "failed");
        assert!(goal["tasks"].as_array().unwrap().is_empty());
    }
    let invalid = sandbox.call(&["add", "Empty", "--criterion", ""]);
    assert!(!invalid.status.success());
}

#[test]
fn acceptance_after_wait_rechecks_the_result() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({"objective":"Review","acceptance":["Reviewed"],
        "checks":[{"name":"ready","command":"true"}]}));
    sandbox.success(&["complete", &id, "work", "--evidence", "Done"]);
    sandbox.success(&["run", &id]);
    sandbox.success(&["wait", &id, "--reason", "Review"]);
    let accepted = sandbox.success(&["accept", &id, "--evidence", "Reviewed"]);
    assert_eq!(accepted["status"], "completed");
    assert!(accepted["waiting"].is_null());
}

#[test]
fn project_prompt_files_resolve_inside_the_workspace() {
    let sandbox = Sandbox::new();
    let config = sandbox.workspace.join(".hgm");
    std::fs::create_dir(&config).unwrap();
    let external = sandbox.root.path().join("example.txt");
    std::fs::write(&external, "Fixture").unwrap();
    std::os::unix::fs::symlink(&external, config.join("guide.md")).unwrap();
    std::fs::write(
        config.join("config.yaml"),
        "prompts: {manager_append: [guide.md]}",
    )
    .unwrap();
    let output = sandbox.call(&["config"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("inside the workspace"));
}

#[test]
fn reports_keep_the_original_text_and_print_without_terminal_controls() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({"objective":"Review","acceptance":["Reviewed"]}));
    let report = "First line\nSecond\rline\u{7}.";
    sandbox.success(&["report", &id, report]);
    assert_eq!(
        sandbox.success(&["packet", &id])["reports"][0]["text"],
        report
    );
    for cmd in ["packet", "log"] {
        let out = sandbox.plain(&[cmd, &id]);
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.contains("Secondline."));
        assert!(!text.contains(['\r', '\u{7}']));
    }
}

#[test]
fn manual_goal_arguments_only_register_data() {
    let sandbox = Sandbox::new();
    assert!(
        !sandbox
            .call(&[
                "add",
                "Review",
                "--criterion",
                "Reviewed",
                "--check",
                "true"
            ])
            .status
            .success()
    );
    assert_eq!(sandbox.success(&["list"]), json!([]));
}

#[test]
fn configured_planner_drives_real_command_execution() {
    let sandbox = Sandbox::new();
    let plan =
        r#"[{"id":"build","instruction":"Create artifact","runner":"build","depends_on":[]}]"#;
    let id = sandbox.add(json!({
        "objective":"Create an artifact", "acceptance":["artifact exists"],
        "planner":"planner",
        "runners":{
            "planner":{"program":"printf","args":["%s",plan]},
            "build":{"program":"sh","args":["-c","test -n \"$HGM_PROMPT\" && touch artifact"]}
        },
        "checks":[{"name":"exists","command":"test -f artifact"}]
    }));
    let goal = sandbox.success(&["run", &id]);
    assert_eq!(goal["status"], "ready_for_review");
    assert_eq!(goal["plans"][0]["status"], "succeeded");
    assert!(sandbox.workspace.join("artifact").exists());
}

#[test]
fn cycles_are_rejected_without_persisting_a_goal() {
    let sandbox = Sandbox::new();
    std::fs::write(
        sandbox.workspace.join("cycle.yaml"),
        r#"objective: cycle
acceptance: [none]
tasks:
  - {id: a, instruction: A, depends_on: [b]}
  - {id: b, instruction: B, depends_on: [a]}
"#,
    )
    .unwrap();
    let result = sandbox.call(&["add", "--file", "cycle.yaml"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("cycle"));
    assert_eq!(sandbox.success(&["list"]), json!([]));
}

#[test]
fn shell_metacharacters_in_prompt_remain_data() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({
        "objective":"literal $(touch INJECTED) ; touch ALSO_INJECTED",
        "acceptance":["prompt is copied as data"],
        "runners":{"copy":{"program":"sh","args":["-c","printf '%s' \"$HGM_PROMPT\" > received.txt"]}},
        "tasks":[{"id":"copy","instruction":"Copy input","runner":"copy"}]
    }));
    sandbox.success(&["run", &id]);
    assert!(!sandbox.workspace.join("INJECTED").exists());
    assert!(!sandbox.workspace.join("ALSO_INJECTED").exists());
    assert!(
        std::fs::read_to_string(sandbox.workspace.join("received.txt"))
            .unwrap()
            .contains("$(touch INJECTED)")
    );
}

#[cfg(unix)]
#[test]
fn pause_stops_owned_processes_and_preserves_interrupted_attempt() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({
        "objective":"Interrupt controlled work", "acceptance":["work is observed"],
        "runners":{"slow":{"program":"sh","args":["-c","echo $$ > runner.pid; sleep 60"]}},
        "tasks":[{"id":"slow","instruction":"Wait","runner":"slow"}]
    }));
    let mut command = sandbox.command(&["run", &id]);
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + WAIT;
    while !sandbox.workspace.join("runner.pid").exists() {
        assert!(Instant::now() < deadline, "runner did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    let replaced = sandbox.call(&[
        "add",
        "--replace",
        &id,
        "Changed objective",
        "--criterion",
        "Changed",
    ]);
    assert!(!replaced.status.success(), "lost an active attempt");
    sandbox.success(&["pause", &id]);
    let deadline = Instant::now() + WAIT;
    let goal = loop {
        let goal = sandbox.show(&id);
        let exited = child.try_wait().unwrap().is_some();
        if exited && goal["status"] == "paused" {
            break goal;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "pause did not stop the run (exited: {exited}, status: {}): {}",
                goal["status"],
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(goal["tasks"][0]["attempts"][0]["status"], "interrupted");
    sandbox.success(&["cancel", &id]);
    assert!(!sandbox.call(&["resume", &id]).status.success());
}

#[test]
fn checks_timeout_and_record_the_failure() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({
        "objective":"Verify bounded checks", "acceptance":["checks terminate"],
        "runners":{"work":{"program":"true"}},
        "tasks":[{"id":"work","instruction":"Finish","runner":"work"}],
        "checks":[{"name":"slow","command":"sleep 30","timeout_secs":1}]
    }));
    let start = Instant::now();
    let result = sandbox.call(&["run", &id]);
    assert!(!result.status.success());
    assert!(start.elapsed() < Duration::from_secs(10));
    let goal = sandbox.show(&id);
    assert_eq!(
        goal["verifications"][0]["results"][0]["attempt"]["status"],
        "failed"
    );
    assert!(
        goal["verifications"][0]["results"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
}

#[test]
fn a_check_the_environment_denied_is_not_reported_as_a_check_result() {
    let sandbox = Sandbox::new();
    let denied = "echo 'error: failed to open: target/debug/.cargo-build-lock: Operation not permitted (os error 1)' >&2; exit 101";
    let goal = |checks: Value| {
        let id = sandbox.add(json!({
            "objective":"Build the report", "acceptance":["It builds"],
            "runners":{"work":{"program":"true"}},
            "tasks":[{"id":"work","instruction":"Finish","runner":"work"}],
            "checks":checks
        }));
        assert!(!sandbox.call(&["run", &id]).status.success());
        let goal = sandbox.show(&id);
        assert_eq!(goal["status"], "needs_input");
        goal["message"].as_str().unwrap().to_string()
    };
    let message = goal(json!([{"name":"build","command":denied}]));
    assert!(
        message.contains("environment denied check build access")
            && message.contains("not a check result")
            && !message.contains("fix the issue"),
        "{message}"
    );
    let message = goal(json!([
        {"name":"build","command":denied},
        {"name":"lint","command":"echo 'unused import' >&2; exit 1"}
    ]));
    assert!(
        message.contains("denied check build access")
            && message.contains("Check lint failed on its own")
            && message.contains("fix the issue"),
        "{message}"
    );
}

#[test]
fn brief_survives_reopening_without_creating_a_goal() {
    let sandbox = Sandbox::new();
    let brief = sandbox.workspace.join("brief.md");
    std::fs::write(
        &brief,
        "Situation: cannot send the proposal.\nNext: share three headings.",
    )
    .unwrap();
    sandbox.success(&["brief", "--save", brief.to_str().unwrap()]);
    let saved = sandbox.success(&["brief"]);
    assert!(
        saved["brief"]
            .as_str()
            .unwrap()
            .contains("share three headings")
    );
    assert_eq!(sandbox.success(&["list"]), json!([]));
    std::fs::write(&brief, "").unwrap();
    assert!(
        !sandbox
            .call(&["brief", "--save", brief.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(sandbox.success(&["brief"]), saved);
}

#[test]
fn status_explains_human_action_and_does_not_claim_acceptance() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({"objective":"Send a useful proposal","acceptance":["Recipient can choose an option"]}));
    let state = sandbox.success(&["status"]);
    assert_eq!(state[0]["id"], id);
    assert_eq!(state[0]["next_action"], "complete_human_work");
    sandbox.success(&[
        "complete",
        &id,
        "work",
        "--evidence",
        "Recipient reviewed the draft",
    ]);
    sandbox.success(&["run", &id]);
    let state = sandbox.success(&["status"]);
    assert_eq!(state[0]["next_action"], "review_and_accept");
    assert_ne!(state[0]["status"], "completed");
}

#[cfg(unix)]
#[test]
fn manager_launches_native_agent_in_workspace_with_context_and_literal_arguments() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    let bin = sandbox.root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let agent = bin.join("codex");
    std::fs::write(&agent, "#!/bin/sh\nprintf '%s\\n' \"$PWD\" > launched.cwd\nprintf '%s\\n' \"$@\" > launched.args\nprintf '%s\\n' \"$HGM_BIN\" > launched.bin\n").unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = sandbox
        .command(&["manager", "--agent", "codex"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let args = std::fs::read_to_string(sandbox.workspace.join("launched.args")).unwrap();
    assert!(args.contains("--add-dir"));
    assert!(args.contains("force a goal or execution onto a reflection-only conversation"));
    assert!(
        args.lines()
            .any(|s| s == "--dangerously-bypass-approvals-and-sandbox")
    );
    assert_eq!(
        std::fs::read_to_string(sandbox.workspace.join("launched.cwd"))
            .unwrap()
            .trim(),
        sandbox.workspace.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(sandbox.success(&["list"]), json!([]));
}

#[test]
fn review_survives_wait_and_revision_reopens_work_with_a_readable_timeline() {
    let sandbox = Sandbox::new();
    let id =
        sandbox.add(json!({"objective":"Summarize orders","acceptance":["Totals are correct"]}));
    sandbox.success(&[
        "complete",
        &id,
        "work",
        "--evidence",
        "Agent wrote summary.json",
    ]);
    let goal = sandbox.success(&["run", &id]);
    assert_eq!(goal["status"], "ready_for_review");
    assert!(
        goal["message"]
            .as_str()
            .unwrap()
            .contains("No command checks")
    );

    sandbox.success(&[
        "wait",
        &id,
        "--reason",
        "Ask the user to review summary.json",
    ]);
    let state = sandbox.success(&["status"]);
    assert_eq!(state[0]["status"], "ready_for_review");
    assert_eq!(state[0]["next_action"], "review_and_accept");
    assert_eq!(
        state[0]["waiting_for"],
        "Ask the user to review summary.json"
    );
    assert_eq!(sandbox.success(&["inbox"])[0]["kind"], "review_result");
    assert_eq!(sandbox.show(&id)["guidance"], json!([]));

    let revised = sandbox.success(&["revise", &id, "--feedback", "Totals must include tax"]);
    let lessons = sandbox.success(&["overview"])["lessons"].clone();
    assert!(
        lessons[0]
            .as_str()
            .unwrap()
            .contains("Totals must include tax"),
        "{lessons}"
    );
    assert_eq!(revised["status"], "ready");
    assert!(revised["tasks"][0]["manual_evidence"].is_null());
    assert_eq!(
        revised["tasks"][0]["superseded_evidence"][0]["note"],
        "Agent wrote summary.json"
    );
    sandbox.success(&["complete", &id, "work", "--evidence", "Agent added tax"]);
    sandbox.success(&["run", &id]);
    sandbox.success(&[
        "accept",
        &id,
        "--evidence",
        "User approved the taxed totals",
    ]);

    let kinds: Vec<String> = sandbox
        .success(&["log", &id])
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"].as_str().unwrap().into())
        .collect();
    let position = |kind: &str| {
        kinds
            .iter()
            .position(|k| k == kind)
            .unwrap_or_else(|| panic!("{kind} missing: {kinds:?}"))
    };
    assert!(position("registered") < position("task_completed"));
    assert!(position("task_completed") < position("waiting"));
    assert!(position("waiting") < position("task_reopened"));
    assert!(position("task_reopened") < position("guidance"));
    assert!(position("guidance") < position("accepted"));
    let text = sandbox.call(&["log", &id]);
    assert!(text.status.success());
}

#[test]
fn revision_refuses_command_tasks_and_finished_goals() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({
        "objective":"Build a report","acceptance":["The report exists"],
        "runners":{"build":{"program":"sh","args":["-c","true"]}},
        "tasks":[{"id":"build","instruction":"Build it","runner":"build"}]
    }));
    sandbox.success(&["run", &id]);
    let refused = sandbox.call(&["revise", &id, "--feedback", "redo", "--task", "build"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("configured command"));
    sandbox.success(&["accept", &id, "--evidence", "Reviewed the report"]);
    assert!(
        !sandbox
            .call(&["revise", &id, "--feedback", "redo"])
            .status
            .success()
    );
}

#[test]
fn loop_contract_keeps_spec_elements_and_advises_on_unverifiable_autonomy() {
    let sandbox = Sandbox::new();
    std::fs::write(
        sandbox.workspace.join("goal.json"),
        json!({
            "objective":"Export the report so finance can download it",
            "acceptance":["One CSV row per visible record"],
            "invariants":["The report API response shape is unchanged"],
            "out_of_scope":["packages/auth"],
            "on_boundary":"Stop and explain why auth seems to need changes",
            "notes":["See apps/orders/export.ts first"],
            "open_questions":[{"question":"Export disabled rows?","resolve_by":"Read the table filter","default":"Match the visible table"}],
            "loop":{"stop_when":["Auth changes are required"],"handoff_percent":70}
        })
        .to_string(),
    )
    .unwrap();
    let output = sandbox.call(&["add", "--file", "goal.json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let goal: Value = serde_json::from_slice(&output.stdout).unwrap();
    let spec = &goal["spec"];
    assert_eq!(
        spec["invariants"][0],
        "The report API response shape is unchanged"
    );
    assert_eq!(
        spec["open_questions"][0]["resolve_by"],
        "Read the table filter"
    );
    assert_eq!(spec["loop"]["stop_when"][0], "Auth changes are required");
    assert_eq!(spec["loop"]["handoff_percent"], 70);
    assert_eq!(spec["loop"]["max_assignments"], 3);

    std::fs::write(
        sandbox.workspace.join("bad.json"),
        json!({"objective":"x","acceptance":["y"],"loop":{"handoff_percent":99}}).to_string(),
    )
    .unwrap();
    let refused = sandbox.call(&["add", "--file", "bad.json"]);
    assert!(String::from_utf8_lossy(&refused.stderr).contains("handoff_percent"));
}

#[test]
fn overview_answers_startup_in_one_call() {
    let sandbox = Sandbox::new();
    let id = sandbox
        .add(json!({"objective":"Draft the memo","acceptance":["The recipient can decide"]}));
    let overview = sandbox.success(&["overview"]);
    assert_eq!(overview["brief"], "");
    assert_eq!(overview["goals"][0]["id"], id);
    assert_eq!(overview["needs_you"], json!([]));
    sandbox.success(&["complete", &id, "work", "--evidence", "Memo drafted"]);
    sandbox.success(&["run", &id]);
    let overview = sandbox.success(&["overview"]);
    assert_eq!(overview["needs_you"][0]["kind"], "review_result");
}

#[test]
fn a_cancelled_goal_leaves_the_inbox_despite_its_saved_wait() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({"objective":"Drop the idea","acceptance":["n/a"]}));
    sandbox.success(&["wait", &id, "--reason", "Ask whether to keep it"]);
    assert_eq!(sandbox.success(&["inbox"])[0]["kind"], "answer_manager");
    assert_eq!(
        sandbox.success(&["status"])[0]["waiting_for"],
        "Ask whether to keep it"
    );
    sandbox.success(&["cancel", &id]);
    let goal = sandbox.show(&id);
    assert_eq!(goal["status"], "cancelled");
    assert_eq!(goal["waiting"]["note"], "Ask whether to keep it");
    assert_eq!(sandbox.success(&["inbox"]), json!([]));
    assert_eq!(sandbox.success(&["overview"])["needs_you"], json!([]));
    assert!(sandbox.success(&["status"])[0]["waiting_for"].is_null());
    assert!(sandbox.success(&["overview"])["goals"][0]["waiting_for"].is_null());
}

#[test]
fn contracts_enforce_length_and_autonomy_within_verification() {
    let sandbox = Sandbox::new();
    let add = |spec: Value| {
        std::fs::write(sandbox.workspace.join("goal.json"), spec.to_string()).unwrap();
        sandbox.call(&["add", "--file", "goal.json"])
    };
    let refused = add(json!({"objective":"word ".repeat(900),"acceptance":["done"]}));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("budget 800"));
    let refused = add(json!({"objective":"仕様".repeat(900),"acceptance":["完了"]}));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("budget 800"));
    assert!(
        add(
            json!({"objective":"word ".repeat(900),"acceptance":["done"],
        "long_spec_reason":"Migration inventory must stay in one place"})
        )
        .status
        .success()
    );

    let auto = sandbox.add(json!({"objective":"Draft the memo","acceptance":["Readable"]}));
    let refused = sandbox.call(&["confirm", &auto, "--evidence", "User: go"]);
    assert!(String::from_utf8_lossy(&refused.stderr).contains("needs at least one command check"));
    let ask = sandbox.add(
        json!({"objective":"Draft the memo","acceptance":["Readable"],"loop":{"autonomy":"ask"}}),
    );
    sandbox.success(&["confirm", &ask, "--evidence", "User: go"]);
}

#[test]
fn a_plugin_folder_links_this_binary_and_leaves_other_builds_alone() {
    let sandbox = Sandbox::new();
    let setup = |folder: &std::path::Path| {
        sandbox
            .command(&["plugin-folder", "--dir", folder.to_str().unwrap()])
            .output()
            .unwrap()
    };
    let folder = sandbox.root.path().join("plugin folder");
    for _ in 0..2 {
        let output = setup(&folder);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let manifest = std::fs::read_to_string(folder.join("herdr-plugin.toml")).unwrap();
    assert!(manifest.contains("id = \"herdr-goal-manager\""));
    assert!(manifest.contains("./target/release/hgm"));
    let binary = folder.join("target/release/hgm");
    assert!(
        std::fs::symlink_metadata(&binary)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::canonicalize(&binary).unwrap(),
        std::fs::canonicalize(env!("CARGO_BIN_EXE_hgm")).unwrap()
    );
    let built = sandbox.root.path().join("built");
    std::fs::create_dir_all(built.join("target/release")).unwrap();
    std::fs::write(built.join("target/release/hgm"), "build").unwrap();
    assert!(!setup(&built).status.success());
    assert_eq!(
        std::fs::read_to_string(built.join("target/release/hgm")).unwrap(),
        "build"
    );
    let other = sandbox.root.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("herdr-plugin.toml"), "id = \"other\"\n").unwrap();
    assert!(!setup(&other).status.success());
    assert!(!other.join("target").exists());
}

const QUEUED: &str = "objective: Write summary.json\nacceptance: [summary.json has the totals]\nchecks:\n  - {name: summary, command: test -f summary.json}\n";

impl Sandbox {
    fn user_config(&self, yaml: &str) {
        let directory = self.root.path().join(".config/herdr-goal-manager");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("config.yaml"), yaml).unwrap();
    }
    fn plain(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hgm"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("LANG", "C.UTF-8")
            .current_dir(&self.workspace)
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--state-dir")
            .arg(&self.state)
            .args(args)
            .output()
            .unwrap()
    }
    fn contract(&self, name: &str, text: &str) -> String {
        let path = self.root.path().join(name);
        std::fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    }
    fn workspace_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<_> = std::fs::read_dir(&self.workspace)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        files.sort();
        files
    }
    fn queue_directories(&self) -> Vec<PathBuf> {
        let Ok(workspaces) = std::fs::read_dir(self.state.join("workspaces")) else {
            return vec![];
        };
        workspaces
            .map(|entry| entry.unwrap().path().join("queue"))
            .filter(|path| path.exists())
            .collect()
    }
}
fn refused(output: &Output, needle: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "expected a refusal: {stderr}");
    assert!(stderr.contains(needle), "{needle:?} not in: {stderr}");
}

#[test]
fn queue_commands_refuse_while_factory_mode_is_off() {
    let sandbox = Sandbox::new();
    let file = sandbox.contract("summary.yaml", QUEUED);
    let commands: [&[&str]; 6] = [
        &["queue", "add", "--file", &file, "--evidence", "Yes"],
        &["queue", "list"],
        &["queue", "park", "q-1"],
        &["queue", "unpark", "q-1"],
        &["queue", "remove", "q-1"],
        &["queue", "next"],
    ];
    for args in commands {
        refused(&sandbox.call(args), "factory.enabled");
    }
    sandbox.user_config("factory: {enabled: false}\n");
    refused(&sandbox.call(&["queue", "list"]), "factory.enabled");
    assert!(sandbox.queue_directories().is_empty());
}

#[test]
fn factory_settings_leave_existing_output_unchanged() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({"objective":"Review the report","acceptance":["Reviewed"]}));
    sandbox.success(&["note", &id, "Check the totals"]);
    let reads: [&[&str]; 7] = [
        &["status", "--all"],
        &["show", &id],
        &["inbox", "--all"],
        &["overview", "--all"],
        &["list", "--all"],
        &["log", &id],
        &["config", "--guide"],
    ];
    let capture = || -> Vec<(Vec<u8>, Vec<u8>)> {
        reads
            .iter()
            .map(|args| (sandbox.call(args).stdout, sandbox.plain(args).stdout))
            .collect()
    };
    let before = capture();
    assert!(sandbox.success(&["config"]).get("factory").is_none());
    sandbox.user_config("factory: {enabled: false, max_awaiting_review: 3}\n");
    assert!(
        capture() == before,
        "factory off changed an existing output"
    );
    sandbox.user_config("factory: {enabled: true}\n");
    let on = capture();
    assert!(
        on[..6] == before[..6],
        "an empty queue changed an existing output"
    );
    let guide = String::from_utf8(on[6].1.clone()).unwrap();
    let off_guide = String::from_utf8(before[6].1.clone()).unwrap();
    assert!(guide.starts_with(off_guide.trim_end()) && guide.contains("hgm queue next"));
}

#[test]
fn queue_add_list_park_unpark_and_remove_keep_history_in_private_state() {
    let sandbox = Sandbox::new();
    sandbox.user_config("factory: {enabled: true}\n");
    let file = sandbox.contract("summary.yaml", QUEUED);
    let before = sandbox.workspace_files();
    refused(
        &sandbox.call(&["queue", "add", "--file", &file, "--evidence", " "]),
        "--evidence",
    );
    let invalid = sandbox.contract("invalid.yaml", "objective: Missing acceptance\n");
    refused(
        &sandbox.call(&["queue", "add", "--file", &invalid, "--evidence", "Yes"]),
        "acceptance",
    );
    let unverifiable = sandbox.contract("auto.yaml", "objective: o\nacceptance: [a]\n");
    refused(
        &sandbox.call(&["queue", "add", "--file", &unverifiable, "--evidence", "Yes"]),
        "command check",
    );
    let item = sandbox.success(&[
        "queue",
        "add",
        "--file",
        &file,
        "--evidence",
        "Yes, run it after the review",
    ]);
    let id = item["id"].as_str().unwrap().to_string();
    assert_eq!(item["state"], "queued");
    assert_eq!(item["agreement"], "Yes, run it after the review");
    assert_eq!(item["agreement_age"], "0m");
    assert_eq!(item["objective"], "Write summary.json");
    let same = sandbox.contract(
        "same.yaml",
        "# the same contract\nacceptance:\n  - summary.json has the totals\nchecks: [{command: test -f summary.json, name: summary}]\nobjective: Write summary.json\n",
    );
    refused(
        &sandbox.call(&["queue", "add", "--file", &same, "--evidence", "Yes"]),
        &id,
    );
    let parked = sandbox.success(&["queue", "park", &id, "--reason", "Wait for the API"]);
    assert_eq!(parked["state"], "parked");
    let listed = sandbox.success(&["queue", "list"]);
    assert_eq!(listed[0]["state"], "parked");
    assert_eq!(
        listed[0]["history"][1]["detail"]["reason"],
        "Wait for the API"
    );
    refused(&sandbox.call(&["queue", "park", &id]), "is parked");
    assert_eq!(
        sandbox.success(&["queue", "unpark", &id])["state"],
        "queued"
    );
    let text = String::from_utf8(sandbox.plain(&["queue", "list"]).stdout).unwrap();
    assert!(
        text.contains("queued  agreed 0m ago  Write summary.json"),
        "{text}"
    );
    assert_eq!(
        sandbox.success(&["queue", "remove", &id])["state"],
        "removed"
    );
    assert_eq!(sandbox.success(&["queue", "list"]), json!([]));
    let all = sandbox.success(&["queue", "list", "--all"]);
    let kinds: Vec<_> = all[0]["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["kind"].clone())
        .collect();
    assert_eq!(kinds, ["added", "parked", "unparked", "removed"]);
    let again = sandbox.success(&["queue", "add", "--file", &same, "--evidence", "Yes, again"]);
    assert_ne!(again["id"], id);
    assert_eq!(
        sandbox.success(&["list", "--all"]),
        json!([]),
        "no goal yet"
    );
    assert_eq!(
        sandbox.workspace_files(),
        before,
        "nothing written into the workspace"
    );
    let queues = sandbox.queue_directories();
    assert_eq!(queues.len(), 1);
    assert!(queues[0].join("ledger.ndjson").is_file());
}

#[test]
fn a_project_may_switch_factory_mode_on_but_not_raise_its_limits() {
    let sandbox = Sandbox::new();
    let project = sandbox.workspace.join(".hgm");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("config.yaml"), "factory: {enabled: true}\n").unwrap();
    assert_eq!(sandbox.success(&["queue", "list"]), json!([]));
    std::fs::write(
        project.join("config.yaml"),
        "factory: {enabled: true, max_active: 2}\n",
    )
    .unwrap();
    refused(&sandbox.call(&["queue", "list"]), "factory.max_active");
}

fn passing_contract(objective: &str) -> String {
    format!(
        "objective: {objective}\nacceptance: [{objective} is done]\nchecks:\n  - {{name: ok, command: 'true'}}\n"
    )
}
fn row<'a>(rows: &'a Value, key: &str, value: &str) -> Option<&'a Value> {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row[key] == value)
}

#[test]
fn queue_next_dispatches_once_when_two_processes_claim_the_same_item() {
    let sandbox = Sandbox::new();
    sandbox.user_config("factory: {enabled: true, max_active: 5}\n");
    let file = sandbox.contract("one.yaml", &passing_contract("Only once"));
    let item = sandbox.success(&["queue", "add", "--file", &file, "--evidence", "Yes"]);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(sandbox.queue_directories()[0].join("claim.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let claimers: Vec<_> = (0..2)
        .map(|_| {
            sandbox
                .command(&["queue", "next"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    std::thread::sleep(Duration::from_millis(500));
    drop(lock);
    let outputs: Vec<Output> = claimers
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect();
    let winners: Vec<_> = outputs.iter().filter(|o| o.status.success()).collect();
    assert_eq!(winners.len(), 1, "{outputs:?}");
    let loser = outputs.iter().find(|o| !o.status.success()).unwrap();
    refused(loser, "no queued item");
    let goals = sandbox.success(&["list", "--all"]);
    assert_eq!(goals.as_array().unwrap().len(), 1);
    let dispatched: Value = serde_json::from_slice(&winners[0].stdout).unwrap();
    assert_eq!(dispatched["goal"]["id"], goals[0]["id"]);
    let history = &sandbox.success(&["queue", "list"])[0]["history"];
    let kinds: Vec<_> = history
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].clone())
        .collect();
    assert_eq!(
        kinds,
        ["added", "claimed", "dispatched"],
        "item {}",
        item["id"]
    );
}

#[test]
fn queue_next_stops_while_review_or_work_is_behind_and_says_why() {
    let sandbox = Sandbox::new();
    sandbox.user_config("factory: {enabled: true, max_awaiting_review: 1, max_active: 1}\n");
    for name in ["First", "Second", "Third"] {
        let file = sandbox.contract(&format!("{name}.yaml"), &passing_contract(name));
        sandbox.success(&[
            "queue",
            "add",
            "--file",
            &file,
            "--evidence",
            "Yes, queue it",
        ]);
    }
    let conversation = sandbox.add(json!({"objective":"Talk it through","acceptance":["Clear"]}));
    let overview = sandbox.success(&["overview", "--all"]);
    assert!(
        overview["queue"]
            .as_str()
            .unwrap()
            .contains("3 agreed contracts queued"),
        "{overview}"
    );
    let first = sandbox.success(&["queue", "next"]);
    let goal = first["goal"]["id"].as_str().unwrap().to_string();
    assert_eq!(first["item"]["state"], "dispatched");
    assert_eq!(first["item"]["objective"], "First");
    assert_eq!(first["item"]["agreement_age"], "0m");
    let confirmed = first["goal"]["confirmed"]["note"].as_str().unwrap();
    assert!(confirmed.contains("Yes, queue it") && confirmed.contains("Queued agreement"));
    assert_eq!(first["goal"]["status"], "ready");

    let held = sandbox.call(&["queue", "next"]);
    refused(&held, "factory.max_active (1)");
    refused(&held, &goal);
    let inbox = sandbox.success(&["inbox", "--all"]);
    let queue_row = row(&inbox, "kind", "queue_held").unwrap();
    assert!(queue_row["reason"].as_str().unwrap().contains(&goal));
    assert_eq!(queue_row["goals"], json!([goal]));
    let overview = sandbox.success(&["overview", "--all"]);
    assert_eq!(
        row(&overview["needs_you"], "kind", "queue_held"),
        Some(queue_row)
    );
    assert!(
        overview.get("queue").is_none(),
        "no dispatch line while held"
    );

    sandbox.success(&["complete", &goal, "work", "--evidence", "Written"]);
    sandbox.success(&["run", &goal]);
    refused(
        &sandbox.call(&["queue", "next"]),
        "factory.max_awaiting_review (1)",
    );
    let status = sandbox.success(&["status", "--all"]);
    assert_eq!(row(&status, "id", &goal).unwrap()["run_class"], Value::Null);
    assert!(
        row(&status, "id", &conversation)
            .unwrap()
            .get("run_class")
            .is_none()
    );

    sandbox.success(&["accept", &goal, "--evidence", "Reviewed the summary"]);
    let shown = sandbox.success(&["show", &goal]);
    assert_eq!(shown["factory"]["item"], first["item"]["id"]);
    assert_eq!(
        shown["factory"]["run_class"], "manual",
        "no agent assignment was sent"
    );
    let text = String::from_utf8(sandbox.plain(&["show", &goal]).stdout).unwrap();
    assert!(
        text.contains("Run class: manual (accepted without agent work)"),
        "{text}"
    );
    let text = String::from_utf8(sandbox.plain(&["status", "--all", "--finished"]).stdout).unwrap();
    assert!(text.contains(" · run: manual"), "{text}");
    let inbox = sandbox.success(&["inbox", "--all"]);
    assert!(row(&inbox, "kind", "queue_held").is_none());
    let overview = sandbox.success(&["overview", "--all"]);
    assert!(
        overview["queue"]
            .as_str()
            .unwrap()
            .contains("2 agreed contracts queued")
    );
    let second = sandbox.success(&["queue", "next"]);
    assert_eq!(second["item"]["objective"], "Second");
    assert_ne!(second["goal"]["id"], first["goal"]["id"]);
}

#[test]
fn a_check_that_passed_on_an_earlier_state_is_not_verified_in_show_and_the_packet() {
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.workspace.join("report.txt"), "draft").unwrap();
    let id = sandbox.add(json!({
        "objective":"Produce a report",
        "acceptance":["The report exists"],
        "tasks":[{"id":"write","instruction":"Write report.txt"}],
        "checks":[{"name":"content","command":"test -s report.txt"}]
    }));
    sandbox.success(&["complete", &id, "write", "--evidence", "Read report.txt"]);
    assert_eq!(sandbox.success(&["run", &id])["status"], "ready_for_review");
    let shown = sandbox.show(&id);
    assert!(shown.get("not_verified").is_none(), "{shown}");
    assert!(shown["verifications"][0]["fingerprint"].is_string());
    let packet = sandbox.success(&["packet", &id]);
    assert_eq!(packet["checks"][0]["state"], "current");
    assert_eq!(packet["not_verified"], json!([]));

    std::fs::write(sandbox.workspace.join("report.txt"), "final version").unwrap();
    let shown = sandbox.show(&id);
    let earlier = shown["not_verified"][0].as_str().unwrap();
    assert!(
        earlier.starts_with("check content passed at ")
            && earlier.ends_with(" on an earlier state of the work dir"),
        "{shown}"
    );
    let packet = sandbox.success(&["packet", &id]);
    assert_eq!(packet["checks"][0]["state"], "stale");
    assert_eq!(packet["not_verified"][0]["what"], earlier);
    let accepted = sandbox.success(&["accept", &id, "--evidence", "Read the final report"]);
    assert_eq!(accepted["status"], "completed");
    assert!(sandbox.show(&id).get("not_verified").is_none());
}

#[test]
fn a_goal_without_review_records_shows_and_lists_as_before() {
    let sandbox = Sandbox::new();
    let id = sandbox.add(json!({
        "objective":"Produce a report",
        "acceptance":["The report exists"],
        "tasks":[{"id":"write","instruction":"Write report.txt"}]
    }));
    let shown = sandbox.show(&id);
    for key in ["reviews", "not_verified", "factory", "native_goal"] {
        assert!(shown.get(key).is_none(), "{key}: {shown}");
    }
    assert!(shown["spec"]["tasks"][0].get("write_paths").is_none());
    assert!(shown["spec"]["loop"].get("require_review").is_none());
    let status = sandbox.success(&["status"]);
    assert!(status[0].get("review").is_none(), "{status}");
}

#[test]
fn factory_checkout_outside_git_dispatches_in_the_workspace_and_says_why() {
    let sandbox = Sandbox::new();
    sandbox.user_config("factory: {enabled: true, max_active: 3, checkout: true}\n");
    let contract = sandbox.contract("c.yaml", QUEUED);
    sandbox.success(&[
        "queue",
        "add",
        "--file",
        &contract,
        "--evidence",
        "User: yes",
    ]);
    let next = sandbox.success(&["queue", "next"]);
    assert!(next["goal"]["checkout"].is_null(), "{next}");
    let reason = next["checkout_skipped"].as_str().unwrap();
    assert!(
        reason.contains("factory.checkout is on, but")
            && reason.contains("is not a Git repository"),
        "{reason}"
    );
    let other = sandbox.contract("d.yaml", &QUEUED.replace("totals", "sums"));
    sandbox.success(&["queue", "add", "--file", &other, "--evidence", "User: yes"]);
    let text = sandbox.plain(&["queue", "next"]);
    assert!(text.status.success());
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(
        stdout.contains("No own checkout: factory.checkout is on"),
        "{stdout}"
    );
    assert!(!stdout.contains("Checkout: "), "{stdout}");
}

fn plain_in(sandbox: &Sandbox, tz: &str, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_hgm"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", sandbox.root.path())
        .env("LANG", "C.UTF-8")
        .env("TZ", tz)
        .current_dir(&sandbox.workspace)
        .arg("--workspace")
        .arg(&sandbox.workspace)
        .arg("--state-dir")
        .arg(&sandbox.state)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn human_readable_times_are_local_with_their_offset_and_json_stays_utc() {
    let sandbox = Sandbox::new();
    let goal = sandbox.add(json!({"objective":"o","acceptance":["a"]}));
    let log = plain_in(&sandbox, "Asia/Tokyo", &["log", &goal]);
    let first = log.lines().next().unwrap();
    assert!(first.contains(" +09:00 "), "{first}");
    let packet = plain_in(&sandbox, "Asia/Tokyo", &["packet", &goal]);
    assert!(!packet.contains(" UTC"), "{packet}");
    let events = sandbox.success(&["log", &goal]);
    assert!(events[0]["at"].as_str().unwrap().ends_with('Z'), "{events}");
}

#[test]
fn add_validate_reports_problems_and_words_per_field_without_registering() {
    let sandbox = Sandbox::new();
    let long = "word ".repeat(300);
    std::fs::write(
        sandbox.workspace.join("draft.yaml"),
        serde_json::to_vec(
            &json!({"objective":"o","acceptance":[long.clone(), long.clone()],
            "notes":[long], "checks":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    let output = sandbox.call(&["add", "--file", "draft.yaml", "--validate"]);
    assert!(!output.status.success(), "a problem fails the command");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["registered"], false);
    assert_eq!(report["valid"], false);
    assert_eq!(report["fields"]["acceptance"], 600);
    assert_eq!(report["fields"]["notes"], 300);
    let problems = report["problems"].to_string();
    assert!(
        problems.contains("by field: acceptance 600, notes 300"),
        "{problems}"
    );
    assert!(
        problems.contains("autonomy auto needs at least one command check"),
        "{problems}"
    );
    let text = sandbox.plain(&["add", "--file", "draft.yaml", "--validate"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains("Words by field: acceptance 600, notes 300"),
        "{text}"
    );
    let refused = sandbox.call(&["add", "--file", "draft.yaml"]);
    assert!(String::from_utf8_lossy(&refused.stderr).contains("by field: acceptance 600"));
    assert_eq!(sandbox.success(&["list"]).as_array().unwrap().len(), 0);
    std::fs::write(
        sandbox.workspace.join("ok.yaml"),
        "objective: o\nacceptance: [a]\nchecks: [{name: c, command: 'true'}]\n",
    )
    .unwrap();
    let ok = sandbox.success(&["add", "--file", "ok.yaml", "--validate"]);
    assert_eq!(
        (ok["valid"].clone(), ok["registered"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!(sandbox.success(&["list"]).as_array().unwrap().len(), 0);
}

#[test]
fn an_unconfirmed_goal_is_replaced_in_place_and_a_confirmed_one_never() {
    let sandbox = Sandbox::new();
    let goal = sandbox.add(json!({"objective":"First draft","acceptance":["a"],
        "checks":[{"name":"c","command":"true"}]}));
    std::fs::write(
        sandbox.workspace.join("fixed.yaml"),
        "objective: Corrected draft\nacceptance: [a, b]\nchecks: [{name: c, command: 'true'}]\ntasks: [{id: docs, instruction: Write the docs}]\n",
    )
    .unwrap();
    let replaced = sandbox.success(&["add", "--file", "fixed.yaml", "--replace", &goal]);
    assert_eq!(replaced["id"], goal.as_str());
    assert_eq!(replaced["spec"]["objective"], "Corrected draft");
    assert_eq!(replaced["tasks"][0]["spec"]["id"], "docs");
    let all = sandbox.success(&["list", "--finished"]);
    assert_eq!(all.as_array().unwrap().len(), 1);
    let events = sandbox.success(&["log", &goal]);
    assert!(
        events
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "contract_replaced")
    );
    sandbox.success(&["confirm", &goal, "--evidence", "User: yes"]);
    let refused = sandbox.call(&["add", "--file", "fixed.yaml", "--replace", &goal]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("is confirmed"));
    assert_eq!(sandbox.show(&goal)["spec"]["objective"], "Corrected draft");
}

#[test]
fn status_and_list_hide_finished_goals_and_say_how_many() {
    let sandbox = Sandbox::new();
    let open = sandbox.add(json!({"objective":"Still open","acceptance":["a"]}));
    let done = sandbox.add(json!({"objective":"Dropped draft","acceptance":["a"]}));
    sandbox.success(&["cancel", &done]);
    for command in ["status", "list"] {
        let text = String::from_utf8(sandbox.plain(&[command]).stdout).unwrap();
        assert!(
            text.contains("Still open") && !text.contains("Dropped draft"),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "(1 completed or cancelled goal(s) hidden; hgm {command} --finished shows them)"
            )),
            "{text}"
        );
        let all = String::from_utf8(sandbox.plain(&[command, "--finished"]).stdout).unwrap();
        assert!(
            all.contains("Dropped draft") && !all.contains("hidden"),
            "{all}"
        );
        assert_eq!(sandbox.success(&[command]).as_array().unwrap().len(), 2);
    }
    let _ = open;
}

#[test]
fn add_confirm_complete_and_run_print_a_short_result_and_confirm_warns_on_a_shared_tree() {
    let sandbox = Sandbox::new();
    std::fs::write(
        sandbox.workspace.join("goal.yaml"),
        "objective: Short output\nacceptance: [a]\nchecks: [{name: c, command: 'true'}]\n",
    )
    .unwrap();
    let lines = |args: &[&str]| -> String {
        let output = sandbox.plain(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let added = lines(&["add", "--file", "goal.yaml"]);
    let id = added.split_whitespace().next().unwrap().to_string();
    assert!(added.lines().count() <= 3, "{added}");
    assert!(
        added.contains(&format!("hgm confirm {id} --evidence")),
        "{added}"
    );
    let confirmed = lines(&["confirm", &id, "--evidence", "User: go"]);
    assert!(
        confirmed.contains("hgm send --goal") && confirmed.lines().count() <= 3,
        "{confirmed}"
    );
    let completed = lines(&["complete", &id, "work", "--evidence", "Done by hand"]);
    assert!(
        completed.contains(&format!("hgm run {id}")) && completed.lines().count() <= 3,
        "{completed}"
    );
    let ran = lines(&["run", &id]);
    assert!(
        ran.contains("ready_for_review") && ran.contains("hgm packet"),
        "{ran}"
    );
    assert!(ran.lines().count() <= 3, "{ran}");
    let verbose = lines(&["show", &id]);
    assert!(verbose.contains("AC1: a"));
    let full = lines(&["--verbose", "add", "--file", "goal.yaml"]);
    assert!(full.contains("AC1: a"), "{full}");
    assert!(sandbox.success(&["show", &id])["spec"].is_object());

    let other = sandbox.add(json!({"objective":"Same tree","acceptance":["a"],
        "checks":[{"name":"c","command":"true"}]}));
    let output = sandbox.plain(&["confirm", &other, "--evidence", "User: go"]);
    assert!(output.status.success());
    let warning = String::from_utf8_lossy(&output.stderr);
    assert!(
        warning.contains(&id) && warning.contains("checkout"),
        "{warning}"
    );
}

#[test]
fn the_workspace_attention_summary_lists_each_open_goal() {
    let sandbox = Sandbox::new();
    sandbox.add(json!({"objective":"First goal","acceptance":["a"]}));
    let second = sandbox.add(json!({"objective":"Second goal","acceptance":["a"]}));
    sandbox.success(&["cancel", &second]);
    let text = String::from_utf8(sandbox.plain(&["attention"]).stdout).unwrap();
    assert!(text.contains("First goal — ready"), "{text}");
    assert!(
        text.contains("0 wakes (0 followed by an action) · idle 0s, longest 0s · 0 notices to the person · 0 correction(s) · 0 refused command(s)"),
        "{text}"
    );
    assert!(
        text.contains(
            "(1 completed or cancelled goal(s) hidden; hgm attention --finished shows them)"
        ),
        "{text}"
    );
    assert_eq!(sandbox.success(&["attention"]).as_array().unwrap().len(), 2);
}
