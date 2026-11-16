//! Goal watch tab text and Herdr sidebar metadata, built as pure functions.
//!
//! The watch loop collects a [`WatchView`] snapshot and passes it here; nothing
//! in this module calls Herdr or reads files.
use chrono::{DateTime, Local, TimeZone, Utc};
use std::fmt::Display;

/// Timeline events shown in the watch tab.
const RECENT_EVENTS: usize = 5;
/// Source id under which hgm reports pane metadata to Herdr.
const SIDEBAR_SOURCE: &str = "hgm";
/// Tokens set by [`sidebar_args`]; [`clear_args`] clears the same names.
const SIDEBAR_TOKENS: [&str; 3] = ["goal", "state", "budget"];
/// Column limit for the sidebar title and token values.
const SIDEBAR_WIDTH: usize = 40;
/// Characters of the goal id shown in the sidebar.
const SIDEBAR_GOAL_CHARS: usize = 10;

/// Snapshot of one goal watch, taken by the watch loop for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WatchView {
    /// Goal id, used in the rearm command.
    pub(crate) goal: String,
    /// Goal objective.
    pub(crate) objective: String,
    /// Watched role, such as main, impl or reviewer.
    pub(crate) role: String,
    /// Herdr pane id of the watched agent.
    pub(crate) pane: String,
    /// Native agent state reported by Herdr or read from the screen.
    pub(crate) state: String,
    /// Watch mode, such as "events" or "polling".
    pub(crate) mode: Option<String>,
    /// Most recent observation of the watched pane.
    pub(crate) last_observation: Option<String>,
    /// Why wakes are held (only the user can resolve it), when they are.
    pub(crate) held_reason: Option<String>,
    /// The wake waiting to reach the manager (cooldown or a busy manager).
    pub(crate) pending_reason: Option<String>,
    /// Manager wakes used so far.
    pub(crate) wakes_used: u32,
    /// Manager wake budget.
    pub(crate) wakes_max: u32,
    /// Seconds until the next patrol of a working agent; zero or less means it
    /// is due. `None` while the agent is not working.
    pub(crate) next_check_secs: Option<i64>,
    /// Patrol interval in minutes; zero when the goal has no patrol.
    pub(crate) patrol_minutes: u32,
    /// Seconds the worker has sat settled short of completion, while it does.
    pub(crate) idle_secs: Option<i64>,
    /// Seconds settled short of completion before the person is told.
    pub(crate) idle_notice_secs: u64,
    /// The native goal hgm set on the worker and its last outcome, if any.
    pub(crate) native_goal: Option<String>,
    /// The native goal hgm set on the Goal Manager and its last outcome, if any.
    pub(crate) manager_goal: Option<String>,
    /// Minutes of time budget left; negative means the budget is exceeded.
    pub(crate) time_left_minutes: Option<i64>,
    /// Assignments used and allowed.
    pub(crate) assignments: Option<(u32, u32)>,
    /// Timeline events, oldest first, as (time, kind, short summary).
    /// Only the last five are shown.
    pub(crate) events: Vec<(DateTime<Utc>, String, String)>,
    /// Why the watch ended, once it has.
    pub(crate) ended_reason: Option<String>,
    /// A recent notification or wake, or the current hold, for the sheep to say.
    pub(crate) notice: Option<String>,
}

/// The sheep that runs across the Goal watch tab (chosen by the user), facing
/// left. The watch process draws it by itself: no Herdr call, no model,
/// nothing for an agent. Widths count terminal columns: full-width characters
/// take two.
const SHEEP: [&str; SHEEP_ROWS] = [
    "⠀,〜⌒⌒､⌒ヽ",
    " (⊂,~〜､⊃）） ) )",
    "((Ꮚ• ﻌ •Ꮚ)　)　）",
    "  (　~　ノ(　ﾉ  ﾉ",
    "　ヽ〜  〜  ノノ",
    "　　UU￣UU",
];
/// [`SHEEP`] mirrored column by column, facing right.
const SHEEP_RIGHT: [&str; SHEEP_ROWS] = [
    "       ノ⌒,⌒⌒〜､",
    "( (（（⊂,〜~､⊃)",
    "（　(　(Ꮚ• ﻌ •Ꮚ))",
    "\\  \\　)ヽ　~　)",
    " ヽヽ  〜  〜ノ",
    "       UU￣UU",
];
/// Rows of [`SHEEP`].
pub(crate) const SHEEP_ROWS: usize = 6;
/// The other leg pose (facing left, then right); alternating it with the last
/// row makes the sheep run.
const SHEEP_STRIDE: [&str; 2] = ["　 U U￣U U", "      U U￣U U"];
/// The sheep's nose, under which its speech bubble points.
const SHEEP_NOSE: char = 'ﻌ';

