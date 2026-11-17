//! Persistent watch driver coordinating observation and handoff.
//!
//! Herdr's event stream wakes the driver when the worker's state changes or
//! attention text appears; every observation reads the screen, so a dialog
//! reported as `done` is still caught. A reconcile every `watch.reconcile_secs`
//! keeps the time-based checks running, reads a working worker's screen, and
//! notices dropped events (the state sequence moved without an event) and a
//! closed pane (closing a tab sends no event). When the stream fails the
//! driver polls every 10 seconds with a full re-read and resubscribes later.
use crate::{
    herdr_cli as herdr,
    herdr_events::{self, Signal, Subscription},
    store::Store,
    supervision::*,
    watch::*,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::{IsTerminal, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Polling interval when Herdr events are unavailable.
const POLL: Duration = Duration::from_secs(10);
/// How often the event wait reads the local watch record, so an ended record
/// (pause, cancel, wait) ends the watch promptly without any Herdr call.
const RECORD_CHECK: Duration = Duration::from_secs(2);
/// How often polling tries to subscribe again.
const RESUBSCRIBE: Duration = Duration::from_secs(60);
/// One step of the Goal watch tab's sheep: about four columns a second.
const SHEEP_STEP: Duration = Duration::from_millis(250);
/// Rows above the sheep kept for its speech bubble.
const BUBBLE_ROWS: usize = 4;
/// How long the sheep repeats a notification or a wake.
const NOTICE_SECS: i64 = 600;

/// How the driver runs.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LoopOptions {
    /// One observation, then exit (no subscription).
    pub once: bool,
    /// Never subscribe: the polling baseline, for measurement.
    pub polling: bool,
    /// Exit after this long (measurement runs and tests).
    pub run_for: Option<Duration>,
}

/// An unattended loop stops when the host idles into sleep. On macOS the watch
/// holds `caffeinate -i` (idle sleep only; lid close and display sleep still
/// apply), which exits with this process. `HGM_KEEP_AWAKE=0` opts out.
fn keep_awake() -> Option<Child> {
    if !cfg!(target_os = "macos") || std::env::var("HGM_KEEP_AWAKE").as_deref() == Ok("0") {
        return None;
    }
    let pid = std::process::id().to_string();
    Command::new("/usr/bin/caffeinate")
        .args(["-i", "-w", &pid])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .inspect_err(|error| eprintln!("Could not keep the host awake: {error}"))
        .ok()
}

/// The executable this watch runs from, so a redeploy (a new build renamed
/// over it) does not end the watch.
struct Build {
    path: PathBuf,
    identity: Option<(u64, u64, i64)>,
}
impl Build {
    fn current() -> Option<Self> {
        let path = std::env::current_exe().ok()?.canonicalize().ok()?;
        let identity = file_identity(&path);
        Some(Self { path, identity })
    }
    /// Whether another file now sits at the path; each replacement counts once.
    fn replaced(&mut self) -> bool {
        let now = file_identity(&self.path);
        let replaced = now.is_some() && now != self.identity;
        if replaced {
            self.identity = now;
        }
        replaced
    }
}
fn file_identity(path: &std::path::Path) -> Option<(u64, u64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.dev(), metadata.ino(), metadata.mtime()))
}
/// Continues this watch in the new build with the same arguments; the saved
/// watch state carries over and the new process takes the lease again. A build
/// that does not run keeps the watch on the old one.
fn reload(store: &Store, key: &str, build: &Build) {
    use std::os::unix::process::CommandExt;
    let runs = Command::new(&build.path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !runs {
        eprintln!("The replaced hgm does not run; the watch stays on its build.");
        return;
    }
    if let Ok(watch) = load_watch(store, key)
        && let Err(error) =
            store.record(&watch.goal, "watch_reloaded", json!({"build": build.path}))
    {
        eprintln!("Could not record the reload: {error:#}");
    }
    let error = Command::new(&build.path)
        .args(std::env::args_os().skip(1))
        .exec();
    eprintln!("Could not continue the watch in the new build: {error}");
}

fn monitor_path(store: &Store, key: &str) -> Result<PathBuf> {
    path(store, key, "-monitor.json")
}
/// Closes the Goal watch tab the previous watch of `key` left on its end view
/// (it waits for Enter), so starting a new watch keeps one Goal watch tab.
/// Call it only when no watch process runs for `key`. A tab that no longer
/// carries the Goal watch label is left alone.
pub(crate) async fn close_ended_monitor(store: &Store, key: &str) {
    let Ok(path) = monitor_path(store, key) else {
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    if let Err(error) = std::fs::remove_file(&path) {
        eprintln!("Could not clear the previous watch tab record: {error}");
    }
    let Some(tab) = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|record| record["tab"].as_str().map(str::to_owned))
    else {
        return;
    };
    let label = herdr::json_query(&args(&["tab", "get", &tab]))
        .await
        .ok()
        .and_then(|info| info["result"]["tab"]["label"].as_str().map(str::to_owned));
    if label.as_deref() == Some(crate::herdr::WATCH_TAB)
        && let Err(error) = herdr::query(&args(&["tab", "close", &tab])).await
    {
        eprintln!("Could not close the ended Goal watch tab {tab}: {error:#}");
    }
}

/// Why the driver observes the worker now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Trigger {
    Start,
    /// A state change or attention output reported by Herdr.
    Event,
    /// Herdr reported that the worker's pane exited or closed.
    Gone,
    /// The periodic reconcile while events drive the watch.
    Reconcile,
    /// A polling tick, or the first read after events dropped.
    Poll,
}

