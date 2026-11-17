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
    composer(screen, agent) == Some("")
}
/// Text typed into the agent's input line: empty for an empty line or one that
/// shows only the agent's placeholder, `None` when no known input line is on screen.
fn composer<'a>(screen: &'a str, agent: &str) -> Option<&'a str> {
    if agent == "copilot" {
        let lines: Vec<_> = screen.lines().rev().take(8).map(str::trim).collect();
        return lines.windows(3).find_map(|w| {
            (w[0].starts_with("╹") && w[2].starts_with("╻"))
                .then(|| w[1].strip_prefix('┃').map(str::trim))
                .flatten()
        });
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
            let placeholder = (agent == "codex" && tail.starts_with("Ask Codex "))
                || (agent == "claude" && tail.starts_with("Try \"") && tail.ends_with('"'));
            return Some(if placeholder { "" } else { tail });
        }
    }
    None
}
/// Glyphs Claude Code's status line cycles through while a turn runs.
const CLAUDE_SPINNER: [char; 7] = ['✻', '✽', '✶', '✳', '✢', '·', '*'];
/// Whether a line changes by itself while a turn runs, whatever the work does:
/// a spinner or status line with elapsed time and token counts, a tip, an
/// interrupt hint, or a retry counter. Claude Code, Codex and Copilot each
/// have their own status line; an unknown provider gets the shared rules.
fn volatile(line: &str, agent: &str) -> bool {
    // `◎ /goal active (12s)`: the native goal's own timer, seen on Claude Code.
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
        // `✽ Nebulizing… (55m 22s · ↓ 31.4k tokens · thinking …)`
        "claude" => {
            trimmed
                .chars()
                .next()
                .is_some_and(|c| CLAUDE_SPINNER.contains(&c))
                && (trimmed.contains('…') || elapsed())
        }
        // `• Working (1m 02s • esc to interrupt)`
        "codex" => (trimmed.starts_with('•') || trimmed.starts_with('◦')) && elapsed(),
        // `◎ Running bash · 53s · 1.2 KiB ctrl+c interrupt`
        "copilot" => (trimmed.starts_with('◎') || trimmed.starts_with('∙')) && elapsed(),
        _ => false,
    }
}
/// The screen as evidence of progress: the transcript above the input line
/// without the lines that change by themselves while a turn runs. A working
/// agent whose progress view stays the same shows no progress, however its
/// timers move; output lines that grow are progress.
pub(crate) fn progress_view(screen: &str, agent: &str) -> String {
    let lines: Vec<&str> = screen.lines().collect();
    let input = lines.iter().rposition(|line| {
        let line = line.trim_start();
        line.starts_with('❯') || line.starts_with('›') || line.starts_with('╻')
    });
    lines[..input.unwrap_or(lines.len())]
        .iter()
        .filter(|line| !line.trim().is_empty() && !volatile(line, agent))
        // A running tool's bullet blinks (`⏺ Counting…`, then `  Counting…`).
        .map(|line| without_elapsed(line.trim().trim_start_matches(['⏺', ' '])))
        .collect::<Vec<_>>()
        .join("\n")
}
/// `line` without a trailing elapsed time such as ` · 5m 2s`. Claude Code
/// 2.1.325 ends a running tool's line with it (`⏺ Counting files … in
/// foreground · 5m 2s`, 2026-11-16): the line stays evidence, its clock does not.
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
/// A provider's retry notice on screen, such as `API error · Retrying in 0s ·
/// attempt 1/10`: the likely cause when a working agent shows no progress.
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
/// What Claude Code's native goal (`/goal`) shows on screen, from its
/// documentation (code.claude.com/docs/en/goal, Claude Code 2.1.269+). The
/// met and impossible wording is not quoted there, so those two are matched
/// loosely; all of it awaits a native check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NativeGoal {
    /// `◎ /goal active`: the goal is set and drives the worker.
    Active,
    /// `Goal still active …`: Claude Code retries a transient error itself.
    Retrying(String),
    /// The evaluator found the condition met.
    Met(String),
    /// The evaluator judged the condition impossible.
    Impossible(String),
    /// `Goal paused …`: the goal stays set but waits.
    Paused(String),
    /// `Goal cleared after an unrecoverable error … Run /goal again to continue`.
    ErrorCleared(String),
    /// `Goal cleared:` or `No goal set` after `/goal clear`.
    Cleared(String),
    /// `/goal` is not offered (hooks disabled, an older version).
    Unavailable(String),
}
/// The latest native-goal notice in the last lines of `screen`.
pub(crate) fn native_goal(screen: &str) -> Option<NativeGoal> {
    // `◎ /goal active (42s)` sits above the input line and stayed there, its
    // timer stopped, after `Goal paused` (Claude Code 2.1.325): it only counts
    // when no outcome follows the newest `Goal set:` in the transcript.
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
/// Lines an agent printed itself: quoted text (a wake, a tool's output, a
/// typed message) is indented under its marker, so a goal notice quoted from
/// another agent is not read as this one's.
pub(crate) fn own_lines(screen: &str) -> String {
    screen
        .lines()
        .filter(|line| !line.starts_with(char::is_whitespace))
        .collect::<Vec<_>>()
        .join("\n")
}
/// Whether Claude Code shows `◎ /goal active` in the lines right above its
/// input line, where it keeps the indicator while a goal is set (also after
/// `Goal paused`, with its clock stopped).
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
/// How a goal that ended unseen ended: the newest outcome Claude Code printed
/// itself, `Goal set:` echoes aside. A goal queued during a turn and met at the
/// stop that ran it prints its outcome above the echoed `Goal set:` (seen
/// natively, 2026-11-16), where reading from the bottom finds the set first.
/// Paused and unavailable keep or never had a goal, so they name nothing here.
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
/// Corrects `live`'s state with what the screen shows (see [`busy_on_screen`]),
/// so a running turn is never taken for a settled one.
pub(crate) fn apply_screen_state(live: &mut Value, screen: &str) {
    if busy_on_screen(screen, live["agent"].as_str().unwrap_or("")) {
        live["agent_status"] = "working".into();
    }
}
/// Whether a dialog may show: one of its phrases in the last lines, failing
/// closed (a box, unnumbered choices or an unknown layout all count). The one
/// exception is a line the agent wrote itself, which Claude Code starts with
/// `⏺` (\"⏺ Do you want to keep the old copy? …\").
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
        // A dialog can leave an empty composer on screen (Herdr may list the
        // agent as done): it is never typed into.
        && !dialog_shown(screen)
}

