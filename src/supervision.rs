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
    lines: Option<u32>,
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
    let pane = live["pane_id"]
        .as_str()
        .context("missing pane")?
        .to_string();
    let screen = if live.get("agent").is_some() {
        screen(&pane).await?
    } else {
        String::new()
    };
    crate::signals::apply_screen_state(&mut live, &screen);
    let mut result = json!({"binding":session.origin,"live":live,"screen":screen});
    if let Some(lines) = lines {
        let recent = crate::binding::recent_lines(&pane, lines).await?;
        result["lines"] = json!(recent.lines().count());
        result["screen"] = json!(recent);
    }
    if let Some(adopted) = adopted {
        result["adopted"] = adopted;
    }
    Ok(result)
}
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
pub(crate) const QUEUES_WHILE_WORKING: [&str; 2] = ["claude", "copilot"];
pub(crate) const DELIVERY_UNCONFIRMED: &str = "delivery not confirmed";
pub(crate) async fn input_consumed(pane: &str, agent: &str) -> Result<bool> {
    for _ in 0..6 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if empty_composer(&screen(pane).await?, agent) {
            return Ok(true);
        }
    }
    Ok(false)
}
async fn observe_recipient(store: &Store, helper: Option<&Target>) -> Result<(Value, String)> {
    let Some(helper) = helper else {
        let observed = target(store, None, None, None).await?;
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
pub(crate) async fn send(
    store: &Store,
    text: &str,
    assignment: Assignment<'_>,
    helper: Option<&Target>,
) -> Result<Value> {
    let goal = assignment.goal.map(String::from);
    let (command, to) = (assignment.kind.command(), assignment.to.to_string());
    let result = deliver(store, text, assignment, helper).await;
    if let (Err(error), Some(goal)) = (&result, goal.as_deref()) {
        let reason = format!("{error:#}");
        if !reason.contains(DELIVERY_UNCONFIRMED) && store.load(goal).is_ok() {
            let first: String = reason
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect();
            if let Err(error) = store.record(
                goal,
                "refused",
                json!({"command": command, "to": to, "reason": first}),
            ) {
                eprintln!("The refusal was not recorded: {error:#}");
            }
        }
    }
    result
}
async fn queue_native_goal(
    store: &Store,
    context: &Session,
    goal: &str,
    task: Option<&str>,
    pane: &str,
    agent: &str,
    by_watch: bool,
) -> Result<Option<crate::native_goal::NativeGoalState>> {
    let key = context.key();
    let Ok(watch) = load_watch(store, &key) else {
        return Ok(None);
    };
    if !watch.active
        || watch.native_unavailable
        || !watch.serves(goal)
        || watch.role != "main"
        || watch.origin.pane != pane
    {
        return Ok(None);
    }
    let goal = store.load(goal)?;
    let Some(state) =
        crate::native_goal::with_assignment(store, &goal, task, pane, agent, &key).await?
    else {
        return Ok(None);
    };
    if !by_watch {
        let mut watch = load_watch(store, &key)?;
        watch.native_goal = Some(state.clone());
        save_watch(store, &watch)?;
    }
    Ok(Some(state))
}
async fn deliver(
    store: &Store,
    text: &str,
    assignment: Assignment<'_>,
    helper: Option<&Target>,
) -> Result<Value> {
    ensure!(
        (assignment.kind == Delivery::Reanchor || !text.trim().is_empty()) && text.len() <= 32_768,
        "assignment must contain 1..32768 bytes"
    );
    let context = session(store)?;
    let key = context.key();
    let mut arming = match assignment.via {
        None => Some(wait_operation_lease(store, &key).await?),
        Some(_) => None,
    };
    let mut delivered = crate::admission::admit(store, &assignment, text)?;
    if assignment.kind == Delivery::Assignment
        && let Some(goal_id) = assignment.goal
    {
        let within_bounds = || -> Result<_> {
            let goal = store.load(goal_id)?;
            let events = store.events(goal_id)?;
            if let Some(reason) =
                crate::watch::boundary_reached(&goal, &events, Utc::now().timestamp())?
            {
                bail!(
                    "{reason}; nothing was sent. Decide with the user whether to revise the goal or stop"
                );
            }
            Ok(goal)
        };
        let goal = within_bounds()?;
        // Prepared delivery checked the predicate in its watch step. Direct
        // sends release the lease while user commands run (they may call hgm)
        // and recheck admission under the lease before typing.
        if arming.is_some() && !goal.spec.loop_policy.stop_checks.is_empty() {
            drop(arming.take());
            if let Some(check) = crate::watch::stop_check_holding(store, &goal).await? {
                bail!(
                    "stop condition {check} holds; nothing was sent. Decide with the user whether to revise the goal or stop"
                );
            }
            arming = Some(wait_operation_lease(store, &key).await?);
            delivered = crate::admission::admit(store, &assignment, text)?;
            within_bounds()?;
        }
    }
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
    let watch = load_watch(store, &context.key())
        .ok()
        .filter(|watch| watch.active);
    let (live, screen) = observe_recipient(store, helper).await?;
    let watched = match (&watch, assignment.goal) {
        (Some(w), Some(goal))
            if w.serves(goal)
                && w.role == assignment.to
                && w.origin.check(&live).is_ok()
                && live["pane_id"] == w.origin.pane =>
        {
            watch_running(store, w, Utc::now().timestamp())?
        }
        _ => false,
    };
    let agent = live["agent"].as_str().unwrap_or("");
    let working = live["agent_status"] == "working";
    let interrupting = working && (assignment.interrupt || assignment.kind != Delivery::Assignment);
    if interrupting {
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
    let scope = match (assignment.kind, assignment.goal, assignment.task) {
        (Delivery::Assignment, Some(goal), Some(task)) => {
            crate::scope::begin(store, &store.load(goal)?, task).err()
        }
        _ => None,
    };
    let receipt_path = path(store, &format!("send-{}", uuid::Uuid::new_v4()), ".json")?;
    store.write_private(
        &receipt_path,
        &json!({"status":"uncertain","pane":pane,"text":delivered,"at":Utc::now()}),
    )?;
    let response = herdr::json_query(&args(&["agent", "prompt", pane, &delivered]))
        .await
        .with_context(|| {
            format!(
                "{DELIVERY_UNCONFIRMED}; inspect target before retrying. Receipt: {}",
                receipt_path.display()
            )
        })?;
    let mut warnings = vec![];
    if let Some(error) = scope {
        warnings.push(format!(
            "the write scope snapshot failed, so writes outside write_paths are not compared for this assignment: {error:#}"
        ));
    }
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
    if let Some(goal) = assignment.goal
        && let Err(error) = store.record(
            goal,
            assignment.kind.event(),
            json!({"task": assignment.task, "text": text, "plan": assignment.plan, "to": assignment.to, "agent": agent, "pane": pane, "state": live["agent_status"], "seq": live["state_change_seq"], "interrupted": interrupting, "consumed": consumed, "receipt": receipt_path, "user_approval": assignment.user_approval, "via": assignment.via}),
        )
    {
        result["timeline_error"] = format!("{error:#}").into();
        unrecorded.push(format!("timeline: {error:#}"));
    }
    if assignment.kind == Delivery::Assignment {
        let key = context.key();
        let settled = match assignment.via {
            Some(_) => crate::prepared::remove(store, &key, assignment.to),
            None => crate::prepared::drop_steps(
                store,
                &key,
                assignment.to,
                None,
                "superseded by a direct send",
            )
            .map(|_| ()),
        };
        if let Err(error) = settled {
            warnings.push(format!(
                "the prepared step for {} was not cleared: {error:#}",
                assignment.to
            ));
        }
    }
    if let Some(goal) = assignment.goal
        && assignment.supervise
    {
        result["supervision"] = match &arming {
            _ if watched => "already_active".into(),
            None => "already_active".into(),
            Some(_operation) => {
                match arm(store, goal, assignment.to, helper.cloned(), None).await {
                    Ok(_) => "armed".into(),
                    Err(error) => format!("not armed: {error:#}").into(),
                }
            }
        };
    }
    if assignment.kind == Delivery::Assignment
        && !assignment.plan
        && assignment.to == "main"
        && let Some(goal) = assignment.goal
    {
        let by_watch = assignment.via.is_some();
        match queue_native_goal(store, &context, goal, assignment.task, pane, agent, by_watch).await {
            Ok(Some(state)) => result["native_goal"] = json!(state),
            Ok(None) => {}
            Err(error) => warnings.push(format!(
                "the native goal was not queued with the assignment ({error:#}); the watch sets it after the first settle"
            )),
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

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Pacing {
    pub idle_secs: Option<u64>,
    pub cooldown_secs: Option<u64>,
    pub max_wakes: Option<u32>,
    pub idle_notice_secs: Option<u64>,
    pub patrol_minutes: Option<u32>,
}
fn apply_pacing(store: &Store, watch: &mut Watch, goal: &str, pacing: Pacing) -> Result<Value> {
    let before = json!({
        "idle_secs": watch.idle_secs,
        "cooldown_secs": watch.cooldown_secs,
        "max_wakes": watch.max_wakes,
        "idle_notice_secs": watch.idle_notice_secs,
        "patrol_minutes": watch.patrol_minutes,
    });
    let mut applied = serde_json::Map::new();
    let mut not_applied = serde_json::Map::new();
    if let Some(secs) = pacing.idle_secs {
        watch.idle_secs = secs;
        applied.insert("idle_secs".into(), secs.into());
    }
    if let Some(secs) = pacing.cooldown_secs {
        watch.cooldown_secs = secs;
        applied.insert("cooldown_secs".into(), secs.into());
    }
    if let Some(secs) = pacing.idle_notice_secs {
        watch.idle_notice_secs = secs;
        applied.insert("idle_notice_secs".into(), secs.into());
    }
    if let Some(minutes) = pacing.patrol_minutes {
        watch.patrol_minutes = Some(minutes);
        applied.insert("patrol_minutes".into(), minutes.into());
    }
    if let Some(limit) = pacing.max_wakes {
        if limit > watch.wakes {
            watch.max_wakes = limit;
            applied.insert("max_wakes".into(), limit.into());
        } else {
            not_applied.insert(
                "max_wakes".into(),
                format!(
                    "{} wakes are already used; give more than that",
                    watch.wakes
                )
                .into(),
            );
        }
    }
    if !applied.is_empty() {
        save_watch(store, watch)?;
        store.record(
            goal,
            "watch_pacing",
            json!({"before": before, "applied": applied}),
        )?;
    }
    Ok(json!({"applied": applied, "not_applied": not_applied}))
}
pub(crate) async fn supervise(
    store: &Store,
    goal: &str,
    role: &str,
    helper: Option<Target>,
    pacing: Pacing,
) -> Result<(Watch, Value)> {
    let _operation = wait_operation_lease(store, &session(store)?.key()).await?;
    arm(store, goal, role, helper, Some(pacing)).await
}
async fn arm(
    store: &Store,
    goal: &str,
    role: &str,
    helper: Option<Target>,
    requested: Option<Pacing>,
) -> Result<(Watch, Value)> {
    let linked = requested.is_none();
    let pacing = requested.unwrap_or_default();
    let context = session(store)?;
    let key = context.key();
    let goal_state = store.load(goal)?;
    ensure!(
        !linked || crate::watch::short_of_completion(&goal_state),
        "goal stopped before supervision could start"
    );
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
    let mut previous = if watch_path(store, &key)?.exists() {
        Some(load_watch(store, &key)?)
    } else {
        None
    };
    if let Some(watch) = &mut previous {
        let running = watch.active && watch_running(store, watch, now)?;
        if linked
            && !running
            && watch.active
            && watch.serves(goal)
            && watch.role == role
            && watch.origin == origin
        {
            let mut watch = watch.clone();
            watch.manager = manager;
            watch.last_change = now;
            watch.last_observation_at = 0;
            let report = apply_pacing(store, &mut watch, goal, pacing)?;
            save_watch(store, &watch)?;
            open_monitor(store, &context).await?;
            return Ok((watch, report));
        }
        let joins = running && !watch.serves(goal) && watch.origin.pane == origin.pane;
        ensure!(
            !running || watch.serves(goal) || joins,
            "another goal is already supervised on another pane; wait/pause it first"
        );
        let retarget = watch.role != role || watch.origin != origin;
        if retarget
            && watch
                .native_goal
                .as_ref()
                .is_some_and(crate::native_goal::NativeGoalState::is_set)
        {
            ensure!(
                crate::native_goal::clear(store, watch, "changing the watched worker").await?,
                "the current worker's native goal could not be cleared; wait for its input to be free before switching roles"
            );
            save_watch(store, watch)?;
        }
        if running {
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
            if retarget {
                watch.native_goal = None;
                watch.native_unavailable = false;
                watch.prepared_after_seq = None;
                watch.continued_observation = None;
                watch.role = role.into();
                watch.origin = origin;
                watch.fingerprint.clear();
                watch.held = None;
                watch.held_cause = None;
                watch.same_wakes = 0;
                save_watch(store, &watch)?;
            }
            let report = apply_pacing(store, &mut watch, goal, pacing)?;
            return Ok((watch, report));
        }
        if watch.active && watch.members() != [goal] {
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
            if let Err(error) =
                herdr::query(&crate::watch_view::clear_args(&stale.origin.pane)).await
            {
                eprintln!("Could not clear the stale sidebar: {error:#}");
            }
        }
    }
    let config = crate::config::load(&store.workspace)?;
    let limit = pacing.max_wakes.unwrap_or(config.max_wakes());
    let same_worker = |w: &&Watch| {
        w.origin.pane == origin.pane
            && w.origin.terminal == origin.terminal
            && w.origin.agent == origin.agent
            && w.origin.session.is_some()
            && w.origin.session == origin.session
    };
    let same_session = previous.as_ref().filter(same_worker);
    let native_goal = same_session.and_then(|w| w.native_goal.clone());
    let cross_check = same_session.and_then(|w| w.cross_check.clone());
    let manager_goal = previous
        .as_ref()
        .and_then(|w| w.manager_goal.clone())
        .filter(|s| s.pane == manager.pane && s.session == manager.session);
    let watch = Watch {
        context: key.clone(),
        goal: goal.into(),
        goals: Vec::new(),
        spent: Default::default(),
        wake_anchor: String::new(),
        origin,
        role: role.into(),
        checked_through: None,
        reanchored_for: None,
        manager,
        active: true,
        idle_secs: pacing.idle_secs.unwrap_or(config.idle_secs()),
        cooldown_secs: pacing.cooldown_secs.unwrap_or(config.cooldown_secs()),
        max_wakes: limit,
        idle_notice_secs: pacing.idle_notice_secs.unwrap_or(config.idle_notice_secs()),
        patrol_minutes: pacing.patrol_minutes,
        pending_stop: false,
        pending_at: 0,
        idle_since: 0,
        idle_noticed: false,
        stuck: false,
        retry: None,
        prepared_held: None,
        native_goal,
        native_unavailable: same_session.is_some_and(|w| w.native_unavailable),
        manager_goal,
        cross_check,
        cross_through: None,
        cross_due: false,
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
        continued_observation: same_session.and_then(|w| w.continued_observation.clone()),
        prepared_after_seq: same_session.and_then(|w| w.prepared_after_seq),
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
    if !linked && goal_state.control == Control::Pause {
        store.update(goal, |g| g.request(Control::Run))?;
    }
    save_watch(store, &watch)?;
    open_monitor(store, &context).await?;
    let report = json!({
        "applied": {
            "idle_secs": watch.idle_secs,
            "cooldown_secs": watch.cooldown_secs,
            "max_wakes": watch.max_wakes,
            "idle_notice_secs": watch.idle_notice_secs,
            "patrol_minutes": watch.patrol_minutes,
        },
        "not_applied": {},
    });
    Ok((watch, report))
}

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