/// Lines of the sheep at animation step `tick` in a tab `width` columns wide.
/// It moves one column per step and turns back at each edge. A `notice`
/// appears in a speech bubble above its head. Empty when the tab is too narrow.
pub(crate) fn sheep(tick: u64, width: usize, notice: Option<&str>) -> Vec<String> {
    let body = SHEEP
        .iter()
        .map(|row| display_width(row))
        .max()
        .unwrap_or(0);
    let Some(track) = width.checked_sub(body + 1).filter(|&track| track > 0) else {
        return Vec::new();
    };
    let track = track as u64;
    let step = tick % (2 * track);
    let (x, right) = if step < track {
        (step, true)
    } else {
        (2 * track - step, false)
    };
    let x = x as usize;
    // The face leads: mirrored while the sheep runs right.
    let mut rows = if right { SHEEP_RIGHT } else { SHEEP };
    if !tick.is_multiple_of(2) {
        rows[SHEEP_ROWS - 1] = SHEEP_STRIDE[usize::from(right)];
    }
    let head = x + rows
        .iter()
        .find_map(|row| row.find(SHEEP_NOSE).map(|at| display_width(&row[..at])))
        .unwrap_or(0);
    let mut lines = Vec::new();
    if let Some(notice) = notice.map(single_line).filter(|n| !n.is_empty()) {
        let text = fit(&notice, width.saturating_sub(4));
        let inner = display_width(&text) + 2;
        let left = head.saturating_sub(inner / 2).min(width - inner - 2);
        let pad = " ".repeat(left);
        lines.push(format!("{pad} {}", "_".repeat(inner)));
        lines.push(format!("{pad}< {text} >"));
        lines.push(format!("{pad} {}", "-".repeat(inner)));
        let tail = if right { '\\' } else { '/' };
        lines.push(format!("{}{tail}", " ".repeat(head)));
    }
    for row in rows {
        lines.push(format!("{}{row}", " ".repeat(x)).trim_end().to_owned());
    }
    lines
}

/// Renders the Goal watch tab as plain text lines joined by `\n`, without a
/// trailing newline or ANSI sequences. Every line fits in `width` columns;
/// longer lines end with "…". Event times use the local time zone.
pub(crate) fn render(view: &WatchView, width: usize) -> String {
    render_in(view, width, &Local)
}