/// The event subscriptions for one worker pane.
struct Events {
    socket: PathBuf,
    pane: String,
    state: Subscription,
    /// One-shot in Herdr 0.9.5; re-armed after each state change.
    output: Option<Subscription>,
}
impl Events {
    async fn open(socket: PathBuf, pane: &str) -> Result<Self> {
        let state =
            herdr_events::subscribe(&socket, pane, herdr_events::state_subscriptions(pane)).await?;
        let mut events = Self {
            socket,
            pane: pane.into(),
            state,
            output: None,
        };
        events.arm_output().await;
        Ok(events)
    }
    /// A failed output subscription only delays attention text until the
    /// next observation; the state stream stays authoritative.
    async fn arm_output(&mut self) {
        let pattern = crate::signals::attention_pattern();
        self.output = herdr_events::subscribe(
            &self.socket,
            &self.pane,
            herdr_events::output_subscription(&self.pane, &pattern),
        )
        .await
        .inspect_err(|error| eprintln!("Attention output is not subscribed: {error:#}"))
        .ok();
    }
    /// Waits for the next signal or `timeout`. `Err` means the state stream
    /// failed or ended: events are no longer trustworthy.
    async fn next(&mut self, timeout: Duration) -> Result<Option<Signal>> {
        let output = async {
            match self.output.as_mut() {
                Some(output) => output.next().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            signal = self.state.next() => match signal? {
                Some(signal) => Ok(Some(signal)),
                None => anyhow::bail!("Herdr closed the event stream"),
            },
            signal = output => match signal {
                Ok(Some(signal)) => {
                    self.output = None;
                    Ok(Some(signal))
                }
                // The output stream ended or failed: wait for the next state change.
                _ => {
                    self.output = None;
                    Ok(None)
                }
            },
            () = tokio::time::sleep(timeout) => Ok(None),
        }
    }
}

/// Herdr calls and elapsed time per worker state, for comparing event
/// watching with polling (`<context>-watch-stats.json`).
#[derive(Debug, Default)]
struct Stats {
    since: Option<Instant>,
    calls_at: u64,
    state: String,
    totals: serde_json::Map<String, Value>,
}
impl Stats {
    /// Charges the time and calls since the previous observation to the
    /// state seen then, and starts a new interval in `state`.
    fn observe(&mut self, state: &str) {
        let calls = herdr::calls();
        if let Some(since) = self.since {
            let entry = self
                .totals
                .entry(self.state.clone())
                .or_insert_with(|| json!({"secs": 0.0, "calls": 0}));
            entry["secs"] =
                json!(entry["secs"].as_f64().unwrap_or(0.0) + since.elapsed().as_secs_f64());
            entry["calls"] = json!(entry["calls"].as_u64().unwrap_or(0) + calls - self.calls_at);
        }
        self.since = Some(Instant::now());
        self.calls_at = calls;
        self.state = if state == "working" {
            "working"
        } else {
            "settled"
        }
        .into();
    }
    fn save(&self, store: &Store, context: &str, mode: &str) -> Result<()> {
        store.write_private(
            &path(store, context, "-watch-stats.json")?,
            &json!({"mode": mode, "by_state": self.totals, "calls": herdr::calls()}),
        )
    }
}

pub(crate) async fn watch_loop(store: &Store, options: LoopOptions) -> Result<()> {
    let mut display = Display {
        terminal: !options.once && std::io::stdout().is_terminal(),
        ..Display::default()
    };
    let result = drive(store, options, &mut display).await;
    // Native goals the watch set may still be set when its input was busy.
    drain(store, options.once || options.run_for.is_some()).await;
    // Only a running watch keeps the tab and sidebar current: show how it
    // ended and clear the sidebar so it never shows a stale state.
    display
        .finish(store, !options.once && options.run_for.is_none())
        .await;
    result
}

/// How long an ended watch keeps trying to clear the native goals it set.
const DRAIN: Duration = Duration::from_secs(1800);

/// After the watch ends, clears the native goals it set that may still be
/// set: main's, and the Goal Manager's once no confirmed goal of the tab is
/// in progress. A draft, a dialog or an unknown input line defers a clear
/// (nothing is typed into them), and a stop (wait, pause, cancel) ends the
/// watch at once, so this retries every [`POLL`] for up to [`DRAIN`] and then
/// tells the user what may still be set. A one-off run tries once.
async fn drain(store: &Store, once: bool) {
    let deadline = Instant::now() + DRAIN;
    loop {
        let (goal, left) = match drain_once(store).await {
            Ok(Some(found)) => found,
            Ok(None) => return,
            Err(error) => {
                eprintln!("The native goals the watch set were not cleared: {error:#}");
                return;
            }
        };
        if once {
            return;
        }
        if Instant::now() >= deadline {
            let reason = format!(
                "{} may still have the /goal hgm set: its input held a draft, a dialog or an unknown line for {} minutes after the goal stopped, so hgm typed nothing; type /goal clear there",
                left.join(" and "),
                DRAIN.as_secs() / 60
            );
            if let Err(error) = store.record(
                &goal,
                "attention",
                json!({"cause": "native_goal_left_set", "reason": reason}),
            ) {
                eprintln!("Could not tell the user: {error:#}");
            }
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}
/// One attempt of [`drain`], holding the watch and operation leases. Returns
/// the watch's goal and who may still have a goal set, or `None` when nothing
/// is left or another watch runs for the tab (it decides from then on).
async fn drain_once(store: &Store) -> Result<Option<(String, Vec<&'static str>)>> {
    let key = session(store)?.key();
    if !watch_path(store, &key)?.exists() {
        return Ok(None);
    }
    // The operation lease first: `supervise` holds it while it starts a new
    // watch, which must then find the watch lease free.
    let _operation = crate::binding::wait_operation_lease(store, &key).await?;
    let lease = watch_lease(store, &key)?;
    if lease.try_lock().is_err() {
        return Ok(None);
    }
    let mut watch = load_watch(store, &key)?;
    if watch.active {
        return Ok(None);
    }
    settle_interrupted(store, &mut watch, chrono::Utc::now().timestamp())?;
    let mut left = vec![];
    // Each clear on its own: a closed main pane must not keep the Goal
    // Manager's goal set.
    match crate::native_goal::clear(store, &mut watch, "the watch ended").await {
        Ok(true) => {}
        Ok(false) => left.push("main"),
        Err(error) => eprintln!("Main's native goal was not cleared: {error:#}"),
    }
    match crate::manager_goal::after_watch(store, &mut watch).await {
        Ok(true) => left.push("the Goal Manager"),
        Ok(false) => {}
        Err(error) => eprintln!("The Goal Manager's native goal was not cleared: {error:#}"),
    }
    // A check still running is stopped; a finished one is recorded.
    crate::cross_check::poll(store, &mut watch, chrono::Utc::now().timestamp(), true)?;
    save_watch(store, &watch)?;
    Ok((!left.is_empty()).then(|| (watch.goal.clone(), left)))
}

async fn drive(store: &Store, options: LoopOptions, display: &mut Display) -> Result<()> {
    let context = session(store)?;
    let lock_path = path(store, &context.key(), "-watch.lock")?;
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    if lease.try_lock().is_err() {
        return Ok(());
    }
    let once = options.once;
    let _awake = if once { None } else { keep_awake() };
    // Only a continuing watch owns its tab: `--once` runs from any pane (the
    // manager's, a shell) and leaves that tab's name alone.
    if !once && let Ok(tab) = std::env::var("HERDR_TAB_ID") {
        herdr::query(&args(&["tab", "rename", &tab, crate::herdr::WATCH_TAB])).await?;
        // Where this watch shows itself, so the next watch of the same tab can
        // close this one once it has ended (see `close_ended_monitor`).
        store.write_private(&monitor_path(store, &context.key())?, &json!({"tab": tab}))?;
    }
    let reconcile = Duration::from_secs(crate::config::load(&store.workspace)?.reconcile_secs());
    let deadline = options.run_for.map(|span| Instant::now() + span);
    let socket = context.socket.clone().map(PathBuf::from);
    let mut events: Option<Events> = None;
    let mut next_subscribe = Instant::now();
    let mut trigger = Trigger::Start;
    let mut last: Option<(Value, String)> = None;
    let mut stats = Stats::default();
    // Consecutive failed observations; see `retry`.
    let mut failures = 0;
    let mut build = if once { None } else { Build::current() };
    loop {
        if deadline.is_some_and(|end| Instant::now() >= end) {
            return Ok(());
        }
        if let Some(current) = build.as_mut()
            && current.replaced()
        {
            // Only returns when the new build cannot take over.
            reload(store, &context.key(), current);
        }
        let operation = match operation_lease(store, &context.key()) {
            Ok(lock) => lock,
            Err(error) => {
                if once {
                    return Err(error);
                }
                tokio::time::sleep(POLL).await;
                continue;
            }
        };
        let mut watch = load_watch(store, &context.key())?;
        ensure!(
            watch.status != "wake_delivery_uncertain",
            "previous wake admission is unknown; inspect the manager before rearming"
        );
        // A handoff in progress continues even when the watch has ended (the
        // goal finished or paused meanwhile): the original tab must not be left
        // without its agent.
        let transfer = crate::handoff::inspect(store)?;
        if !matches!(transfer["phase"].as_str(), Some("not_started" | "complete")) {
            drop(operation);
            crate::handoff::advance(store).await?;
            if once {
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
            continue;
        }
        if !watch.active {
            return Ok(());
        }
        // Goals that stopped leave the watch; wakes follow the pane's latest assignment.
        if !refocus(store, &mut watch)? {
            save_watch(store, &watch)?;
            return Ok(());
        }
        let goal = store.load(&watch.goal)?;
        // A retargeted watch (another role) needs its own subscription.
        if events
            .as_ref()
            .is_some_and(|events| events.pane != watch.origin.pane)
        {
            events = None;
            next_subscribe = Instant::now();
        }
        if !once
            && !options.polling
            && events.is_none()
            && Instant::now() >= next_subscribe
            && let Some(socket) = socket.clone()
        {
            match Events::open(socket, &watch.origin.pane).await {
                Ok(opened) => {
                    events = Some(opened);
                    // Anything that changed before the subscription is read now.
                    trigger = Trigger::Poll;
                }
                Err(error) => {
                    next_subscribe = Instant::now() + RESUBSCRIBE;
                    if watch.mode != "polling" {
                        record_mode(store, &watch.goal, "polling", &format!("{error:#}"));
                    }
                }
            }
        }
        watch.mode = if events.is_some() {
            "events"
        } else {
            "polling"
        }
        .into();
        let observed = observe_worker(&watch, &trigger, last.as_ref()).await;
        let (live, screen) = match observed {
            Ok(observed) => observed,
            Err(error) => {
                // The pane may vanish on any trigger: a status event can come
                // before (or instead of) Herdr's exit event.
                let gone = error
                    .chain()
                    .any(|cause| cause.to_string() == crate::binding::PANE_GONE);
                if !gone && trigger != Trigger::Gone && retry(&error, &mut failures, once) {
                    tokio::time::sleep(POLL).await;
                    continue;
                }
                let reason = if trigger == Trigger::Gone || gone {
                    format!("target_exited: {error:#}")
                } else {
                    format!("needs_inspection: {error:#}")
                };
                end_for_person(store, &mut watch, &reason);
                save_watch(store, &watch)?;
                return Err(error);
            }
        };
        if trigger == Trigger::Reconcile
            && let Some((previous, _)) = &last
            && previous["state_change_seq"] != live["state_change_seq"]
        {
            // The state moved and no event said so: stop trusting the stream.
            events = None;
            next_subscribe = Instant::now() + RESUBSCRIBE;
            record_mode(
                store,
                &watch.goal,
                "polling",
                "state changed without an event",
            );
            watch.mode = "polling".into();
        }
        stats.observe(live["agent_status"].as_str().unwrap_or("unknown"));
        // The context handoff protocol restarts the original agent only.
        let main = watch.role == "main";
        let threshold = goal.spec.loop_policy.handoff_percent;
        if main
            && crate::handoff::context_used(watch.origin.agent.as_deref().unwrap_or(""), &screen)
                .is_some_and(|used| used >= f64::from(threshold))
        {
            watch.handoff_due = true;
            watch.pending = Some(format!(
                "context reached {threshold}%; prepare handoff and restart, do not compact"
            ));
        }
        let result = watch_once(store, &mut watch, &live, &screen).await;
        last = Some((live, screen));
        if let Err(error) = result {
            if retry(&error, &mut failures, once) {
                save_watch(store, &watch)?;
                tokio::time::sleep(POLL).await;
                continue;
            }
            end_for_person(store, &mut watch, &format!("needs_inspection: {error:#}"));
            save_watch(store, &watch)?;
            return Err(error);
        }
        failures = 0;
        // The manager's own native goal: a failed step leaves the watch running.
        if watch.active
            && let Err(error) = crate::manager_goal::step(store, &mut watch).await
        {
            eprintln!("The Goal Manager's native goal was not updated: {error:#}");
        }
        save_watch(store, &watch)?;
        display
            .show(store, &watch, last.as_ref().map(|(live, _)| live))
            .await;
        if let Err(error) = stats.save(store, &context.key(), &watch.mode) {
            eprintln!("Could not save watch statistics: {error:#}");
        }
        if !watch.active {
            return Ok(());
        }
        drop(operation);
        if main && watch.handoff_due {
            let observed = target(store, None, None, None).await?;
            if can_prompt(&observed["live"], observed["screen"].as_str().unwrap_or("")) {
                crate::handoff::request(store).await?;
            }
        }
        if once {
            return Ok(());
        }
        let remaining = deadline.map(|end| end.saturating_duration_since(Instant::now()));
        let span = |interval: Duration| remaining.map_or(interval, |left| left.min(interval));
        trigger = match events.as_mut() {
            Some(stream) => {
                let until = Instant::now() + span(reconcile);
                loop {
                    let slice = until
                        .saturating_duration_since(Instant::now())
                        .min(RECORD_CHECK);
                    match stream.next(slice).await {
                        // Between Herdr's events only the local record is read:
                        // a pause, cancel or wait ends this watch within seconds.
                        Ok(None) if Instant::now() < until => {
                            let Ok(record) = load_watch(store, &context.key()) else {
                                break Trigger::Poll;
                            };
                            // An await that started while a wake waits takes it now.
                            if !record.active
                                || (wake_ready(&record, chrono::Utc::now().timestamp())
                                    && crate::wake_await::running(store, &context.key())
                                        .unwrap_or(false))
                            {
                                break Trigger::Poll;
                            }
                        }
                        Ok(Some(Signal::Status(_))) => {
                            // A new turn state: watch for its attention text afresh.
                            stream.arm_output().await;
                            break Trigger::Event;
                        }
                        Ok(Some(Signal::Output(_))) => break Trigger::Event,
                        Ok(Some(Signal::Gone)) => break Trigger::Gone,
                        Ok(None) => break Trigger::Reconcile,
                        Err(error) => {
                            events = None;
                            next_subscribe = Instant::now() + RESUBSCRIBE;
                            record_mode(store, &watch.goal, "polling", &format!("{error:#}"));
                            break Trigger::Poll;
                        }
                    }
                }
            }
            None => {
                tokio::time::sleep(span(POLL)).await;
                Trigger::Poll
            }
        };
    }
}

/// Failed observations in a row that a continuing watch retries (one per
/// [`POLL`]) before the goal waits for the person.
const RETRIES: u32 = 6;
/// Whether to observe again after `error` instead of ending the watch: a
/// passing failure (a Herdr call that timed out) is retried; a replaced or
/// moved original agent, an unknown wake admission, or too many failures in a
/// row end it for the person. A one-off observation never retries.
fn retry(error: &anyhow::Error, failures: &mut u32, once: bool) -> bool {
    const LASTING: [&str; 6] = [
        crate::binding::PANE_GONE,
        crate::binding::SESSION_CHANGED,
        "bound terminal moved",
        "bound agent was replaced",
        "original agent exited",
        "admission uncertain",
    ];
    let lasting = error.chain().any(|cause| {
        let text = cause.to_string();
        LASTING.iter().any(|phrase| text.contains(phrase))
    });
    if once || lasting || *failures >= RETRIES {
        return false;
    }
    *failures += 1;
    eprintln!("Observation failed ({failures}/{RETRIES}), retrying: {error:#}");
    true
}

/// Reads the worker's state, and its screen unless a reconcile finds a settled
/// worker whose state has not moved since `last` (its screen is then reused).
async fn observe_worker(
    watch: &Watch,
    trigger: &Trigger,
    last: Option<&(Value, String)>,
) -> Result<(Value, String)> {
    let mut live = worker_state(&watch.origin.pane).await?;
    // Copilot can start or end a turn without a Herdr state change (see
    // `busy_on_screen`), so its screen is always read again.
    if *trigger == Trigger::Reconcile
        && live["agent_status"] != "working"
        && live["agent"] != "copilot"
        && let Some((previous, screen)) = last
        && previous["state_change_seq"] == live["state_change_seq"]
        && previous["agent_status"] == live["agent_status"]
    {
        return Ok((live, screen.clone()));
    }
    let screen = if live.get("agent").is_some() {
        screen(&watch.origin.pane).await?
    } else {
        String::new()
    };
    crate::signals::apply_screen_state(&mut live, &screen);
    Ok((live, screen))
}

/// Records a switch between event watching and polling on the goal timeline.
fn record_mode(store: &Store, goal: &str, mode: &str, reason: &str) {
    if let Err(error) = store.record(goal, "watch_mode", json!({"mode": mode, "reason": reason})) {
        eprintln!("Could not record the watch mode: {error:#}");
    }
}

/// The Goal watch tab (redrawn in place) and the watched pane's Herdr sidebar.
#[derive(Default)]
struct Display {
    /// Context of the shown watch, and the pane whose sidebar shows it.
    shown: Option<(String, String)>,
    /// Sidebar arguments last sent; Herdr is called only when they change.
    sidebar: Vec<String>,
    /// The tab is a terminal of a continuing watch, so the sheep may run.
    terminal: bool,
    /// The running sheep, while `watch.sheep` allows it.
    sheep: Option<Sheep>,
}
impl Display {
    async fn show(&mut self, store: &Store, watch: &Watch, live: Option<&Value>) {
        let state = live
            .and_then(|live| live["agent_status"].as_str())
            .unwrap_or("unknown");
        let view = match view_of(store, watch, state, None) {
            Ok(view) => view,
            Err(error) => {
                eprintln!("Could not build the watch view: {error:#}");
                return;
            }
        };
        // Read on every redraw, so `watch.sheep: false` takes effect without a rearm.
        let wanted = self.terminal
            && crate::config::load(&store.workspace).is_ok_and(|config| config.sheep());
        match (&self.sheep, wanted) {
            (Some(_), false) => {
                if let Some(sheep) = self.sheep.take() {
                    sheep.stop();
                }
            }
            (None, true) => self.sheep = Some(Sheep::start()),
            _ => {}
        }
        let size = if self.terminal {
            terminal_size().await
        } else {
            None
        };
        let width = size.map_or_else(width, |(_, columns)| columns);
        let text = crate::watch_view::render(&view, width);
        match &self.sheep {
            Some(sheep) => sheep.redraw(
                &text,
                width,
                size.map(|(rows, _)| rows),
                view.notice.clone(),
            ),
            None => println!("\x1b[H\x1b[2J{text}"),
        }
        if let Some((_, pane)) = &self.shown
            && *pane != view.pane
        {
            // The watch moved to another role: its old pane stops showing it.
            report(&crate::watch_view::clear_args(pane)).await;
            self.sidebar.clear();
        }
        self.shown = Some((watch.context.clone(), view.pane.clone()));
        let sidebar = crate::watch_view::sidebar_args(&view);
        if sidebar != self.sidebar {
            report(&sidebar).await;
            self.sidebar = sidebar;
        }
    }
    /// Shows why the watch stopped and clears the sidebar. `wait` keeps the
    /// tab open until Enter so the reason and the rearm command stay readable.
    async fn finish(&mut self, store: &Store, wait: bool) {
        if let Some(sheep) = self.sheep.take() {
            sheep.stop();
        }
        let Some((context, pane)) = self.shown.take() else {
            return;
        };
        report(&crate::watch_view::clear_args(&pane)).await;
        let Ok(watch) = load_watch(store, &context) else {
            return;
        };
        let ended = if watch.active {
            "the watch process stopped; the record is still active".to_string()
        } else {
            watch.status.clone()
        };
        if let Ok(view) = view_of(store, &watch, "unknown", Some(ended)) {
            println!("\x1b[H\x1b[2J{}", crate::watch_view::render(&view, width()));
        }
        if wait {
            println!("Press Enter to close this tab.");
            let _ = tokio::task::spawn_blocking(|| {
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)
            })
            .await;
        }
    }
}

/// Sends sidebar arguments to Herdr; a failure only costs the sidebar.
async fn report(arguments: &[String]) {
    if let Err(error) = herdr::query(arguments).await {
        eprintln!("Could not update the Herdr sidebar: {error:#}");
    }
}

/// The sheep under the watch text. Its task redraws only the sheep rows each
/// [`SHEEP_STEP`] and writes nothing but this tab: no Herdr call, no model and
/// no text any agent reads. The lock keeps its frames and full redraws apart.
struct Sheep {
    frame: Arc<Mutex<SheepFrame>>,
    task: tokio::task::JoinHandle<()>,
}
#[derive(Default)]
struct SheepFrame {
    /// First terminal row (1-based) below the watch text.
    row: usize,
    width: usize,
    /// Terminal rows, when known; a sheep that would not fit is not drawn.
    height: Option<usize>,
    notice: Option<String>,
    tick: u64,
    stopped: bool,
}
impl SheepFrame {
    fn draw(&self, out: &mut impl Write) {
        if self.stopped || self.row == 0 {
            return;
        }
        let lines = crate::watch_view::sheep(self.tick, self.width, self.notice.as_deref());
        let last = self.row + BUBBLE_ROWS + crate::watch_view::SHEEP_ROWS - 1;
        if lines.is_empty() || self.height.is_some_and(|height| last > height) {
            return;
        }
        // Without a bubble the sheep keeps its place below the empty bubble rows.
        let top = last + 1 - lines.len();
        let mut frame = String::from("\x1b7");
        for (offset, line) in lines.iter().enumerate() {
            frame += &format!("\x1b[{};1H\x1b[2K{line}", top + offset);
        }
        frame += "\x1b8";
        let _ = out.write_all(frame.as_bytes()).and_then(|()| out.flush());
    }
}
impl Sheep {
    fn start() -> Self {
        print!("\x1b[?25l");
        let frame = Arc::new(Mutex::new(SheepFrame::default()));
        let shared = Arc::clone(&frame);
        let task = tokio::spawn(async move {
            let mut every = tokio::time::interval(SHEEP_STEP);
            loop {
                every.tick().await;
                let Ok(mut frame) = shared.lock() else {
                    return;
                };
                frame.tick += 1;
                frame.draw(&mut std::io::stdout().lock());
            }
        });
        Self { frame, task }
    }
    /// Redraws the tab: `text`, then the sheep with `notice` below it.
    fn redraw(&self, text: &str, width: usize, height: Option<usize>, notice: Option<String>) {
        let Ok(mut frame) = self.frame.lock() else {
            return;
        };
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "\x1b[H\x1b[2J{text}");
        frame.row = text.lines().count() + 2;
        frame.width = width;
        frame.height = height;
        frame.notice = notice;
        frame.draw(&mut out);
    }
    fn stop(self) {
        if let Ok(mut frame) = self.frame.lock() {
            frame.stopped = true;
        }
        self.task.abort();
        print!("\x1b[H\x1b[2J\x1b[?25h");
        let _ = std::io::stdout().flush();
    }
}

/// Rows and columns of this tab's terminal (`stty size` on stdin).
async fn terminal_size() -> Option<(usize, usize)> {
    let output = tokio::process::Command::new("stty")
        .arg("size")
        .stdin(Stdio::inherit())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let mut numbers = text.split_whitespace().map(str::parse::<usize>);
    let (Some(Ok(rows)), Some(Ok(columns))) = (numbers.next(), numbers.next()) else {
        return None;
    };
    (columns >= 20).then_some((rows, columns))
}

/// The tab width Herdr gives this pane (`COLUMNS`), or 100 columns.
fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|columns| columns.parse().ok())
        .filter(|&columns| columns >= 20)
        .unwrap_or(100)
}

