use serde_json::Value;

const PROMPTS: [&str; 5] = [
    "Do you want to",
    "Would you like to run",
    "Do you trust the files",
    "Permission request",
    "requires approval",
];
const LOGIN: [&str; 7] = [
    "please log in",
    "please login",
    "run /login",
    "not logged in",
    "token has expired",
    "could not be refreshed",
    "invalid api key",
];
const LIMIT: [&str; 6] = [
    "usage limit",
    "rate limit reached",
    "rate limit exceeded",
    "limit reached",
    "hit your limit",
    "quota exceeded",
];
const COMPACTION: [&str; 4] = [
    "conversation compacted",
    "context compacted",
    "compacted (ctrl+o",
    "compaction complete",
];

pub(crate) fn attention_pattern() -> String {
    let escape = |text: &str| {
        text.chars().fold(String::new(), |mut out, c| {
            if "\\.+*?()|[]{}^$".contains(c) {
                out.push('\\');
            }
            out.push(c);
            out
        })
    };
    let alternatives: Vec<String> = PROMPTS
        .iter()
        .chain(&LOGIN)
        .chain(&LIMIT)
        .chain(&COMPACTION)
        .map(|text| escape(text))
        .collect();
    format!("(?i)({})", alternatives.join("|"))
}

pub(crate) fn needs_human(live: &Value, screen: &str) -> bool {
    live["agent_status"] == "blocked"
        || screen
            .lines()
            .rev()
            .take(30)
            .any(|line| PROMPTS.iter().any(|prompt| line.contains(prompt)))
}

pub(crate) fn rot_signals(screen: &str) -> Vec<(&'static str, String)> {
    const AGREEMENT: [&str; 4] = [
        "You're absolutely right",
        "You are absolutely right",
        "まったくその通り",
        "おっしゃる通り",
    ];
    let mut signals = vec![];
    if let Some(line) = screen
        .lines()
        .find(|line| AGREEMENT.iter().any(|phrase| line.contains(phrase)))
    {
        signals.push(("reflexive_agreement", line.trim().to_string()));
    }
    let mut calls = std::collections::HashMap::new();
    for line in screen.lines().map(str::trim) {
        if names_command(line) {
            *calls.entry(line).or_insert(0) += 1;
        }
    }
    if let Some((line, _)) = calls.iter().find(|(_, count)| **count >= 3) {
        signals.push(("repeated_tool_call", line.to_string()));
    }
    signals
}

