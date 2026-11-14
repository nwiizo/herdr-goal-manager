//! Origin-bound dispatch and arming of the watch; no automatic task completion.
pub(crate) use crate::binding::{
    Target, args, capture_origin, live_pane, operation_lease, origin_terminal, path, screen,
    session, wait_operation_lease,
};
pub(crate) use crate::signals::{can_prompt, refusal};
use crate::{
    binding::{SESSION_CHANGED, Session},
    herdr_cli as herdr,
    model::{Control, GoalStatus},
    signals::{Refusal, empty_composer, input_refusal},
    store::Store,
    watch::{Watch, end_watch, load_watch, save_watch, watch_path, watch_running},
};
use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use serde_json::{Value, json};
pub(crate) async fn target(
    store: &Store,
    start: Option<&str>,
    adopt: Option<&str>,
) -> Result<Value> {
    let mut session = session(store)?;
    let origin = session
        .origin
        .clone()
        .context("no original pane; open the manager using the Herdr shortcut")?;
    let mut live = live_pane(&origin.pane).await?;
    let adopted = match adopt {
        Some(evidence) => adopt_session(store, &mut session, &live, evidence)?,
        None => None,
    };
    let origin = session.origin.as_ref().unwrap_or(&origin);
    check_main(origin, &live)?;
    if let Some(provider) = start {
        ensure!(
            live.get("agent").is_none(),
            "original pane already contains an agent"
        );
        let name = format!("hgm-{}", &session.id[2..10]);
        herdr::query(&args(&[
            "agent",
            "start",
            &name,
            "--kind",
            provider,
            "--pane",
            &origin.pane,
        ]))
        .await?;
        live = live_pane(&origin.pane).await?;
        session.origin = Some(Target::from_value(&live)?);
        store.write_private(&path(store, &session.id, ".json")?, &session)?;
    }
    if session.origin.as_ref().is_some_and(|o| o.session.is_none())
        && live.get("agent_session").is_some()
    {
        session.origin = Some(Target::from_value(&live)?);
        store.write_private(&path(store, &session.id, ".json")?, &session)?;
    }
    let screen = if live.get("agent").is_some() {
        screen(live["pane_id"].as_str().context("missing pane")?).await?
    } else {
        String::new()
    };
    crate::signals::apply_screen_state(&mut live, &screen);
    let mut result = json!({"binding":session.origin,"live":live,"screen":screen});
    if let Some(adopted) = adopted {
        result["adopted"] = adopted;
    }
    Ok(result)
}
/// Checks the pinned original agent. A new session in its tab (the user
/// restarted it) gets a pointer to adoption instead of a bare refusal.
fn check_main(origin: &Target, live: &Value) -> Result<()> {
    origin.check(live).map_err(|error| {
        if error.to_string() == SESSION_CHANGED {
            error.context(
                "the original tab runs a new agent session; if it continues this tab's work (the user restarted it), run hgm target --adopt EVIDENCE",
            )
        } else {
            error
        }
    })
}
/// Rebinds the pinned tab to the agent session now running in it, when the
/// user restarted the original agent outside `hgm restart`. The terminal and
/// tab must be the pinned ones and no handoff may be in flight. The fresh
/// session gets a goal check; `evidence` records why it continues the work.
fn adopt_session(
    store: &Store,
    session: &mut Session,
    live: &Value,
    evidence: &str,
) -> Result<Option<Value>> {
    ensure!(
        !evidence.trim().is_empty(),
        "--adopt needs evidence that the new session continues this tab's work"
    );
    let bound = session.origin.clone().context("no original pane")?;
    ensure!(
        live["terminal_id"].as_str() == Some(&bound.terminal)
            && live["tab_id"].as_str() == Some(&bound.tab),
        "bound terminal moved or was replaced; reopen Goal Manager from the intended tab"
    );
    let current = Target::from_value(live)?;
    ensure!(
        current.agent.is_some() && current.session.is_some(),
        "the original tab shows no agent session to adopt yet"
    );
    if current.agent == bound.agent && current.session == bound.session {
        return Ok(None);
    }
    let key = session.key();
    let _operation = operation_lease(store, &key)?;
    let transfer_path = path(store, &key, "-handoff.json")?;
    if transfer_path.exists() {
        let transfer: Value = serde_json::from_slice(&std::fs::read(transfer_path)?)?;
        ensure!(
            transfer["phase"] == "complete",
            "a handoff restart is in progress; advance it with hgm restart instead"
        );
    }
    session.origin = Some(current.clone());
    store.write_private(&path(store, &session.id, ".json")?, session)?;
    let goal = crate::watch::rebind_watch(
        store,
        &key,
        &current,
        json!({"via": "adopt", "previous": bound.session, "agent": current.agent, "evidence": evidence}),
    )?;
    Ok(Some(
        json!({"previous": bound.session, "session": current.session, "agent": current.agent, "goal": goal}),
    ))
}
pub(crate) use crate::admission::{Assignment, Delivery};
/// Providers observed to queue input typed while they work: Claude Code reads
/// it at its next step, Copilot (1.0.110) lists it above its input line and
/// runs it after the current step. Others are added only after a native check.
const QUEUES_WHILE_WORKING: [&str; 2] = ["claude", "copilot"];
/// Whether the recipient took the text out of its input line: the composer is
/// empty again shortly after delivery. Text left there is never resent.
async fn input_consumed(pane: &str, agent: &str) -> Result<bool> {
    for _ in 0..6 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if empty_composer(&screen(pane).await?, agent) {
            return Ok(true);
        }
    }
    Ok(false)
}
/// The recipient's live state and screen: the pinned original agent (`None`)
/// or a helper role's target.
async fn observe_recipient(store: &Store, helper: Option<&Target>) -> Result<(Value, String)> {
    let Some(helper) = helper else {
        let observed = target(store, None, None).await?;
        let screen = observed["screen"]
            .as_str()
            .context("missing target screen")?
            .to_string();
        return Ok((observed["live"].clone(), screen));
    };
    let live = live_pane(&helper.pane).await?;
    helper.check(&live)?;
    let screen = screen(&helper.pane).await?;
    let mut live = live;
    crate::signals::apply_screen_state(&mut live, &screen);
    Ok((live, screen))
}
/// Sends to `main` (the pinned original agent) or to a helper role's target.
pub(crate) async fn send(
    store: &Store,
    text: &str,
    assignment: Assignment<'_>,
    helper: Option<&Target>,
) -> Result<Value> {
    ensure!(
        (assignment.kind == Delivery::Reanchor || !text.trim().is_empty()) && text.len() <= 32_768,
        "assignment must contain 1..32768 bytes"
    );
    let delivered = crate::admission::admit(store, &assignment, text)?;
    let context = session(store)?;
    let transfer_path = path(store, &context.key(), "-handoff.json")?;
    if helper.is_none() && transfer_path.exists() {
        let transfer: Value = serde_json::from_slice(&std::fs::read(transfer_path)?)?;
        ensure!(
            transfer["phase"] == "complete",
            "handoff is in progress; wait for verified adoption before assigning work"
        );
    }
    // Every lock the delivery needs is taken before anything is typed: once the
    // text leaves the input, no failure may invite a resend.
    let arming = match assignment.goal {
        Some(goal) if assignment.supervise => {
            let active = load_watch(store, &context.key()).is_ok_and(|watch| {
                watch.active && watch.serves(goal) && watch.role == assignment.to
            });
            if active {
                None
            } else {
                let config = crate::config::load(&store.workspace)?;
                let pacing = (
                    config.idle_secs(),
                    config.cooldown_secs(),
                    config.max_wakes(),
                );
                Some((pacing, wait_operation_lease(store, &context.key()).await?))
            }
        }
        _ => None,
    };
    let (live, screen) = observe_recipient(store, helper).await?;
    let agent = live["agent"].as_str().unwrap_or("");
    let working = live["agent_status"] == "working";
    let interrupting = working && (assignment.interrupt || assignment.kind != Delivery::Assignment);
    if interrupting {
        // The provider reads input typed during a turn at its next step. It
        // still never goes into a draft, a dialog, or an unknown screen.
        ensure!(
            QUEUES_WHILE_WORKING.contains(&agent),
            "{} ({agent}) is working and is not known to queue input typed during a turn; send when its turn settles",
            assignment.to
        );
        if let Some(refusal) = input_refusal(&live, &screen) {
            bail!("{}; nothing was typed", refusal.explain(assignment.to));
        }
    } else if let Some(refusal) = refusal(&live, &screen) {
        let hint = if refusal == Refusal::Working {
            ", or queue a linked assignment with send --interrupt (for an agent that reads input during a turn)"
        } else {
            ""
        };
        bail!(
            "{}{hint}; nothing was typed",
            refusal.explain(assignment.to)
        );
    }
    let pane = live["pane_id"].as_str().context("missing pane")?;
    let receipt_path = path(store, &format!("send-{}", uuid::Uuid::new_v4()), ".json")?;
    store.write_private(
        &receipt_path,
        &json!({"status":"uncertain","pane":pane,"text":delivered,"at":Utc::now()}),
    )?;
    let response = herdr::json_query(&args(&["agent", "prompt", pane, &delivered]))
        .await
        .with_context(|| {
            format!(
                "delivery not confirmed; inspect target before retrying. Receipt: {}",
                receipt_path.display()
            )
        })?;
    // The text is delivered: from here on, failures are reported in the result
    // and never returned, so nobody resends what the recipient already has.
    let mut warnings = vec![];
    let consumed = input_consumed(pane, agent).await.unwrap_or_else(|error| {
        warnings.push(format!("could not confirm the input was taken: {error:#}"));
        false
    });
    if !consumed {
        warnings.push("the text may still sit in the recipient's input; inspect it".into());
    }
    let mut unrecorded = vec![];
    if let Err(error) = store.write_private(
        &receipt_path,
        &json!({"status":"admitted","pane":pane,"text":delivered,"at":Utc::now(),"response":response,"consumed":consumed}),
    ) {
        unrecorded.push(format!("receipt: {error:#}"));
    }
    let mut result = json!({"status":"admitted","to":assignment.to,"interrupted":interrupting,"consumed":consumed,"receipt":receipt_path,"message":"Admission is not completion. Inspect the result when the goal wakes you."});
    if let Some(goal) = assignment.goal {
        if let Err(error) = store.record(
            goal,
            assignment.kind.event(),
            json!({"task": assignment.task, "text": text, "plan": assignment.plan, "to": assignment.to, "agent": agent, "pane": pane, "state": live["agent_status"], "seq": live["state_change_seq"], "interrupted": interrupting, "consumed": consumed, "receipt": receipt_path, "user_approval": assignment.user_approval}),
        ) {
            result["timeline_error"] = format!("{error:#}").into();
            unrecorded.push(format!("timeline: {error:#}"));
        }
        if assignment.supervise {
            result["supervision"] = match arming {
                None => "already_active".into(),
                Some((pacing, _operation)) => {
                    match arm(store, goal, assignment.to, helper.cloned(), pacing).await {
                        Ok(_) => "armed".into(),
                        Err(error) => format!("not armed: {error:#}").into(),
                    }
                }
            };
        }
    }
    if !unrecorded.is_empty() {
        warnings.push(format!(
            "{} received the text, but hgm could not record it ({})",
            assignment.to,
            unrecorded.join("; ")
        ));
    }
    if !warnings.is_empty() {
        result["warning"] = format!("{}; do not resend", warnings.join("; ")).into();
    }
    Ok(result)
}

