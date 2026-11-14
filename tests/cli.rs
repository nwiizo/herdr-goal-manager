//! Real CLI user flows in private non-Git workspaces, with no provider credentials.
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Command, Output},
    time::{Duration, Instant},
};

/// How long a test waits for a state another process records; slow CI hosts
/// must not turn a late state into a failure.
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
    let malicious = r#"[{"id":"bad","instruction":"Do work","runner":"unconfigured"}]"#;
    let id = sandbox.add(json!({
        "objective":"Plan only trusted commands", "acceptance":["Only configured runners execute"],
        "planner":"planner",
        "runners":{"planner":{"program":"printf","args":["%s",malicious]}}
    }));
    assert!(!sandbox.call(&["run", &id]).status.success());
    let goal = sandbox.show(&id);
    assert_eq!(goal["status"], "needs_input");
    assert_eq!(goal["plans"][0]["status"], "failed");
    assert!(goal["tasks"].as_array().unwrap().is_empty());
    let invalid = sandbox.call(&["add", "Empty", "--criterion", ""]);
    assert!(!invalid.status.success());
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
    sandbox.success(&["pause", &id]);
    // The run notices the pause on its own poll, records it, and exits.
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
    assert!(args.contains("Do not force a goal"));
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
    sandbox.success(&["cancel", &id]);
    let goal = sandbox.show(&id);
    assert_eq!(goal["status"], "cancelled");
    assert_eq!(goal["waiting"]["note"], "Ask whether to keep it");
    assert_eq!(sandbox.success(&["inbox"]), json!([]));
    assert_eq!(sandbox.success(&["overview"])["needs_you"], json!([]));
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
    // An hgm installed on its own (from crates.io) becomes a linkable plugin.
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
    // A folder holding an hgm build, or another plugin, is left alone.
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