/// A snapshot of `watch` for the tab and the sidebar.
fn view_of(
    store: &Store,
    watch: &Watch,
    state: &str,
    ended: Option<String>,
) -> Result<crate::watch_view::WatchView> {
    let goal = store.load(&watch.goal)?;
    let events = store.events(&goal.id)?;
    let now = chrono::Utc::now();
    let policy = &goal.spec.loop_policy;
    let (assignments, time_left_minutes) = crate::admission::budget_numbers(&goal, &events, now);
    // Budgets start with the first assignment; before it the whole budget is left.
    let assignments = assignments.or(Some((0, policy.max_assignments)));
    let time_left_minutes =
        time_left_minutes.or(policy.max_hours.map(|hours| i64::from(hours) * 60));
    let patrol = i64::from(watch.patrol(&goal)) * 60;
    let next_check_secs = (patrol > 0 && state == "working" && ended.is_none())
        .then(|| watch.last_patrol + patrol - now.timestamp());
    let last_observation = (watch.last_observation_at > 0).then(|| {
        let at = chrono::DateTime::from_timestamp(watch.last_observation_at, 0)
            .unwrap_or(now)
            .with_timezone(&chrono::Local);
        format!("{} {state}", at.format("%H:%M:%S"))
    });
    let summary = |detail: &Value| {
        ["reason", "signal", "text", "task", "mode"]
            .iter()
            .find_map(|key| detail[*key].as_str())
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let recent = events.len().saturating_sub(5);
    let notice = watch.held.clone().or_else(|| {
        events
            .iter()
            .rev()
            .take_while(|event| (now - event.at).num_seconds() < NOTICE_SECS)
            .find_map(crate::events::notice)
    });
    Ok(crate::watch_view::WatchView {
        goal: goal.id.clone(),
        objective: goal.spec.objective.clone(),
        role: watch.role.clone(),
        pane: watch.origin.pane.clone(),
        state: state.into(),
        mode: (!watch.mode.is_empty()).then(|| watch.mode.clone()),
        last_observation,
        held_reason: watch.held.clone(),
        pending_reason: watch.pending.clone(),
        wakes_used: watch.wakes,
        wakes_max: watch.max_wakes,
        next_check_secs,
        patrol_minutes: watch.patrol(&goal),
        idle_secs: watch
            .idle_for(now.timestamp())
            .filter(|_| ended.is_none() && crate::watch::short_of_completion(&goal)),
        idle_notice_secs: watch.idle_notice_secs,
        native_goal: watch.native_goal.as_ref().map(|state| {
            crate::cli::native_text(
                &serde_json::json!({"set": state.is_set(), "outcome": state.outcome}),
            )
        }),
        manager_goal: watch
            .manager_goal
            .as_ref()
            .map(|state| crate::manager_goal::text(&state.view())),
        time_left_minutes,
        assignments,
        events: events[recent..]
            .iter()
            .map(|e| (e.at, e.kind.clone(), summary(&e.detail)))
            .collect(),
        ended_reason: ended,
        notice,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passing_failures_are_retried_and_lasting_ones_end_the_watch() {
        let mut failures = 0;
        let timeout = anyhow::anyhow!("Herdr did not answer within 5s");
        for _ in 0..RETRIES {
            assert!(retry(&timeout, &mut failures, false));
        }
        assert!(!retry(&timeout, &mut failures, false), "too many in a row");
        let mut failures = 0;
        assert!(
            !retry(&timeout, &mut failures, true),
            "a one-off never retries"
        );
        let replaced = anyhow::anyhow!(crate::binding::SESSION_CHANGED).context("observe main");
        assert!(!retry(&replaced, &mut failures, false));
        let gone = anyhow::anyhow!(crate::binding::PANE_GONE);
        assert!(!retry(&gone, &mut failures, false));
    }
}