/// Watches `role` (`main`, or a helper whose target is `helper`) for `goal`.
/// One watch per manager context: a linked send to another role retargets it.
pub(crate) async fn supervise(
    store: &Store,
    goal: &str,
    role: &str,
    helper: Option<Target>,
    idle: u64,
    cooldown: u64,
    limit: u32,
) -> Result<Watch> {
    let _operation = wait_operation_lease(store, &session(store)?.key()).await?;
    arm(store, goal, role, helper, (idle, cooldown, limit)).await
}
/// Arms the watch; the caller holds the supervision lock.
async fn arm(
    store: &Store,
    goal: &str,
    role: &str,
    helper: Option<Target>,
    (idle, cooldown, limit): (u64, u64, u32),
) -> Result<Watch> {
    let context = session(store)?;
    let key = context.key();
    let goal_state = store.load(goal)?;
    ensure!(
        !matches!(
            goal_state.status,
            GoalStatus::Completed | GoalStatus::Cancelled | GoalStatus::ReadyForReview
        ),
        "goal has no executable work to supervise"
    );
    let origin = match helper {
        Some(target) => {
            target.check(&live_pane(&target.pane).await?)?;
            target
        }
        None => {
            let origin = context.origin.clone().context("no original pane")?;
            check_main(&origin, &live_pane(&origin.pane).await?)?;
            origin
        }
    };
    let manager_pane = std::env::var("HERDR_PANE_ID").context("manager pane is missing")?;
    ensure!(
        manager_pane != origin.pane,
        "manager and target must use different panes"
    );
    let manager = Target::from_value(&live_pane(&manager_pane).await?)?;
    ensure!(
        manager.agent.is_some(),
        "supervise must be called by the native manager agent"
    );
    let now = Utc::now().timestamp();
    let previous = if watch_path(store, &key)?.exists() {
        Some(load_watch(store, &key)?)
    } else {
        None
    };
    if let Some(watch) = &previous {
        // One watch per tab: a running (or just armed) watch is reused, so a
        // second Goal watch tab never opens for it. Another goal whose work
        // goes to the watched pane joins it; wakes follow the latest assignment.
        let running = watch.active && watch_running(store, watch, now)?;
        let joins = running && !watch.serves(goal) && watch.origin.pane == origin.pane;
        if running && (watch.serves(goal) || joins) {
            let mut watch = watch.clone();
            if joins {
                watch.join(goal);
                save_watch(store, &watch)?;
                store.record(
                    goal,
                    "watch_joined",
                    json!({"pane": watch.origin.pane, "goals": watch.members()}),
                )?;
            }
            if watch.role != role {
                watch.role = role.into();
                watch.origin = origin;
                watch.fingerprint.clear();
                watch.held = None;
                watch.held_cause = None;
                watch.same_wakes = 0;
                save_watch(store, &watch)?;
            }
            return Ok(watch);
        }
        ensure!(
            !running,
            "another goal is already supervised on another pane; wait/pause it first"
        );
        if watch.active && watch.members() != [goal] {
            // An active record with no watch process holding its lease is stale
            // (its tab closed or the server restarted): end it for every goal
            // it served and continue.
            let mut stale = watch.clone();
            for member in stale.members() {
                if member != goal && member != stale.goal {
                    store.record(
                        &member,
                        "watch_ended",
                        json!({"reason": "stale_recovered", "notify": false}),
                    )?;
                }
            }
            end_watch(store, &mut stale, "stale_recovered", false);
            save_watch(store, &stale)?;
            // No process keeps the stale watch's sidebar current any more.
            if let Err(error) =
                herdr::query(&crate::watch_view::clear_args(&stale.origin.pane)).await
            {
                eprintln!("Could not clear the stale sidebar: {error:#}");
            }
        }
    }
    let watch = Watch {
        context: key.clone(),
        goal: goal.into(),
        goals: Vec::new(),
        spent: Default::default(),
        origin,
        role: role.into(),
        checked_through: None,
        reanchored_for: None,
        manager,
        active: true,
        idle_secs: idle,
        cooldown_secs: cooldown,
        max_wakes: limit,
        wakes: previous
            .as_ref()
            .filter(|w| w.goal == goal)
            .map_or(0, |w| w.wakes),
        last_sent: previous
            .as_ref()
            .filter(|w| w.goal == goal)
            .map_or(0, |w| w.last_sent),
        last_change: now,
        fingerprint: String::new(),
        pending: None,
        status: "watching".into(),
        handoff_due: false,
        last_patrol: now,
        rot_signals: 0,
        held: None,
        held_cause: None,
        wake_fingerprint: String::new(),
        same_wakes: 0,
        manager_unavailable_since: 0,
        manager_notified: false,
        rot_evidence: vec![],
        mode: String::new(),
        last_observation_at: 0,
    };
    ensure!(
        watch.wakes < limit,
        "wake budget exhausted; inspect results before explicitly increasing --max-wakes"
    );
    if goal_state.control == Control::Pause {
        store.update(goal, |g| g.request(Control::Run))?;
    }
    save_watch(store, &watch)?;
    open_monitor(store, &context).await?;
    Ok(watch)
}

