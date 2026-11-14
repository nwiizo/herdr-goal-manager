//! Screen heuristics for the original and manager agents. They read product
//! text that changes between versions, so each result is a cue to inspect,
//! never proof of success or failure.
use serde_json::Value;

/// Lines of an approval, trust or question dialog.
const PROMPTS: [&str; 5] = [
    "Do you want to",
    "Would you like to run",
    "Do you trust the files",
    "Permission request",
    "requires approval",
];
/// Expired sign-in or credentials (matched in lower case).
const LOGIN: [&str; 7] = [
    "please log in",
    "please login",
    "run /login",
    "not logged in",
    "token has expired",
    "could not be refreshed",
    "invalid api key",
];
/// Usage or rate limits (matched in lower case).
const LIMIT: [&str; 6] = [
    "usage limit",
    "rate limit reached",
    "rate limit exceeded",
    "limit reached",
    "hit your limit",
    "quota exceeded",
];
/// Compaction notices (matched in lower case).
const COMPACTION: [&str; 4] = [
    "conversation compacted",
    "context compacted",
    "compacted (ctrl+o",
    "compaction complete",
];

/// A case-insensitive regular expression for output that needs a look even
/// while the agent's state does not change: dialogs, sign-in, limits and
/// compaction. Herdr reports a match without a status change.
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

/// Herdr can classify a visible approval dialog as `done` (Copilot file creation,
/// Herdr 0.9.5), so the screen is checked as well as the native state.
pub(crate) fn needs_human(live: &Value, screen: &str) -> bool {
    live["agent_status"] == "blocked"
        || screen
            .lines()
            .rev()
            .take(30)
            .any(|line| PROMPTS.iter().any(|prompt| line.contains(prompt)))
}

/// Context-rot signs visible on screen: reflexive agreement, or the same tool
/// call line shown three or more times. Each comes with the line that shows it,
/// so one sign that stays visible across observations is counted once.
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

/// Whether `line` shows a tool call together with its command: a shell prompt
/// (`$ cargo test`), Claude Code's `Bash(...)` or Codex's `• Ran cargo test`.
/// A collapsed summary such as Claude Code's "Ran 2 shell commands" names no
/// command, so identical summaries are not a repeated call.
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

/// Why a settled agent stopped, read from the last lines of its screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    /// An approval, trust or question dialog: only a person answers it.
    Dialog,
    /// Expired login or credentials: only a person can sign in again.
    Login(String),
    /// A usage or rate limit: a person decides whether to wait or switch.
    Limit(String),
    /// A transient provider failure ended the turn; a short continue may help.
    ProviderError(String),
}

/// Distinguishes stops a person must resolve from a transient provider error.
/// Only a settled agent is classified: a working agent's output may mention
/// these words while it edits code about them.
pub(crate) fn stop_reason(live: &Value, screen: &str) -> Option<Stop> {
    // Network failures after a sleep or a dropped connection read like these
    // (e.g. `getaddrinfo ENOTFOUND`): the turn ended, the work did not.
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

/// A compaction notice near the end of the screen (Claude Code, Codex and
/// Copilot print one after summarizing the conversation).
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
/// Conservative known native input markers; unknown/draft inputs are never sent to.
/// Plain text of a screen read with `--format ansi`. Faint text (SGR 2) right
/// after a line's leading `❯` is left out: Claude Code draws its input
/// placeholder and its suggested next prompt there, which nobody typed, while
/// a real draft is drawn in normal intensity.
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
                    // OSC: up to BEL or the string terminator.
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
                    if c == '❯' && !faint && text[start..].trim().is_empty() {
                        prompt = true;
                    }
                    text.push(c);
                }
            }
        }
    }
    text
}
/// Whether text is faint after the SGR `parameters`, starting from `faint`.
fn sgr_faint(parameters: &str, mut faint: bool) -> bool {
    let codes: Vec<&str> = parameters.split(';').collect();
    let mut index = 0;
    while index < codes.len() {
        match codes[index] {
            "" | "0" | "22" => faint = false,
            "2" => faint = true,
            // Extended colors: `5;n` or `2;r;g;b` follow and are not styles.
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
    if agent == "copilot" {
        let lines: Vec<_> = screen.lines().rev().take(8).map(str::trim).collect();
        return lines
            .windows(3)
            .any(|w| w[0].starts_with("╹") && w[1] == "┃" && w[2].starts_with("╻"));
    }
    for line in screen.lines().rev().take(16) {
        let line = line.trim();
        let tail = match agent {
            "claude" => line.strip_prefix('❯'),
            "codex" => line.strip_prefix('›'),
            _ => None,
        };
        if let Some(tail) = tail {
            let tail = tail.trim();
            return tail.is_empty()
                || (agent == "codex" && tail.starts_with("Ask Codex "))
                || (agent == "claude" && tail.starts_with("Try \"") && tail.ends_with('"'));
        }
    }
    false
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
/// Corrects `live`'s state with what the screen shows (see [`busy_on_screen`]),
/// so a running turn is never taken for a settled one.
pub(crate) fn apply_screen_state(live: &mut Value, screen: &str) {
    if busy_on_screen(screen, live["agent"].as_str().unwrap_or("")) {
        live["agent_status"] = "working".into();
    }
}
pub(crate) fn can_prompt(live: &Value, screen: &str) -> bool {
    let agent = live["agent"].as_str().unwrap_or("");
    matches!(live["agent_status"].as_str(), Some("idle" | "done"))
        && !busy_on_screen(screen, agent)
        && empty_composer(screen, agent)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        // Claude Code collapses tool calls into summaries without the command.
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
        // Network failures after a sleep end the turn, whatever the wording.
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
    fn a_copilot_turn_on_screen_is_working_whatever_herdr_lists() {
        // Herdr 0.9.5 listed this Copilot 1.0.110 pane `done` mid-tool.
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
        // Other providers keep Herdr's state.
        let mut claude = json!({"agent":"claude","agent_status":"done"});
        apply_screen_state(&mut claude, &running);
        assert_eq!(claude["agent_status"], "done");
    }
    #[test]
    fn a_faint_prompt_suggestion_is_no_draft_but_typed_text_is() {
        let claude = json!({"agent":"claude","agent_status":"done"});
        // Claude Code 2.1 suggests a next prompt in faint text (as Herdr reads it).
        let suggested =
            "⏺ Done.\r\n\x1b[0m❯\u{a0}\x1b[0m\x1b[2m終わったら教えてください\x1b[0m\r\n──\r\n";
        assert_eq!(plain_screen(suggested), "⏺ Done.\n❯\u{a0}\n──\n");
        assert!(can_prompt(&claude, &plain_screen(suggested)));
        // The user's own draft is normal text, also after a reset or color.
        for typed in [
            "\x1b[0m❯ \x1b[0m終わったら教えて\x1b[0m",
            "\x1b[0m❯ \x1b[2m\x1b[22mdraft",
            "\x1b[0m❯ \x1b[38;2;200;200;200mdraft",
        ] {
            assert!(!can_prompt(&claude, &plain_screen(typed)), "{typed:?}");
        }
        // Faint text elsewhere stays: only the input line drops it.
        assert_eq!(plain_screen("\x1b[2m  ⎿ output\x1b[0m"), "  ⎿ output");
        assert_eq!(plain_screen("\x1b]0;title\x07\x1b(B❯ 1. Yes"), "❯ 1. Yes");
    }
}