fn render_in<Tz>(view: &WatchView, width: usize, zone: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let mut lines = vec![
        single_line(&view.objective),
        format!("goal {}", single_line(&view.goal)),
    ];
    let watched = format!(
        "{} in {}: {}",
        single_line(&view.role),
        single_line(&view.pane),
        single_line(&view.state)
    );
    match &view.ended_reason {
        Some(reason) => {
            lines.push(format!("watch ended: {}", single_line(reason)));
            lines.push(format!("rearm: {}", rearm_command(view)));
            lines.push(format!("last seen {watched}"));
        }
        None => match &view.mode {
            Some(mode) => lines.push(format!("watching {watched} ({})", single_line(mode))),
            None => lines.push(format!("watching {watched}")),
        },
    }
    lines.push(match &view.last_observation {
        Some(observation) => format!("last observation: {}", single_line(observation)),
        None => "last observation: none yet".into(),
    });
    if let Some(idle) = view.idle_secs {
        lines.push(format!(
            "{} idle {} short of completion (you are told at {})",
            single_line(&view.role),
            duration(idle),
            duration(view.idle_notice_secs as i64)
        ));
    }
    if let Some(native) = &view.native_goal {
        lines.push(format!("native /goal: {}", single_line(native)));
    }
    if let Some(manager) = &view.manager_goal {
        lines.push(format!("manager /goal: {}", single_line(manager)));
    }
    if let Some(reason) = &view.held_reason {
        lines.push(format!("held: {}", single_line(reason)));
    } else if let Some(reason) = &view.pending_reason {
        lines.push(format!("wake pending: {}", single_line(reason)));
    }
    let mut wakes = format!("wakes {}/{}", view.wakes_used, view.wakes_max);
    if view.ended_reason.is_none() {
        match view.next_check_secs {
            Some(secs) => {
                wakes.push_str(" · ");
                wakes.push_str(&next_check(secs));
            }
            // Patrols run only while the agent works; a settled agent wakes
            // the manager by settling.
            None if view.patrol_minutes > 0 => wakes.push_str(&format!(
                " · next patrol when {} works (every {})",
                single_line(&view.role),
                duration(i64::from(view.patrol_minutes) * 60)
            )),
            None => {}
        }
    }
    lines.push(wakes);
    let budget = budget_parts(view);
    if !budget.is_empty() {
        lines.push(budget.join(" · "));
    }
    let recent = &view.events[view.events.len().saturating_sub(RECENT_EVENTS)..];
    if recent.is_empty() {
        lines.push("recent events: none".into());
    } else {
        lines.push("recent events:".into());
        for (at, kind, summary) in recent {
            lines.push(format!(
                "{} {} {}",
                at.with_timezone(zone).format("%H:%M"),
                single_line(kind),
                single_line(summary)
            ));
        }
    }
    lines
        .iter()
        .map(|line| fit(line, width))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Arguments after the `herdr` binary that report goal state and budget in the
/// watched pane's sidebar.
pub(crate) fn sidebar_args(view: &WatchView) -> Vec<String> {
    let mut args = metadata_args(&view.pane);
    args.push("--title".into());
    args.push(fit(
        &format!("hgm: {}", single_line(&view.objective)),
        SIDEBAR_WIDTH,
    ));
    let goal: String = single_line(&view.goal)
        .chars()
        .take(SIDEBAR_GOAL_CHARS)
        .collect();
    let values = [goal, sidebar_state(view), sidebar_budget(view)];
    for (name, value) in SIDEBAR_TOKENS.iter().zip(values) {
        args.push("--token".into());
        args.push(format!("{name}={}", fit(&value, SIDEBAR_WIDTH)));
    }
    args
}

/// Arguments after the `herdr` binary that clear everything [`sidebar_args`]
/// set on `pane`.
pub(crate) fn clear_args(pane: &str) -> Vec<String> {
    let mut args = metadata_args(pane);
    args.push("--clear-title".into());
    for name in SIDEBAR_TOKENS {
        args.push("--clear-token".into());
        args.push(name.into());
    }
    args
}

/// Herdr 0.9.5 parses the pane id only before the options, although its help
/// lists the options first.
fn metadata_args(pane: &str) -> Vec<String> {
    vec![
        "pane".into(),
        "report-metadata".into(),
        pane.into(),
        "--source".into(),
        SIDEBAR_SOURCE.into(),
    ]
}

fn sidebar_state(view: &WatchView) -> String {
    if view.ended_reason.is_some() {
        "watch ended".into()
    } else if view.held_reason.is_some() {
        format!("held ({})", single_line(&view.state))
    } else if view.pending_reason.is_some() {
        format!("{} · wake pending", single_line(&view.state))
    } else {
        single_line(&view.state)
    }
}

/// Budget in the short form the narrow sidebar fits, such as
/// "wakes 2/8 · 1h 30m left · 1/5 assigned".
fn sidebar_budget(view: &WatchView) -> String {
    let mut parts = vec![format!("wakes {}/{}", view.wakes_used, view.wakes_max)];
    if let Some(minutes) = view.time_left_minutes {
        parts.push(if minutes < 0 {
            format!("{} over", duration(-minutes * 60))
        } else {
            format!("{} left", duration(minutes * 60))
        });
    }
    if let Some((used, max)) = view.assignments {
        parts.push(format!("{used}/{max} assigned"));
    }
    parts.join(" · ")
}

fn rearm_command(view: &WatchView) -> String {
    let goal = single_line(&view.goal);
    if view.role == "main" {
        format!("hgm supervise {goal}")
    } else {
        format!("hgm supervise {goal} --role {}", single_line(&view.role))
    }
}

fn budget_parts(view: &WatchView) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(minutes) = view.time_left_minutes {
        parts.push(if minutes < 0 {
            format!("time over by {}", duration(-minutes * 60))
        } else {
            format!("time left {}", duration(minutes * 60))
        });
    }
    if let Some((used, max)) = view.assignments {
        parts.push(format!("assignments {used}/{max}"));
    }
    parts
}