/// Opens the Goal watch tab for `context`. Call it only when no watch process
/// runs for the tab: an earlier Goal watch tab then only shows its end, and is
/// closed so the tab keeps one Goal watch.
pub(crate) async fn open_monitor(store: &Store, context: &Session) -> Result<()> {
    crate::monitor::close_ended_monitor(store, &context.key()).await;
    let workspace = std::env::var("HERDR_WORKSPACE_ID").context("workspace ID missing")?;
    let parameters = vec![
        "plugin".into(),
        "pane".into(),
        "open".into(),
        "--plugin".into(),
        "herdr-goal-manager".into(),
        "--entrypoint".into(),
        "monitor".into(),
        "--placement".into(),
        "tab".into(),
        "--workspace".into(),
        workspace,
        "--env".into(),
        format!("HGM_WORKSPACE={}", store.workspace.display()),
        "--env".into(),
        format!("HGM_STATE_DIR={}", store.root.display()),
        "--env".into(),
        format!("HGM_CONTEXT={}", context.id),
        "--no-focus".into(),
    ];
    if let Err(error) = herdr::query(&parameters).await {
        return Err(error).context(
            "watch saved but monitor launch unconfirmed; inspect Herdr tabs, run `herdr plugin link` on the current checkout if the plugin was updated, or use hgm watch-loop",
        );
    }
    Ok(())
}