/// The one observed reason an agent cannot take typed input now, so a refusal
/// tells the manager whether to wait or to ask the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Its turn has not ended.
    Working,
    /// A dialog waits for an answer.
    Dialog,
    /// The input line holds text nobody sent (quoted, shortened).
    Draft(String),
    /// Neither Herdr's state nor the screen shows a known input state.
    Unknown(String),
}
impl Refusal {
    /// A sentence naming the cause for `who` and the next step, without a final period.
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
/// Why the agent cannot take a prompt; `None` exactly when [`can_prompt`] holds.
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
/// What on screen keeps typed input from reaching the agent, whatever its
/// state: a dialog, draft text, or no input line hgm recognizes.
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
    fn a_dialog_above_an_empty_input_line_is_not_free_input() {
        // UC-1: Herdr may list the agent as done while a dialog shows above
        // an empty composer.
        let live = serde_json::json!({"agent": "claude", "agent_status": "done"});
        assert!(can_prompt(&live, "Finished\n❯ "));
        assert!(!can_prompt(
            &live,
            "Permission request: write notes.txt\n  1. Yes\n  2. No\n❯ "
        ));
        // UCV-1: the same words in ordinary text are no dialog.
        assert!(can_prompt(
            &live,
            "⏺ Do you want to keep the old copy? I kept it.\n❯ "
        ));
        // A boxed dialog without numbered choices still counts.
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

    /// The transcript main showed from 10:49 on 2026-11-15, above its status line.
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
        // New output is progress.
        let more = format!("{}  ⎿  test result: ok. 59 passed\n", thinking("2m", "2k"));
        assert_ne!(view, progress_view(&more, "claude"));
        // Codex and Copilot status lines, and the input line, are not progress.
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
        // Quoted in Claude Code's documentation.
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
        // Assumed wording, pending a native check.
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

    /// Observed on a real Claude Code with `/goal` (2026-11-15).
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
        // The active line's timer is not progress, so a goal that drives a
        // stuck worker is still noticed.
        let working = |t: &str| format!("⏺ Ran the tests\n◎ /goal active ({t})\n");
        assert_eq!(
            progress_view(&working("12s"), "claude"),
            progress_view(&working("4m 48s"), "claude")
        );
    }

    #[test]
    fn a_running_tools_clock_is_not_progress() {
        // Claude Code 2.1.325, a silent `find /` five minutes in (2026-11-16).
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
        // Another tool is progress; a middle dot inside ordinary text stays.
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
        // Natively (2026-11-16) a queued goal met at the stop that ran it.
        let screen = "⏺ 待機\n✔ Goal achieved (4s · 1 turn · 647 tokens)\n✻ Sautéed for 5s · done 3:05 AM\n❯ Goal set: hgm Goal Manager (03:05:34): done when\n⏺ 待機\n────\n❯ \n";
        assert_eq!(native_goal(screen), Some(NativeGoal::Active), "bottom-up");
        assert_eq!(
            unseen_outcome(screen),
            Some(NativeGoal::Met(
                "✔ Goal achieved (4s · 1 turn · 647 tokens)".into()
            ))
        );
        assert!(!goal_indicator_shown(screen));
        // A paused goal names nothing: it keeps its goal and its indicator.
        let paused = "⏺ Goal paused · goal checks kept finding it unmet\n    ◎ /goal active (59s)\n────\n❯ \n";
        assert_eq!(unseen_outcome(paused), None);
        assert!(goal_indicator_shown(paused));
        // Quoted (indented) outcomes are not this agent's.
        assert_eq!(unseen_outcome("  ✔ Goal achieved (9s)\n❯ "), None);
    }

    #[test]
    fn the_active_indicator_left_after_a_pause_does_not_hide_the_outcome() {
        // Claude Code 2.1.325 (2026-11-15): after nine blocked stops the goal
        // paused, and the indicator above the input line stayed at 59s.
        const PAUSED: &str = "⏺ Goal paused · goal checks kept finding it unmet this turn · send a message to continue";
        let paused = format!(
            "  ⎿  Goal set: hgm goal g-1 task work: the assigned work is done\n◯ Goal not yet met… continuing\n{PAUSED}\n✻ Cogitated for 41s · done 11:36 PM\n    ◎ /goal active (59s)\n────\n❯ \n────\n"
        );
        assert_eq!(
            native_goal(&paused),
            Some(NativeGoal::Paused(PAUSED.into()))
        );
        // A goal set after an earlier outcome is active again, and the
        // indicator alone still reads as active.
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