fn next_check(secs: i64) -> String {
    if secs <= 0 {
        "next patrol due".into()
    } else {
        format!("next patrol in {}", duration(secs))
    }
}

/// Formats a non-negative span as "45s", "4m 10s" or "1h 30m".
pub(crate) fn duration(secs: i64) -> String {
    let (hours, minutes, seconds) = (secs / 3600, secs % 3600 / 60, secs % 60);
    match (hours, minutes, seconds) {
        (0, 0, s) => format!("{s}s"),
        (0, m, 0) => format!("{m}m"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

/// Collapses whitespace, including newlines, to single spaces and drops other
/// control characters so one value never spans lines.
fn single_line(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// Truncates `text` to `width` terminal columns, ending with "…" when cut.
fn fit(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_owned();
    }
    let Some(room) = width.checked_sub(1) else {
        return String::new();
    };
    let mut fitted = String::new();
    let mut used = 0;
    for c in text.chars() {
        let columns = char_width(c);
        if used + columns > room {
            break;
        }
        fitted.push(c);
        used += columns;
    }
    fitted.push('…');
    fitted
}

fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

// nwiizo-coding-style: covers CJK, Hangul, fullwidth forms, emoji and combining
// marks, not the full Unicode width tables; switch to the unicode-width crate if
// it becomes a dependency or other scripts misalign.
fn char_width(c: char) -> usize {
    match u32::from(c) {
        0x0300..=0x036F | 0x200B..=0x200F | 0x20D0..=0x20FF | 0xFE00..=0xFE0F => 0,
        _ if c.is_control() => 0,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOAL: &str = "g-656867c2-bd39-4a00-acb7-a143548327e6";

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 13, hour, minute, 0).unwrap()
    }

    fn watching() -> WatchView {
        WatchView {
            goal: GOAL.into(),
            objective: "Ship the watch tab".into(),
            role: "main".into(),
            pane: "w1-p2".into(),
            state: "working".into(),
            mode: Some("events".into()),
            last_observation: Some("running cargo test\nall green".into()),
            held_reason: None,
            pending_reason: None,
            wakes_used: 2,
            wakes_max: 8,
            next_check_secs: Some(250),
            patrol_minutes: 5,
            idle_secs: None,
            idle_notice_secs: 180,
            native_goal: None,
            manager_goal: None,
            time_left_minutes: Some(90),
            assignments: Some((1, 5)),
            events: (0..7)
                .map(|i| (at(9, i), format!("kind{i}"), format!("summary {i}")))
                .collect(),
            ended_reason: None,
            notice: None,
        }
    }

    #[test]
    fn the_sheep_runs_one_column_per_step_and_turns_at_the_edges() {
        let width = 30;
        let body = 17;
        assert_eq!(SHEEP.iter().map(|row| display_width(row)).max(), Some(body));
        let track = width - body - 1;
        let face = |tick: u64| sheep(tick, width, None)[2].clone();
        // Running right, the face is at the front (right) of the body.
        assert_eq!(face(0), "（　(　(Ꮚ• ﻌ •Ꮚ))");
        assert_eq!(face(1), " （　(　(Ꮚ• ﻌ •Ꮚ))");
        for (left, right) in SHEEP.iter().zip(SHEEP_RIGHT) {
            assert!(display_width(right) <= body, "{right}");
            assert!(display_width(left) <= body, "{left}");
        }
        let last = SHEEP_ROWS - 1;
        assert_ne!(
            sheep(0, width, None)[last],
            sheep(1, width, None)[last],
            "legs move"
        );
        // At the right edge it turns: the original sheep faces left.
        assert_eq!(
            face(track as u64 + 1),
            format!("{}((Ꮚ• ﻌ •Ꮚ)　)　）", " ".repeat(track - 1))
        );
        assert_eq!(sheep(2 * track as u64, width, None), sheep(0, width, None));
        for tick in 0..(4 * track as u64) {
            let lines = sheep(tick, width, None);
            assert_eq!(lines.len(), SHEEP_ROWS);
            for line in lines {
                assert!(display_width(&line) < width, "{tick}: {line}");
            }
        }
        assert!(sheep(0, body + 1, None).is_empty(), "too narrow");
    }

    #[test]
    fn the_sheep_says_a_notice_in_a_bubble_that_fits_the_tab() {
        let width = 30;
        let notice = "Original agent needs you: 承認ダイアログが表示されています";
        for tick in [0, 5, 12, 20] {
            let lines = sheep(tick, width, Some(notice));
            assert_eq!(lines.len(), 4 + SHEEP_ROWS, "{lines:?}");
            assert!(lines[1].contains("< Original agent"), "{lines:?}");
            assert!(lines[1].ends_with("… >"), "{lines:?}");
            for line in &lines {
                assert!(display_width(line) <= width, "{tick}: {line}");
            }
            let tail = lines[3].len() - 1;
            assert!(tail >= lines[1].find('<').unwrap(), "tail under the bubble");
        }
        assert_eq!(
            sheep(0, width, Some(" ")).len(),
            SHEEP_ROWS,
            "blank notice: no bubble"
        );
    }

    fn render_utc(view: &WatchView, width: usize) -> String {
        render_in(view, width, &Utc)
    }

    #[test]
    fn render_while_watching_shows_state_budget_and_last_five_events() {
        let text = render_utc(&watching(), 80);
        assert_eq!(
            text,
            [
                "Ship the watch tab",
                &format!("goal {GOAL}"),
                "watching main in w1-p2: working (events)",
                "last observation: running cargo test all green",
                "wakes 2/8 · next patrol in 4m 10s",
                "time left 1h 30m · assignments 1/5",
                "recent events:",
                "09:02 kind2 summary 2",
                "09:03 kind3 summary 3",
                "09:04 kind4 summary 4",
                "09:05 kind5 summary 5",
                "09:06 kind6 summary 6",
            ]
            .join("\n")
        );
    }

    #[test]
    fn render_uses_local_time_for_events() {
        let view = watching();
        let text = render(&view, 80);
        assert_eq!(text, render_in(&view, 80, &Local));
        let local = at(9, 6).with_timezone(&Local).format("%H:%M").to_string();
        assert!(
            text.ends_with(&format!("\n{local} kind6 summary 6")),
            "{text}"
        );
    }

    #[test]
    fn render_while_held_shows_reason_and_due_check() {
        let mut view = watching();
        view.mode = None;
        view.held_reason = Some("user draft in the composer".into());
        view.next_check_secs = Some(0);
        view.time_left_minutes = Some(-65);
        view.assignments = None;
        view.events.clear();
        view.last_observation = None;
        let text = render_utc(&view, 80);
        assert!(
            text.contains("\nwatching main in w1-p2: working\n"),
            "{text}"
        );
        assert!(text.contains("\nlast observation: none yet\n"), "{text}");
        assert!(
            text.contains("\nheld: user draft in the composer\n"),
            "{text}"
        );
        assert!(text.contains("\nwakes 2/8 · next patrol due\n"), "{text}");
        assert!(text.contains("\ntime over by 1h 5m\n"), "{text}");
        assert!(text.ends_with("\nrecent events: none"), "{text}");
    }

    #[test]
    fn render_of_a_settled_agent_says_when_the_next_patrol_comes() {
        let mut view = watching();
        view.state = "idle".into();
        view.next_check_secs = None;
        let text = render_utc(&view, 80);
        assert!(
            text.contains("\nwakes 2/8 · next patrol when main works (every 5m)\n"),
            "{text}"
        );
        view.patrol_minutes = 0;
        assert!(render_utc(&view, 80).contains("\nwakes 2/8\n"));
    }

    #[test]
    fn render_after_end_shows_reason_and_rearm_command() {
        let mut view = watching();
        view.ended_reason = Some("goal cancelled".into());
        let text = render_utc(&view, 80);
        assert!(text.contains("\nwatch ended: goal cancelled\n"), "{text}");
        assert!(
            text.contains(&format!("\nrearm: hgm supervise {GOAL}\n")),
            "{text}"
        );
        assert!(
            text.contains("\nlast seen main in w1-p2: working\n"),
            "{text}"
        );
        assert!(!text.contains("watching"), "{text}");
        assert!(!text.contains("next patrol"), "{text}");

        view.role = "reviewer".into();
        let text = render_utc(&view, 80);
        assert!(
            text.contains(&format!("rearm: hgm supervise {GOAL} --role reviewer")),
            "{text}"
        );
    }

    #[test]
    fn render_truncates_every_line_including_wide_text() {
        let mut view = watching();
        view.objective = "監視タブを本番相当のデータで確認する".into();
        view.last_observation = Some("ダイアログが表示されています".into());
        for width in [0, 1, 2, 9, 20, 33] {
            let text = render_utc(&view, width);
            for line in text.lines() {
                assert!(display_width(line) <= width, "{width}: {line:?}");
            }
        }
        let text = render_utc(&view, 9);
        let first = text.lines().next().unwrap();
        assert_eq!(first, "監視タブ…");
        assert_eq!(fit("監視", 3), "監…");
        assert_eq!(fit("監視", 4), "監視");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("abc", 0), "");
        assert_eq!(fit("é", 1), "é");
    }

    #[test]
    fn sidebar_reports_goal_state_and_budget() {
        let mut view = watching();
        view.held_reason = Some("dialog".into());
        assert_eq!(
            sidebar_args(&view),
            [
                "pane",
                "report-metadata",
                "w1-p2",
                "--source",
                "hgm",
                "--title",
                "hgm: Ship the watch tab",
                "--token",
                "goal=g-656867c2",
                "--token",
                "state=held (working)",
                "--token",
                "budget=wakes 2/8 · 1h 30m left · 1/5 assigned",
            ]
        );
        view.ended_reason = Some("pane closed".into());
        view.time_left_minutes = Some(-5);
        view.assignments = None;
        let args = sidebar_args(&view);
        assert!(args.contains(&"state=watch ended".to_string()), "{args:?}");
        assert!(
            args.contains(&"budget=wakes 2/8 · 5m over".to_string()),
            "{args:?}"
        );
        view.objective = "x".repeat(60);
        let args = sidebar_args(&view);
        assert!(
            args.iter()
                .all(|arg| display_width(arg) <= SIDEBAR_WIDTH + 7)
        );
        assert_eq!(display_width(&args[6]), SIDEBAR_WIDTH);
    }

    #[test]
    fn a_pending_wake_is_not_shown_as_held() {
        let mut view = watching();
        view.state = "done".into();
        view.pending_reason = Some("target state changed to done".into());
        let text = render_utc(&view, 80);
        assert!(
            text.contains("\nwake pending: target state changed to done\n"),
            "{text}"
        );
        assert!(!text.contains("held"), "{text}");
        assert!(sidebar_args(&view).contains(&"state=done · wake pending".to_string()));
        // A hold wins: only the user can resolve it.
        view.held_reason = Some("dialog".into());
        let text = render_utc(&view, 80);
        assert!(text.contains("\nheld: dialog\n") && !text.contains("wake pending"));
    }

    #[test]
    fn clear_removes_everything_the_sidebar_sets() {
        assert_eq!(
            clear_args("w1-p2"),
            [
                "pane",
                "report-metadata",
                "w1-p2",
                "--source",
                "hgm",
                "--clear-title",
                "--clear-token",
                "goal",
                "--clear-token",
                "state",
                "--clear-token",
                "budget",
            ]
        );
    }
}