fn names_command(line: &str) -> bool {
    const TOOL_MARKERS: [&str; 4] = ["• Ran ", "$ ", "● Bash(", "⏺ Bash("];
    TOOL_MARKERS.iter().any(|marker| {
        line.strip_prefix(marker).is_some_and(|command| {
            let count = command.trim_start_matches(|c: char| c.is_ascii_digit());
            !command.trim().is_empty()
                && !(count.len() < command.len() && count.starts_with(" shell command"))
        })
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    Dialog,
    Login(String),
    Limit(String),
    ProviderError(String),
}

pub(crate) fn stop_reason(live: &Value, screen: &str) -> Option<Stop> {
    const PROVIDER: [&str; 15] = [
        "api error:",
        "overloaded",
        "stream disconnected",
        "internal server error",
        "request timed out",
        "service unavailable",
        "connection error",
        "enotfound",
        "econnreset",
        "econnrefused",
        "etimedout",
        "fetch failed",
        "network error",
        "socket hang up",
        "getaddrinfo",
    ];
    if needs_human(live, screen) {
        return Some(Stop::Dialog);
    }
    if !matches!(
        live["agent_status"].as_str(),
        Some("idle" | "done" | "unknown")
    ) {
        return None;
    }
    let tail: Vec<&str> = screen
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(12)
        .collect();
    let find = |phrases: &[&str]| {
        tail.iter()
            .find(|line| {
                let lower = line.to_lowercase();
                phrases.iter().any(|phrase| lower.contains(phrase))
            })
            .map(|line| line.chars().take(160).collect::<String>())
    };
    if let Some(line) = find(&LOGIN) {
        return Some(Stop::Login(line));
    }
    if let Some(line) = find(&LIMIT) {
        return Some(Stop::Limit(line));
    }
    find(&PROVIDER).map(Stop::ProviderError)
}

pub(crate) fn compaction_shown(screen: &str) -> Option<String> {
    screen
        .lines()
        .rev()
        .take(20)
        .map(str::trim)
        .find(|line| {
            let lower = line.to_lowercase();
            COMPACTION.iter().any(|marker| lower.contains(marker))
        })
        .map(|line| line.chars().take(160).collect())
}
pub(crate) fn plain_screen(ansi: &str) -> String {
    let mut text = String::with_capacity(ansi.len());
    for (index, line) in ansi.split('\n').enumerate() {
        if index > 0 {
            text.push('\n');
        }
        let start = text.len();
        let (mut faint, mut prompt) = (false, false);
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            match c {
                '\x1b' => match chars.next() {
                    Some('[') => {
                        let mut parameters = String::new();
                        for p in chars.by_ref() {
                            if ('\x40'..='\x7e').contains(&p) {
                                if p == 'm' {
                                    faint = sgr_faint(&parameters, faint);
                                }
                                break;
                            }
                            parameters.push(p);
                        }
                    }
                    Some(']') => {
                        while let Some(p) = chars.next() {
                            if p == '\x07' || (p == '\x1b' && chars.next().is_some()) {
                                break;
                            }
                        }
                    }
                    Some('(' | ')') => {
                        chars.next();
                    }
                    _ => {}
                },
                '\r' => {}
                _ if prompt && faint => {}
                _ => {
                    if matches!(c, '❯' | '›') && !faint && text[start..].trim().is_empty() {
                        prompt = true;
                    }
                    text.push(c);
                }
            }
        }
    }
    text
}
fn sgr_faint(parameters: &str, mut faint: bool) -> bool {
    let codes: Vec<&str> = parameters.split(';').collect();
    let mut index = 0;
    while index < codes.len() {
        match codes[index] {
            "" | "0" | "22" => faint = false,
            "2" => faint = true,
            "38" | "48" | "58" => {
                index += match codes.get(index + 1) {
                    Some(&"5") => 2,
                    Some(&"2") => 4,
                    _ => 0,
                };
            }
            _ => {}
        }
        index += 1;
    }
    faint
}
pub(crate) fn empty_composer(screen: &str, agent: &str) -> bool {
    composer(screen, agent) == Some("")
}
fn composer<'a>(screen: &'a str, agent: &str) -> Option<&'a str> {
    if agent == "copilot" {
        let lines: Vec<_> = screen.lines().rev().take(8).map(str::trim).collect();
        return lines.windows(3).find_map(|w| {
            (w[0].starts_with("╹") && w[2].starts_with("╻"))
                .then(|| w[1].strip_prefix('┃').map(str::trim))
                .flatten()
        });
    }
    let mut continuation = None;
    for row in screen.lines().rev().take(16) {
        let line = row.trim();
        let tail = match agent {
            "claude" => line.strip_prefix('❯'),
            "codex" => line.strip_prefix('›'),
            _ => None,
        };
        if let Some(tail) = tail {
            let tail = tail.trim();
            return Some(if tail.is_empty() {
                continuation.unwrap_or("")
            } else {
                tail
            });
        }
        if row.starts_with(['─', '━']) && line.chars().all(|c| matches!(c, '─' | '━')) {
            continuation = None;
        } else if !line.is_empty() {
            continuation = Some(line);
        }
    }
    None
}
const CLAUDE_SPINNER: [char; 7] = ['✻', '✽', '✶', '✳', '✢', '·', '*'];
fn volatile(line: &str, agent: &str) -> bool {
    const SHARED: [&str; 8] = [
        "tip:",
        "esc to interrupt",
        "ctrl+c interrupt",
        "esc to cancel",
        " tokens",
        "retrying",
        "reconnecting",
        "/goal active",
    ];
    let trimmed = line.trim();
    let lower = trimmed.to_lowercase();
    if SHARED.iter().any(|phrase| lower.contains(phrase)) {
        return true;
    }
    let elapsed = || {
        trimmed.split(|c: char| !c.is_alphanumeric()).any(|word| {
            word.len() > 1
                && matches!(word.chars().last(), Some('s' | 'm' | 'h'))
                && word[..word.len() - 1].chars().all(|c| c.is_ascii_digit())
        })
    };
    match agent {
        "claude" => {
            trimmed
                .chars()
                .next()
                .is_some_and(|c| CLAUDE_SPINNER.contains(&c))
                && (trimmed.contains('…') || elapsed())
        }
        "codex" => (trimmed.starts_with('•') || trimmed.starts_with('◦')) && elapsed(),
        "copilot" => (trimmed.starts_with('◎') || trimmed.starts_with('∙')) && elapsed(),
        _ => false,
    }
}
pub(crate) fn progress_view(screen: &str, agent: &str) -> String {
    let lines: Vec<&str> = screen.lines().collect();
    let input = lines.iter().rposition(|line| {
        let line = line.trim_start();
        line.starts_with('❯') || line.starts_with('›') || line.starts_with('╻')
    });
    lines[..input.unwrap_or(lines.len())]
        .iter()
        .filter(|line| !line.trim().is_empty() && !volatile(line, agent))
        .map(|line| without_elapsed(line.trim().trim_start_matches(['⏺', ' '])))
        .collect::<Vec<_>>()
        .join("\n")
}
fn without_elapsed(line: &str) -> &str {
    let Some((head, tail)) = line.rsplit_once(" · ") else {
        return line;
    };
    let clock = tail.split_whitespace().all(|word| {
        word.len() > 1
            && matches!(word.chars().last(), Some('s' | 'm' | 'h'))
            && word[..word.len() - 1].chars().all(|c| c.is_ascii_digit())
    });
    if clock && !tail.trim().is_empty() {
        head
    } else {
        line
    }
}
pub(crate) fn retry_line(screen: &str) -> Option<String> {
    screen
        .lines()
        .rev()
        .take(16)
        .map(str::trim)
        .find(|line| {
            let lower = line.to_lowercase();
            lower.contains("retrying") || lower.contains("reconnecting")
        })
        .map(|line| {
            line.trim_start_matches(|c: char| CLAUDE_SPINNER.contains(&c) || c.is_whitespace())
                .chars()
                .take(120)
                .collect()
        })
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NativeGoal {
    Active,
    Retrying(String),
    Met(String),
    Impossible(String),
    Paused(String),
    ErrorCleared(String),
    Cleared(String),
    Unavailable(String),
}
pub(crate) fn native_goal(screen: &str) -> Option<NativeGoal> {
    let mut indicator = false;
    for line in screen.lines().rev().take(20).map(str::trim) {
        let lower = line.to_lowercase();
        let quoted = || line.chars().take(200).collect::<String>();
        if line.contains("/goal active") {
            indicator = true;
            continue;
        }
        let found = if lower.contains("goal set:") {
            NativeGoal::Active
        } else if lower.contains("goal cleared after an unrecoverable error") {
            NativeGoal::ErrorCleared(quoted())
        } else if lower.starts_with("goal paused") || lower.contains(" goal paused") {
            NativeGoal::Paused(quoted())
        } else if lower.contains("goal still active") {
            NativeGoal::Retrying(quoted())
        } else if lower.contains("goal cleared:") || lower.contains("no goal set") {
            NativeGoal::Cleared(quoted())
        } else if lower.contains("unknown command: /goal")
            || lower.contains("/goal is not available")
            || lower.contains("/goal is unavailable")
            || (lower.contains("/goal") && lower.contains("hooks") && lower.contains("disabled"))
        {
            NativeGoal::Unavailable(quoted())
        } else if lower.contains("goal achieved") || lower.contains("goal met") {
            NativeGoal::Met(quoted())
        } else if lower.contains("goal failed") || lower.contains("goal impossible") {
            NativeGoal::Impossible(quoted())
        } else {
            continue;
        };
        return Some(found);
    }
    indicator.then_some(NativeGoal::Active)
}
pub(crate) fn own_lines(screen: &str) -> String {
    screen
        .lines()
        .filter(|line| !line.starts_with(char::is_whitespace))
        .collect::<Vec<_>>()
        .join("\n")
}
pub(crate) fn goal_indicator_shown(screen: &str) -> bool {
    let lines: Vec<&str> = screen.lines().collect();
    let input = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with('❯'))
        .unwrap_or(lines.len());
    lines[input.saturating_sub(4)..input]
        .iter()
        .any(|line| line.contains("/goal active"))
}
pub(crate) fn unseen_outcome(screen: &str) -> Option<NativeGoal> {
    let own = own_lines(screen);
    let earlier: Vec<&str> = own
        .lines()
        .filter(|line| !line.contains("Goal set:"))
        .collect();
    native_goal(&earlier.join("\n")).filter(|notice| {
        matches!(
            notice,
            NativeGoal::Met(_)
                | NativeGoal::Impossible(_)
                | NativeGoal::ErrorCleared(_)
                | NativeGoal::Cleared(_)
        )
    })
}
/// Whether the screen shows a turn still running where Herdr's state may not:
/// Herdr 0.9.5 lists Copilot 1.0.110 as `done` while it runs a tool, and
/// Copilot's bottom status line then offers `ctrl+c interrupt`.
pub(crate) fn busy_on_screen(screen: &str, agent: &str) -> bool {
    agent == "copilot"
        && screen
            .lines()
            .rev()
            .filter(|line| !line.trim().is_empty())
            .take(2)
            .any(|line| line.contains("ctrl+c interrupt"))
}
pub(crate) fn apply_screen_state(live: &mut Value, screen: &str) {
    if busy_on_screen(screen, live["agent"].as_str().unwrap_or("")) {
        live["agent_status"] = "working".into();
    }
}
fn dialog_shown(screen: &str) -> bool {
    let lines: Vec<&str> = screen.lines().collect();
    lines[lines.len().saturating_sub(30)..].iter().any(|line| {
        PROMPTS.iter().any(|prompt| line.contains(prompt)) && !line.trim_start().starts_with('⏺')
    })
}
pub(crate) fn can_prompt(live: &Value, screen: &str) -> bool {
    let agent = live["agent"].as_str().unwrap_or("");
    matches!(live["agent_status"].as_str(), Some("idle" | "done"))
        && !busy_on_screen(screen, agent)
        && empty_composer(screen, agent)
        && !dialog_shown(screen)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    Working,
    Dialog,
    Draft(String),
    Unknown(String),
}
impl Refusal {
    pub(crate) fn explain(&self, who: &str) -> String {
        match self {
            Self::Working => {
                format!("{who} is working: its turn has not ended. Wait for it to finish")
            }
            Self::Dialog => format!("{who} shows a dialog: ask the user to answer it"),
            Self::Draft(text) => format!(
                "{who} has draft text in its input: \"{text}\". Do not type over it; ask the user whether to send or clear it"
            ),
            Self::Unknown(detail) => {
                format!("{who} is in an unknown state ({detail}): inspect its pane before retrying")
            }
        }
    }
}
pub(crate) fn refusal(live: &Value, screen: &str) -> Option<Refusal> {
    if can_prompt(live, screen) {
        return None;
    }
    let agent = live["agent"].as_str().unwrap_or("");
    Some(match live["agent_status"].as_str() {
        Some("blocked") => Refusal::Dialog,
        Some("working") => Refusal::Working,
        _ if busy_on_screen(screen, agent) => Refusal::Working,
        Some("idle" | "done") => input_refusal(live, screen).unwrap_or_else(|| {
            Refusal::Unknown("its input line looks empty but was not accepted".into())
        }),
        other => Refusal::Unknown(format!("Herdr lists it as {}", other.unwrap_or("missing"))),
    })
}
pub(crate) fn input_refusal(live: &Value, screen: &str) -> Option<Refusal> {
    const QUOTE: usize = 40;
    if needs_human(live, screen) {
        return Some(Refusal::Dialog);
    }
    match composer(screen, live["agent"].as_str().unwrap_or("")) {
        Some("") => None,
        Some(draft) => {
            let mut quoted: String = draft.chars().take(QUOTE).collect();
            if draft.chars().count() > QUOTE {
                quoted.push('…');
            }
            Some(Refusal::Draft(quoted))
        }
        None => Some(Refusal::Unknown(
            "no input line recognized on screen".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placeholder_like_or_multiline_draft_is_occupied() {
        for (agent, screen) in [
            ("codex", "› Ask Codex about orders"),
            ("claude", "❯ Try \"summary\""),
            ("claude", "❯ \n  Unfinished draft\n───"),
            ("codex", "› \n  Unfinished draft"),
        ] {
            assert!(!empty_composer(screen, agent));
        }
    }

    #[test]
    fn a_dialog_above_an_empty_input_line_is_not_free_input() {
        let live = serde_json::json!({"agent": "claude", "agent_status": "done"});
        assert!(can_prompt(&live, "Finished\n❯ "));
        assert!(!can_prompt(
            &live,
            "Permission request: write notes.txt\n  1. Yes\n  2. No\n❯ "
        ));
        assert!(can_prompt(
            &live,
            "⏺ Do you want to keep the old copy? I kept it.\n❯ "
        ));
        assert!(!can_prompt(
            &live,
            "│ Do you want to make this edit?\n│ Yes  No\n❯ "
        ));
    }
    use serde_json::json;

    #[test]
    fn only_tool_lines_that_name_the_command_count_as_repeated_calls() {
        let repeated = |line: &str| {
            rot_signals(&format!("{line}\nout\n{line}\nout\n{line}\n"))
                .iter()
                .any(|(signal, _)| *signal == "repeated_tool_call")
        };
        for line in [
            "$ cargo test",
            "⏺ Bash(cargo test)",
            "● Bash(cargo test)",
            "• Ran cargo test",
        ] {
            assert!(repeated(line), "{line}");
        }
        for line in [
            "Ran 1 shell command",
            "Ran 2 shell commands",
            "  Ran 1 shell command",
            "• Ran 3 shell commands",
            "$ ",
        ] {
            assert!(!repeated(line), "{line}");
        }
    }

    #[test]
    fn settled_stops_are_classified_and_working_output_is_not() {
        let idle = json!({"agent_status":"idle"});
        for line in [
            "Error: getaddrinfo ENOTFOUND api.githubcopilot.com",
            "✗ Execution failed: fetch failed",
            "■ stream error: connect ECONNRESET",
        ] {
            assert_eq!(
                stop_reason(&idle, &format!("work\n{line}\n› ")),
                Some(Stop::ProviderError(line.into())),
                "{line}"
            );
        }
        assert_eq!(
            stop_reason(&idle, "work\n⎿ API Error: 529 overloaded_error\n❯ "),
            Some(Stop::ProviderError(
                "⎿ API Error: 529 overloaded_error".into()
            ))
        );
        assert!(matches!(
            stop_reason(&idle, "You've hit your usage limit. Try again at 3pm.\n› "),
            Some(Stop::Limit(_))
        ));
        assert!(matches!(
            stop_reason(&idle, "Invalid API key · Please run /login\n❯ "),
            Some(Stop::Login(_))
        ));
        assert_eq!(
            stop_reason(
                &json!({"agent_status":"done"}),
                "Do you want to create a.txt?"
            ),
            Some(Stop::Dialog)
        );
        assert_eq!(
            stop_reason(
                &json!({"agent_status":"working"}),
                "editing usage limit handling\nAPI Error: in test"
            ),
            None
        );
        assert_eq!(stop_reason(&idle, "Wrote summary.json\n❯ "), None);
    }

    #[test]
    fn compaction_notices_are_recognized() {
        assert!(compaction_shown("✻ Conversation compacted (ctrl+o for history)\n❯ ").is_some());
        assert!(compaction_shown("• Context compacted\n› ").is_some());
        assert!(compaction_shown("Refactored the compaction module\n❯ ").is_none());
    }
    #[test]
    fn input_gate_preserves_drafts_and_unknown_states() {
        assert!(can_prompt(
            &json!({"agent":"claude","agent_status":"idle"}),
            "result\n❯ "
        ));
        assert!(!can_prompt(
            &json!({"agent":"claude","agent_status":"idle"}),
            "❯ my unfinished question"
        ));
        assert!(!can_prompt(
            &json!({"agent":"codex","agent_status":"blocked"}),
            "› "
        ));
        assert!(!can_prompt(
            &json!({"agent":"codex","agent_status":"idle"}),
            "no recognized composer"
        ));
        assert!(!can_prompt(
            &json!({"agent":"codex","agent_status":"working"}),
            "› "
        ));
    }
    #[test]
    fn a_refusal_names_the_one_cause_observed() {
        let agent = |agent: &str, status: &str| json!({"agent":agent,"agent_status":status});
        assert_eq!(refusal(&agent("claude", "idle"), "result\n❯ "), None);
        assert_eq!(
            refusal(&agent("codex", "working"), "Editing\n› "),
            Some(Refusal::Working)
        );
        assert_eq!(
            refusal(&agent("claude", "blocked"), "anything"),
            Some(Refusal::Dialog)
        );
        assert_eq!(
            refusal(
                &agent("claude", "idle"),
                "Do you want to create a.txt?\n❯ 1. Yes\n  2. No"
            ),
            Some(Refusal::Dialog)
        );
        let long = "please also rename every module and update the docs accordingly";
        assert_eq!(
            refusal(&agent("claude", "done"), &format!("result\n❯ {long}")),
            Some(Refusal::Draft(
                "please also rename every module and upda…".into()
            ))
        );
        assert_eq!(
            refusal(&agent("copilot", "idle"), "╻▄▄\n┃ fix the test\n╹▀▀\n"),
            Some(Refusal::Draft("fix the test".into()))
        );
        assert!(matches!(
            refusal(&agent("codex", "idle"), "no recognized composer"),
            Some(Refusal::Unknown(_))
        ));
        assert!(matches!(
            refusal(&agent("codex", "unknown"), "› "),
            Some(Refusal::Unknown(detail)) if detail.contains("unknown")
        ));
    }
    #[test]
    fn a_copilot_turn_on_screen_is_working_whatever_herdr_lists() {
        let composer = "╻▄▄▄▄\n┃\n╹▀▀▀▀\n";
        let running = format!(
            "○ Shell Wait 120 seconds\n  sleep 120\n{composer} ◎ Running bash · 53s · 1.2 KiB ctrl+c interrupt Copilot Fusion → GPT-6 Luna · (8%)\n"
        );
        let settled = format!(
            "● READY\n{composer} Interactive · Manual Approval · / commands      Copilot Fusion → GPT-6 Luna · (8%)\n"
        );
        let mut live = json!({"agent":"copilot","agent_status":"done"});
        assert!(!can_prompt(&live, &running));
        assert!(can_prompt(&live, &settled));
        apply_screen_state(&mut live, &running);
        assert_eq!(live["agent_status"], "working");
        let mut claude = json!({"agent":"claude","agent_status":"done"});
        apply_screen_state(&mut claude, &running);
        assert_eq!(claude["agent_status"], "done");
    }
    #[test]
    fn a_faint_prompt_suggestion_is_no_draft_but_typed_text_is() {
        let claude = json!({"agent":"claude","agent_status":"done"});
        let suggested =
            "⏺ Done.\r\n\x1b[0m❯\u{a0}\x1b[0m\x1b[2m終わったら教えてください\x1b[0m\r\n──\r\n";
        assert_eq!(plain_screen(suggested), "⏺ Done.\n❯\u{a0}\n──\n");
        assert!(can_prompt(&claude, &plain_screen(suggested)));
        for typed in [
            "\x1b[0m❯ \x1b[0m終わったら教えて\x1b[0m",
            "\x1b[0m❯ \x1b[2m\x1b[22mdraft",
            "\x1b[0m❯ \x1b[38;2;200;200;200mdraft",
        ] {
            assert!(!can_prompt(&claude, &plain_screen(typed)), "{typed:?}");
        }
        assert_eq!(plain_screen("\x1b[2m  ⎿ output\x1b[0m"), "  ⎿ output");
        assert_eq!(plain_screen("\x1b]0;title\x07\x1b(B❯ 1. Yes"), "❯ 1. Yes");
    }

    const STUCK_TOP: &str = "⏺ Running unit tests after the gate changes\n  ⎿  $ cargo test --locked --bin hgm 2>&1 | grep -E \"test\n     result|FAILED|panicked|^error\" -A8 | head -60\n\n";

    #[test]
    fn timers_token_counts_tips_and_retries_are_not_progress() {
        let retrying = |attempt: u32, wait: u32| {
            format!("{STUCK_TOP}✻ API error · Retrying in {wait}s · attempt {attempt}/10\n")
        };
        let thinking = |time: &str, tokens: &str| {
            format!(
                "{STUCK_TOP}✽ Nebulizing… ({time} · ↓ {tokens} tokens · thinking with xhigh effort)\n  ⎿  Tip: Use /clear to start fresh when switching topics and free up context\n"
            )
        };
        let view = progress_view(&retrying(1, 0), "claude");
        assert_eq!(view, progress_view(&retrying(4, 8), "claude"));
        assert_eq!(view, progress_view(&thinking("1m 02s", "1.2k"), "claude"));
        assert_eq!(view, progress_view(&thinking("55m 22s", "31.4k"), "claude"));
        assert!(view.contains("Running unit tests"), "{view}");
        let more = format!("{}  ⎿  test result: ok. 59 passed\n", thinking("2m", "2k"));
        assert_ne!(view, progress_view(&more, "claude"));
        let codex = |t: &str| format!("• Ran cargo test\n• Working ({t} • esc to interrupt)\n› ");
        assert_eq!(
            progress_view(&codex("12s"), "codex"),
            progress_view(&codex("1m 40s"), "codex")
        );
        let copilot = |t: &str| {
            format!("○ Shell cargo test\n◎ Running bash · {t} · 1.2 KiB ctrl+c interrupt\n")
        };
        assert_eq!(
            progress_view(&copilot("5s"), "copilot"),
            progress_view(&copilot("53s"), "copilot")
        );
        assert_eq!(
            progress_view("done\n❯ draft", "claude"),
            progress_view("done\n❯ ", "claude")
        );
    }

    #[test]
    fn a_retry_notice_is_named_as_the_cause() {
        let screen = format!("{STUCK_TOP}✻ API error · Retrying in 0s · attempt 1/10\n");
        assert_eq!(
            retry_line(&screen).as_deref(),
            Some("API error · Retrying in 0s · attempt 1/10")
        );
        assert_eq!(
            retry_line(&format!("{STUCK_TOP}✽ Nebulizing… (55m 22s)\n")),
            None
        );
    }

    #[test]
    fn the_enotfound_turn_end_of_12_49_is_a_provider_error() {
        let live = json!({"agent":"claude","agent_status":"idle"});
        let screen =
            "⏺ Writing the plan\n  ⎿  API Error: Can't reach the API server (ENOTFOUND)\n\n❯ ";
        assert!(matches!(
            stop_reason(&live, screen),
            Some(Stop::ProviderError(_))
        ));
    }

    #[test]
    fn native_goal_notices_are_read_from_the_screen() {
        use NativeGoal as N;
        let read = |line: &str| native_goal(&format!("⏺ Working\n{line}\n❯ "));
        assert_eq!(read("◎ /goal active · 4m"), Some(N::Active));
        assert!(matches!(
            read("Goal still active: retrying in 30s (overloaded)"),
            Some(N::Retrying(_))
        ));
        assert!(matches!(
            read("Goal paused: usage limit reached"),
            Some(N::Paused(_))
        ));
        assert!(matches!(
            read(
                "Goal cleared after an unrecoverable error (credit balance). Run /goal again to continue"
            ),
            Some(N::ErrorCleared(_))
        ));
        assert!(matches!(
            read("Goal cleared: all tests pass"),
            Some(N::Cleared(_))
        ));
        assert!(matches!(read("No goal set"), Some(N::Cleared(_))));
        assert!(matches!(
            read("✓ Goal achieved: all tests pass"),
            Some(N::Met(_))
        ));
        assert!(matches!(
            read("✗ Goal failed: the API was removed"),
            Some(N::Impossible(_))
        ));
        assert!(matches!(
            read("Unknown command: /goal"),
            Some(N::Unavailable(_))
        ));
        assert_eq!(read("all quiet"), None);
    }

    const OBSERVED_ACTIVE: &str = "◎ /goal active (12s)";
    const OBSERVED_MET: &str = "✔ Goal achieved (59s · 1 turn · 1.9k tokens)";

    #[test]
    fn the_native_goal_lines_observed_on_claude_code_are_read() {
        let screen = |line: &str| format!("⏺ Ran the tests\n{line}\n❯ ");
        assert_eq!(
            native_goal(&screen(OBSERVED_ACTIVE)),
            Some(NativeGoal::Active)
        );
        assert_eq!(
            native_goal(&screen(OBSERVED_MET)),
            Some(NativeGoal::Met(OBSERVED_MET.into()))
        );
        let working = |t: &str| format!("⏺ Ran the tests\n◎ /goal active ({t})\n");
        assert_eq!(
            progress_view(&working("12s"), "claude"),
            progress_view(&working("4m 48s"), "claude")
        );
    }

    #[test]
    fn a_running_tools_clock_is_not_progress() {
        let screen = |tool: &str, spinner: &str| {
            format!(
                "⏺ Counting files matching hgm-never-exists-* across root in foreground · {tool}\n     (ctrl+b to run in background)\n· Cerebrating… ({spinner} · ↓ 247 tokens)\n────\n❯ \n"
            )
        };
        assert_eq!(
            progress_view(&screen("5m 2s", "5m 5s"), "claude"),
            progress_view(&screen("5m 9s", "5m 12s").replacen("⏺ ", "  ", 1), "claude")
        );
        assert!(progress_view(&screen("12s", "14s"), "claude").contains("Counting files"));
        assert_ne!(
            progress_view(&screen("5m 2s", "5m 5s"), "claude"),
            progress_view(
                &screen("5m 2s", "5m 5s").replace("Counting files", "Listing files"),
                "claude"
            )
        );
        assert_eq!(without_elapsed("a · b"), "a · b");
        assert_eq!(without_elapsed("Ran tests · 1h 3m"), "Ran tests");
    }

    #[test]
    fn an_outcome_printed_above_a_queued_goal_set_is_found() {
        let screen = "⏺ 待機\n✔ Goal achieved (4s · 1 turn · 647 tokens)\n✻ Sautéed for 5s · done 3:05 AM\n❯ Goal set: hgm Goal Manager (03:05:34): done when\n⏺ 待機\n────\n❯ \n";
        assert_eq!(native_goal(screen), Some(NativeGoal::Active), "bottom-up");
        assert_eq!(
            unseen_outcome(screen),
            Some(NativeGoal::Met(
                "✔ Goal achieved (4s · 1 turn · 647 tokens)".into()
            ))
        );
        assert!(!goal_indicator_shown(screen));
        let paused = "⏺ Goal paused · goal checks kept finding it unmet\n    ◎ /goal active (59s)\n────\n❯ \n";
        assert_eq!(unseen_outcome(paused), None);
        assert!(goal_indicator_shown(paused));
        assert_eq!(unseen_outcome("  ✔ Goal achieved (9s)\n❯ "), None);
    }

    #[test]
    fn the_active_indicator_left_after_a_pause_does_not_hide_the_outcome() {
        const PAUSED: &str = "⏺ Goal paused · goal checks kept finding it unmet this turn · send a message to continue";
        let paused = format!(
            "  ⎿  Goal set: hgm goal g-1 task work: the assigned work is done\n◯ Goal not yet met… continuing\n{PAUSED}\n✻ Cogitated for 41s · done 11:36 PM\n    ◎ /goal active (59s)\n────\n❯ \n────\n"
        );
        assert_eq!(
            native_goal(&paused),
            Some(NativeGoal::Paused(PAUSED.into()))
        );
        let again = format!(
            "{OBSERVED_MET}\n❯ /goal hgm goal g-1 task next\n  ⎿  Goal set: hgm goal g-1 task next\n✻ Working…\n    {OBSERVED_ACTIVE}\n❯ \n"
        );
        assert_eq!(native_goal(&again), Some(NativeGoal::Active));
        assert_eq!(
            native_goal(&format!("⏺ Working\n{OBSERVED_ACTIVE}\n❯ ")),
            Some(NativeGoal::Active)
        );
    }
}
