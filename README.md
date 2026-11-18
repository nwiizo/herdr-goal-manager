# Herdr Goal Manager

Talk through a problem with Claude Code, Codex, or Copilot, ask the agent in
your original tab to do the work, and keep evidence of the result. The manager
opens a separate Herdr tab. Your original target stays pinned when focus changes.
Git, a software project, and ccswarm are optional.

This is an experimental local tool. It has bounded monitoring, a durable
handoff protocol and an optional [factory mode](#factory-mode-optional): a local
queue of agreed contracts, taken one at a time and held while your review falls
behind. It does not create, update or merge pull requests, take work from
GitHub issues, Slack or other services, or start several agent loops in
parallel. See [verification and limits](docs/verification.md) before unattended use.
What hgm shows you, the Goal Manager and the worker, and why, is in
[interaction design](docs/design.md); prompt changes are reviewed against the
[prompt checklist](docs/prompt-checklist.md).

## Install

You need macOS or Linux, [Herdr](https://herdr.dev) 0.9.5 or later, and at
least one agent CLI. hgm is one binary plus a Herdr plugin manifest.

### 1. Herdr and an agent CLI

- Install [Herdr](https://herdr.dev) with its installer or Homebrew, then start
  it with `herdr` and check `herdr --version` (0.9.5 or later). Goal Manager
  runs in Herdr tabs.

  ```sh
  curl -fsSL https://herdr.dev/install.sh | sh   # or: brew install herdr
  ```
- Install at least one agent CLI, sign in, and start it once in a terminal so
  its own first-run questions (sign-in, folder trust) are answered:
  [Claude Code](https://github.com/anthropics/claude-code) (`claude`),
  [Codex](https://github.com/openai/codex) (`codex`) or
  [GitHub Copilot CLI](https://github.com/github/copilot-cli) (`copilot`).

### 2. The plugin

Pick one of these ways.

**From GitHub through Herdr.** Herdr fetches the tag and runs the plugin's build
step, `cargo build --release --locked`, so this needs Rust 1.89 or later with
`cargo` on `PATH` (for example from [rustup.rs](https://rustup.rs)):

```sh
herdr plugin install nwiizo/herdr-goal-manager --ref v0.1.2
```

**Prebuilt archive** (no Rust needed). Download the archive for your machine,
check it, and extract it into a folder that stays: Herdr links to that folder.
Run these lines in bash or zsh (from fish, start `bash` first). The Linux
archives are built on Ubuntu 24.04; if the binary does not start on an older
distribution (an older glibc), build from a checkout instead.

```sh
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)  target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-aarch64) target=aarch64-unknown-linux-gnu ;;
  Linux-x86_64)  target=x86_64-unknown-linux-gnu ;;
  *) echo "no prebuilt archive for $(uname -sm); build from a checkout" ;;
esac
base=https://github.com/nwiizo/herdr-goal-manager/releases/download/v0.1.2
mkdir -p ~/.local/share && cd ~/.local/share
curl -fLO "$base/hgm-v0.1.2-$target.tar.gz"
curl -fLO "$base/SHA256SUMS"
shasum -a 256 -c SHA256SUMS --ignore-missing  # Linux: sha256sum -c SHA256SUMS --ignore-missing
tar -xzf "hgm-v0.1.2-$target.tar.gz"
"hgm-v0.1.2-$target/target/release/hgm" --version  # hgm 0.1.2
herdr plugin link "$PWD/hgm-v0.1.2-$target"
```

On macOS, Gatekeeper may block an archive downloaded with a browser; download it
with `curl` as above, or clear the attribute with
`xattr -dr com.apple.quarantine "hgm-v0.1.2-$target"`.

**From a checkout** (Rust 1.89 or later):

```sh
git clone --branch v0.1.2 https://github.com/nwiizo/herdr-goal-manager
cd herdr-goal-manager
cargo build --release --locked
target/release/hgm --version  # hgm 0.1.2
herdr plugin link "$PWD"
```

**From crates.io**, if the ways above do not work for you (Rust 1.89 or
later). `cargo install` installs only the `hgm` binary; `hgm plugin-folder`
then makes a plugin folder around it (the manifest and a link to the binary,
by default in `~/.local/share/herdr-goal-manager/plugin`) for Herdr to link:

```sh
cargo install herdr-goal-manager --locked
hgm --version  # hgm 0.1.2
hgm plugin-folder
herdr plugin link ~/.local/share/herdr-goal-manager/plugin
```

A later `cargo install herdr-goal-manager --locked` upgrades the plugin too,
since the folder links to the installed binary.

Check that `herdr plugin list` shows `herdr-goal-manager` as enabled.

### 3. A shortcut

Goal Manager opens through a key bound to the plugin's `open` action. Add this to
Herdr's config, `~/.config/herdr/config.toml` (or the file `HERDR_CONFIG_PATH`
names), and pick another key if `prefix+ctrl+g` is taken:

```toml
[[keys.command]]
key = "prefix+ctrl+g"
type = "plugin_action"
command = "herdr-goal-manager.open"
description = "open goal manager for this folder"
```

Validate it with `herdr config check` and apply it with
`herdr server reload-config` (or **prefix, Shift+R**). The shortcut is then
**prefix, Ctrl+G**: Herdr's prefix key (`prefix` in its config), then Ctrl+G.

### 4. First run

1. In Herdr, open a tab (**prefix, c**), change to the folder you work in, and
   start your agent there (`claude`, `codex` or `copilot`). This tab is main:
   the agent that does the work.
2. Press the shortcut from that tab. A Goal Manager tab opens with the same
   agent and asks what you want to move forward. If its provider asks whether to
   trust the folder, answer it in that tab. With Copilot, set a capable model
   for the manager first (`models.copilot`, see Limits).
3. Describe the problem or the result you want. Small requests go straight to
   main; for larger work the manager writes a contract, shows it to you and
   sends the work once you agree. It keeps the goal records (no YAML to write)
   and leaves acceptance to you.

Pressing the shortcut again from the same tab switches to its running manager.
To run `hgm` in your own shell as well (`hgm overview`, `hgm log`), link the
plugin's `target/release/hgm` onto your `PATH`: inside the archive or checkout
folder, or for a GitHub install under
`~/.config/herdr/plugins/github-installations/`. For the archive above:
`ln -s ~/.local/share/hgm-v0.1.2-$target/target/release/hgm ~/.local/bin/hgm`
(any folder on your `PATH`). The managers do not need it.

### Uninstall

Remove the shortcut, then run `herdr plugin uninstall herdr-goal-manager` (if
installed from GitHub) or `herdr plugin unlink herdr-goal-manager` (if linked).
For a crates.io install, also run `cargo uninstall herdr-goal-manager` and delete
the plugin folder. Goal records stay in hgm's private state (see Inspect and
recover) until you delete them.

## The manager

The manager uses the same agent as the original tab; when that tab runs no known
agent it uses `HGM_AGENT`, the config's `agent`, or Codex. The CLI also supports:

```sh
hgm manager --agent claude
hgm manager --agent codex
hgm manager --agent copilot --model MODEL   # pick a capable model, see Limits
```

Install and authenticate the selected provider first. Folder trust and tool
permissions belong to that provider. The manager session runs this build as plain
`hgm`; its `PATH`, `HGM_WORKSPACE`, and `HGM_STATE_DIR` select the folder and
private state. The manager guide is sent as instructions, not as your first
message: Claude `--append-system-prompt`, Codex `developer_instructions`, and a
private Copilot instructions directory (`COPILOT_CUSTOM_INSTRUCTIONS_DIRS`).
Agents hgm launches (the manager, `impl` and `reviewer`) start with their
provider's allow-everything flag so they never stall on a permission prompt:
Claude `--dangerously-skip-permissions`, Codex
`--dangerously-bypass-approvals-and-sandbox`, Copilot `--allow-all`. A Claude
manager's settings also allow Herdr's socket (`HERDR_SOCKET_PATH`) in Claude
Code's sandbox, because every `hgm` and `herdr` call goes through it. Your own
original agent keeps the options you started it with. Opening the manager names the
original tab `main` only when it has no name (Herdr shows an unnamed tab by
its number); a tab you named keeps its name. `HGM_AGENT` selects the plugin's provider
when set in its launch environment. Bare `hgm` starts the conversation. Standalone
use has no original Herdr tab until opened through the plugin.

## What happens to a request

For example: “Read orders.csv, calculate quantities and the total, and write
summary.json.” The manager establishes an observable result, inspects the pinned
original agent, submits the task, and checks the resulting file. A reflection-only
conversation need not create a goal. A clear execution request need not repeat an
interview. The conversation follows [oi-owarasero](https://github.com/nwiizo/oi-owarasero)
principles: understand the actual obstacle, keep the next action small, and judge
whether the recipient can proceed.

Small requests go straight to the original agent with `send`. Larger work gets a
loop contract: the outcome and why, observable acceptance, invariants, boundaries
and what to do at one, prior art, open questions with how to resolve them, and a
`loop` policy. The manager shows it and records your agreement with `confirm`;
the contract is then fixed. Agent assignments go out as `send --goal G --task T`,
which records them on the goal's timeline and starts monitoring.

```yaml
loop:
  max_assignments: 3      # per task; a human revision starts a new budget
  max_hours: 4            # from the first assignment
  patrol_minutes: 3       # check main every 3 minutes while it works; 0 disables
  patrol_wake: finding    # a patrol wakes the manager only with a finding; always: every patrol
  stop_when: ["Auth changes are required"]
  autonomy: auto          # ask: every assignment needs your recorded approval
  handoff_percent: 85     # original-agent context that triggers handoff
```

The original agent makes; the manager and registered checks verify. `auto`
autonomy needs at least one command check. Every linked assignment stays under
400 words and carries the binding contract; with `plan_first`, implementation
waits for `approve-plan`. `protected_paths` and machine `stop_checks` stop the
loop mechanically.

A contract can ask for its own checkout:
`"checkout": {"repository": "/abs/repo", "base": "main", "branch": "goal/x"}`
(`repository` defaults to the workspace; an optional absolute `path` places it).
`confirm` creates it with `herdr worktree create`, so it appears as a Herdr
workspace, or reopens the worktree that already holds the branch; a branch
without a worktree is refused. Linked assignments say to work there, and
checks, settle checks, stop checks and `protected_paths` use it.
`hgm checkout remove ID` removes it through Herdr, never with uncommitted
changes, a running watch or a running goal; the branch stays. A failed check becomes a linked fix assignment within budget. A spent budget, a stop
condition, an approval dialog in the original tab, a result awaiting review, or
monitoring that stops abnormally becomes a Herdr notification and an `inbox`
entry. `revise ID --feedback` reopens work you reject; acceptance is always yours.

The watch tab wakes the manager when the worker settles, changes state, shows no
observed progress, or a patrol finds something, with the goal and task of the
latest linked assignment to that pane, that goal's budget line and what changed.
A patrol that finds main working with visible progress and no finding (no
progress, a write outside `write_paths`, a context-rot sign, a regressed check)
is recorded and wakes nobody; `loop.patrol_wake: always` (contract or config)
wakes the manager on every patrol. Goals confirmed before this change have no
such field and get the quiet default. It does not replay prompts or treat an idle agent
as completed work. Defaults: a check of main every three minutes while it works, a wake after three
minutes without change, three minutes between patrol and unchanged-screen wakes, 160 wakeups (about 8 hours of 3-minute checks) per goal/context; [Keep main working](#keep-main-working) lists every time and how to change it. `wait ID --reason` pauses wakeups
without hiding a result awaiting review. Pause/cancel also stop monitoring.

The watch follows Herdr's event stream (agent status changes, attention text,
pane exit) and reads the screen on every change, so a dialog reported as `done`
is still caught. A reconcile read every `watch.reconcile_secs` (30 s) notices a
closed pane or dropped events; when events stop it polls every 10 seconds and
subscribes again later. The Goal watch tab redraws the goal, watched pane and
state, mode, wakes, budgets and the last five timeline events in place, and the
original pane's Herdr sidebar shows goal state and budget until the watch ends.
A sheep runs across the tab and repeats recent notifications and wakes in a
speech bubble. The watch process draws it by itself, with no Herdr call or model
turn; turn it off with `watch: {sheep: false}` in the user or project config.
The watch, roles and a pending handoff belong to the original terminal, so a
reopened Goal Manager in the same tab continues them.

Claude managers receive a session-scoped Stop hook. It continues only when a
new observation is pending, respects pause/budget, and prevents recursive hook
continuation. Other providers use the external watch loop.

## Native goal (`/goal`)

Design rule: where a provider has an official mechanism for something hgm
needs, hgm uses it as documented instead of rebuilding it, and keeps only what
the mechanism lacks. Claude Code's `/goal` keeps a session working toward a
condition between turns; hgm sets it on main and keeps its own checks, budgets,
protected paths and your acceptance.

- **Setting.** `loop.native_goal` in a contract, or as a `loop` default in the
  user or project config: `assignment`, `settle` or `false`. A newly
  registered contract gets `assignment`; a stored contract keeps what it had
  (no field, or `true`, means `settle`). It applies to a Claude Code main under
  autonomy `auto` only; Codex and Copilot stay off. With it off, nothing is
  typed beyond the assignments and every output is unchanged.
- **When hgm sets it.** With `assignment`, right after a linked assignment that
  is not a plan request reaches main, `hgm send` types `/goal <condition>` as
  queued input under the delivery rules (main read again first; an empty input
  line and no dialog), while a watch of the tab observes main. Claude Code runs
  a slash command typed during a turn once the turn ends, so the goal takes over
  where the assignment's turn stops. The set is recorded as `native_goal_queued`
  (pending) and becomes set only when the watch reads `Goal set:` for that goal
  and task; when main sits settled without it, it is recorded as uncertain and
  never typed again. With `settle`, the watch types it once main has settled
  with an empty input line after the assignment, and Claude Code starts a turn
  at once. Either way the settle checks run and the manager is woken when the
  goal ends and main settles. The condition comes from a template in hgm and contract fields only: goal
  and task ids, the report every linked assignment asks for, the registered
  check commands, the boundaries and protected paths, and `or stop after N
  turns` with N = 10 × `loop.max_assignments`. It stays under 700 bytes: Claude
  Code 2.1.325 takes a `/goal` line typed through Herdr that is longer than 800
  characters as pasted text, not as a command; a long check or boundary list is
  then named by reference to the contract in the assignment. No assignment text
  or other model output enters it. hgm reads main again right before typing and
  counts the goal as set only when main answers `Goal set:`; otherwise the set
  is recorded as uncertain. Never for a plan request, an `ask` goal or a helper
  role.
- **When hgm clears it.** `/goal clear` goes to main when the goal becomes ready
  for review, completed, cancelled, paused or waiting, when a stop condition,
  protected path, time or wake budget holds, and before a handoff. Setting and
  clearing follow the delivery rules: a known input state, an empty input line
  and no dialog. A settled main must answer `Goal cleared:` or `No goal set`; a
  working Claude Code takes the clear as queued input and runs it after its
  turn. An uncertain delivery is recorded and not repeated.
- **Outcomes are observations.** The watch reads the outcome from main's screen
  (met, judged impossible, paused, cleared after an error, cleared, unavailable)
  and records it (`native_goal`, `native_goal_unavailable`); the manager is woken
  as for any stop. None of these completes a task or accepts a goal: hgm's
  checks and your acceptance decide. `hgm status`, `hgm show` and the Goal watch
  tab show whether main's native goal and the manager's are set and their last
  outcome (`native_goal`, `manager_goal` with `guarded` after the spin guard).
- **One loop at a time.** While the native goal drives main (no outcome yet, and
  the screen shows it active or retrying), the watch types no continue and no
  prepared step; they apply once it has ended or paused.
- **Unavailable.** When `/goal` is not offered (hooks disabled, an older version,
  an unknown command), hgm records it once and works on without it.
- **The Goal Manager's own goal.** For a Claude Code Goal Manager, the watch
  also sets a short goal on the manager while a confirmed goal of the tab is
  short of completion, and clears it once none is (also when the watch ends).
  Its condition is fixed text with the set time: the manager may stop once
  `hgm status` shows every goal of the tab ready for review, waiting, completed
  or cancelled, or once it has stated plainly that only you can decide the next
  step; until then it ends every turn with `hgm await` running in the
  background. It follows the same delivery rules (queued into a working
  manager with an empty input line, never into a draft or a dialog), is pending
  until its own `Goal set:` shows, and after it ends is set again only after a
  manager action. `manager.native_goal: false` in the user or project config
  turns it off; other providers' managers get nothing.
- **`hgm await`.** The manager runs it in the background at the end of a turn
  (`--timeout SECS`, default 1800). While it runs, the watch hands it the wake
  it would have typed, unchanged, instead of typing into the manager's input,
  so a draft there no longer holds a wake back; the wake is recorded with
  `via: await`. It prints the wake and exits, which starts the manager's next
  turn; it also returns at once when the watch has ended, and after the
  timeout. Without an await, wakes are typed as before. Claude Code holds a
  goal's check while a background command runs, so a manager waiting this way
  does not spin.
- **Spin guard.** When the manager's goal keeps continuing (`Goal not yet met`
  lines Claude Code printed itself, quoted lines aside) three times with no
  `hgm await` running and no manager action in between, hgm clears it once,
  tells you (`attention`, a Herdr notification) and does not set it again for
  that manager session.

Limits: on a real Claude Code 2.1.325 (2026-11-16) hgm set the goal, read its
met and paused outcomes and cleared it on wait, and a `/goal` typed during a
turn was queued and ran after it; the wording for impossible and unavailable is
neither quoted in its documentation nor observed yet, so hgm matches it
loosely. A second native check the same day saw the goal queued with the
assignment and read back, the checks and wake when it ended, wakes handed to a
running `hgm await` past a draft, the manager's goal set and cleared, and the
spin guard clear it once. The manager's goal is read from the screen: its
continuations are counted from lines Claude Code printed itself, so a fast
scroll can undercount them. A queued goal met at the stop that ran it prints its
outcome above its `Goal set:`; on main and on the manager, hgm then reads the
end from the missing `◎ /goal active` indicator once the agent settles (seen
natively on both), and records it as `ended` without a name when the outcome
line has already scrolled out of the lines it reads. Claude Code's evaluator
also accepted a manager saying it waits as an exit ([verification](docs/verification.md)).

## Another provider's check (`loop.cross_check`)

A contract (or a `loop` default in the user or project config) can ask the
watch to have another provider check the worker's work each time it settles
after new work, next to the settle checks: `false` (the default), `true` or
`auto` (Codex checks a Claude Code worker, Claude Code checks the others),
`codex` or `claude`. The watch starts a one-shot, read-only run of that CLI
(`codex exec --sandbox read-only`, or `claude -p` with Read, Grep and Glob
only) on the goal's objective and acceptance and the work dir's `git diff HEAD`
and untracked files, and does not wait for it: a later observation reads the
result, records `cross_checked` (verdict `approve`, `changes_requested`,
`no_verdict` or `timeout` after 15 minutes, with up to ten findings) and wakes
the manager with it. One check runs at a time. Its verdict is an observation:
it completes no task and accepts no goal.

## Keep main working

Keeping main working toward the confirmed goal is part of the manager's goal.
The watch acts on its own where waiting would only cost time:

- **A stop goes at once.** A wake about main settling after new work skips the
  wake cooldown; the cooldown still spaces patrols and unchanged-screen wakes. A
  working Claude Code or Copilot manager receives it as queued input under the
  rules of `send --interrupt` (empty input line, no dialog, the text must leave
  the input line); a Codex manager gets it when its turn ends. A wake whose
  observation is older than an assignment, correction or goal check sent to that
  pane (for any goal) is dropped instead of spent.
- **A prepared next step.** While main works, `hgm send --goal G --task T
  --prepare TEXT` queues a linked assignment for that role (up to 5), checked at
  once like any send (confirmation, word limit, plan approval, `ask` approval,
  budget). Each time main settles, the watch delivers the next one through
  normal admission unless a
  check that passed before now fails, a stop condition, protected path or
  budget holds, the goal is not running or main's input is not free; it is
  recorded as delivered by the watch (`assigned` with `via: watch`), counts
  against `loop.max_assignments`, and the manager is woken either way, with the
  reason when the step is kept. Another `--prepare` queues behind it, `hgm send
  --withdraw --step N` removes one (1 is next) and `--withdraw` all, and the
  manager's own send to that role supersedes them all.
  A step needs a watch on that role in the same tab; a goal it does not serve
  yet joins it.
- **A provider error gets a continue.** When a turn ends on an API or network
  error (`API Error: Can't reach the API server (ENOTFOUND)`), the watch types a
  fixed continue into main's empty input line itself, at most three an hour,
  records `provider_continue`, and the next wake carries it. More errors in an
  hour go to you.
- **Working without progress is noticed.** While main works, spinners, elapsed
  times, token counts, tips and retry counters are left out of the progress
  check. When the rest of the screen stays the same for `watch.idle_secs`, the
  watch records `no_progress`, wakes the manager and names a retry notice
  (`API error · Retrying …`) as the cause when one is on screen; a long silent
  tool run looks the same, so failure is never assumed. The wake also says when
  the goal's working directory last changed and whether that was after the
  screen stopped (files still change) or before (neither moves); the time is
  the newest modification among the files that differ from HEAD (in a
  repository, also the last commit or checkout), or among all files elsewhere,
  and is recorded on `no_progress`.
- **Idle time is shown and told.** `hgm status` and the Goal watch tab show how
  long main has sat settled short of completion, or worked without progress.
  When that exceeds `watch.idle_notice_secs` with no next step prepared (or
  while it works without progress), you get one notification per stretch and
  an `inbox` entry.

Every time can be changed. Running watches take new values from `hgm supervise
GOAL --idle-secs … --cooldown-secs … --max-wakes … --idle-notice-secs …
--patrol-minutes …` at their next step and say which they did not apply.
Goals confirmed earlier keep the values stored in their contract.

| Time | Default | Config (user and project) | Contract | CLI | Running watch |
|---|---|---|---|---|---|
| Patrol of a working main | 3 min (was 5) | `loop.patrol_minutes` | `loop.patrol_minutes` | `supervise --patrol-minutes` | yes, overrides the contract for that watch |
| Wake after no change | 180 s (was 300) | `watch.idle_secs` | — | `supervise --idle-secs` | yes |
| Between patrol and unchanged-screen wakes | 180 s (was 300) | `watch.cooldown_secs` | — | `supervise --cooldown-secs` | yes |
| Telling you main is idle | 180 s | `watch.idle_notice_secs` | — | `supervise --idle-notice-secs` | yes |
| Wakes per goal (about 8 h) | 160 (was 100), at most 500 | `watch.max_wakes` | — | `supervise --max-wakes` | yes, if above the wakes used |
| Reconcile read | 30 s | `watch.reconcile_secs` | — | — | no, read when the watch starts |
| Goal check | 45 min | `loop.reanchor_minutes` | `loop.reanchor_minutes` | — | no, part of the contract |
| Time budget | none | `loop.max_hours` | `loop.max_hours` | — | no, part of the contract |

## Steer work and add helpers

```sh
hgm correct GOAL_ID --task TASK "Use the CSV header row"   # short correction
hgm reanchor GOAL_ID                    # the worker restates the goal
hgm send --goal GOAL_ID --task TASK --interrupt "Stop: totals include tax"
hgm role start impl                     # an implementer next to main
hgm role start reviewer --agent claude  # an independent reviewer
hgm send --to impl --goal GOAL_ID --task TASK "Write the CSV import"
hgm review GOAL_ID "Check the totals against orders.csv"
```

Corrections stay under 150 words and within `loop.max_corrections`; goal checks
(`reanchor`) are due every `loop.reanchor_minutes` and after a compaction,
restart or scope change. Both, and `send --interrupt`, reach a working agent only
where its provider queues typed input (see Limits); otherwise they are refused
until the turn settles. Nothing is typed into a draft or a dialog.

Each role name runs once per original tab: a second `role start` of a running
role is refused, and so is a second Goal Manager for the same tab (opening it
again switches to the running one). Give each task one owner. `impl` reports
with `hgm report`, which wakes the manager (main reports the same way, see
[Review a result](#review-a-result)); a reviewer's verdict and findings
arrive on the timeline apart from the work and are input to your decision, not
acceptance.

Overrides live in `$XDG_CONFIG_HOME/herdr-goal-manager/config.yaml` (or
`~/.config/...`) and in a project's `.hgm/config.yaml`: `agent`, `models`
(per provider), `prompts` (guide additions, opener, text appended to wakes and
assignments), `loop` defaults, `watch` settings and `manager.native_goal`
(`false` keeps hgm from setting a Claude Code manager's `/goal`). A project config may only add
to the guide. No config can switch off confirmation, budgets, word caps or human
acceptance. `hgm config` shows what applies.

## Review a result

Unattended work is reviewable only if what the worker decided, and what is
still unverified, is written down outside its conversation.

```sh
hgm decision GOAL_ID --task TASK "Parse with serde_json" --why "Already a dependency"
hgm follow-up GOAL_ID --task TASK "The example in docs/import.md is dated"
hgm report GOAL_ID --task TASK --file report.md   # or the report as TEXT
hgm packet GOAL_ID           # one summary for review (--json for the same data)
hgm attention                # wakes, idle time, notices and refusals per goal
```

- **Decisions.** A worker records a choice it made inside the contract, and
  why, when it makes it. Every linked assignment ends with the exact command:
  main gets this build's path with `--workspace` (and `--state-dir` when it is
  not the default), because main has no `hgm` on its PATH; `impl` runs plain
  `hgm`. A decision lands on the goal's timeline (`hgm log`).
- **Reports.** main, `impl` and the manager file a report with `hgm report`, as
  text or with `--file PATH` (up to 3,000 words; the assignment names the
  command the same way). It is kept in full: `hgm log` prints it under its
  line, the packet lists every report, and the manager never needs the
  provider's transcript. An `impl` report wakes the manager; main's arrives
  with the wake its settling brings. A report completes nothing.
  `hgm target --lines N` returns up to N recent lines from Herdr's scrollback
  for what a report does not say.
- **Follow-ups.** A finding that should not block the work (from a worker with
  `hgm follow-up`, or a reviewer's advisory finding) is kept as a follow-up,
  listed apart from blocking findings.
- **Packet.** `hgm packet` gathers, for one goal: each acceptance item, the
  tasks, the latest result of every registered check (from `run`, `accept` or
  the watch's settle checks, whichever is newest), how many assignments,
  corrections and revisions it took, the decisions, the follow-ups, the
  blocking findings of each reviewer's latest verdict, every review verdict,
  and what the timeline marks as not verified: a check with no result, a
  failing one or one that last passed before the latest assignment or
  correction, a delivery recorded as uncertain, a protected-path change, the
  native goal's last outcome, and a review of a workspace that changed while it
  was reviewed.
- **Attention.** From the timeline alone, hgm counts what a goal cost its
  readers: wakes delivered and how many a manager action (an assignment, a
  correction, a goal check, a recorded completion, a wait) followed before the
  next wake; the total and longest time the worker sat settled short of
  completion; notifications to the person; how long the result has awaited
  review; corrections; and refused commands (a linked send, correction or goal
  check refused before anything was typed is recorded as `refused`). The packet
  and `hgm status --json` show it per goal, and `hgm attention` lists every goal
  of the folder. These are observations: they complete and accept nothing.

Each record holds up to 150 words. Records are attributed to the helper role
that ran the command, to the manager (its session sets `HGM_CONTEXT`), or
otherwise to main. A reviewer records nothing but its verdict. None of this
completes a task or accepts a goal: the checks and your recorded acceptance
still decide. A goal without decisions, follow-ups or reviews prints and
behaves as before; only the assignment text gains the one recording line.

**Verdicts are tied to the state they judged.** Each review verdict stores a
fingerprint of the goal's work dir (its checkout, or the workspace). In a Git
repository it covers HEAD, the status, the diff against HEAD and the untracked
files Git does not ignore, so ignored build output never counts; elsewhere it
covers every file's path, size and modification time, skipping `.git`,
`target` and `node_modules`. A verdict whose fingerprint differs from the
current one is shown as **stale** in `hgm show`, `hgm status` (the latest
verdict) and the packet, which also lists it as not verified; a verdict on a
work dir that changed during its review is stale from the start.

**Requiring a review.** With `loop.require_review: true` (a contract field, or
a `loop` default in the config), `hgm run` and `hgm resume` move the goal to
ready for review only with a current approving verdict from a reviewer whose
provider received none of the goal's linked assignments or corrections, and
while no reviewer's latest current verdict asks for a revision. It asks before
the checks run and again after they pass. Otherwise the goal waits for input
with a message naming the reason and the next commands (`hgm review`, then
`hgm resume`). Without the setting nothing changes. A verdict never completes a
task or accepts the goal, and hgm never merges or lands anything on it.

The reviewer guide asks reviewers to judge the files and the diff rather than a
summary, to block only on a problem they can show (a failing command, a wrong
line) and to leave everything else as advisory findings, which become
follow-ups.

**Checks are tied to the state they ran on.** `hgm run`, `hgm accept` and the
watch's settle checks keep the work dir's fingerprint with their results. A
check that passed on a fingerprint the work dir no longer has is listed as not
verified ("passed … on an earlier state of the work dir") in the packet and in
`hgm show`; a result kept before fingerprints were recorded is `unknown` in the
packet (and `show` stays as it was for it). `hgm accept` still reruns every
check on the current state.

**Write scope.** A task may name the paths it may write:

```yaml
tasks:
  - id: parser
    instruction: Change the parser
    write_paths: [src/parser, tests/parser.rs]
```

Its linked assignments then say `Write only in: …`, and hgm keeps a snapshot of
the work dir when each one goes out. When the worker settles, and at each
patrol while it works, hgm compares the work dir with that snapshot: every
changed path outside the task's `write_paths` is recorded once per assignment
(`scope_exceeded`), added to the manager's wake and listed in the packet. hgm
never blocks the write; like `protected_paths`, it observes and tells. A
finished task is no longer compared, and tasks without `write_paths` behave
and print as before.

Limits:

- A write scope sees every change in the work dir, including another goal's
  or the user's edits in the same tree; it cannot tell who made them. Outside
  Git it compares sizes and modification times of all files except `.git`,
  `target` and `node_modules`, and reads no ignore file. A change made and
  undone between two comparisons is not seen.
- The fingerprint covers the whole work dir: several goals sharing one tree
  make each other's verdicts stale (a goal checkout, or `factory.checkout`
  for queued goals, avoids that).
- Outside Git a file rewritten with the same size within the file system's
  timestamp resolution is not seen as changed, and ignore files are not read:
  only `.git`, `target` and `node_modules` are skipped. Measured on this
  repository (2026-11-15): the Git fingerprint took 0.13–0.28 s with a 286 KB
  diff; a metadata walk over the 56 files outside `target` took about 0.02 s,
  against 0.7 s with `target`'s 22,836 files included.
- "Another provider" is judged by the agent names hgm recorded (`claude`,
  `codex`, `copilot`), not by model; a goal whose work had no linked
  assignment accepts an approval from any provider.

## Context handoff

On a recognized context footer at or above `loop.handoff_percent` (85% by
default), the watcher latches a handoff
request. At a safe input boundary the original agent saves a private handoff and
a release receipt. The manager validates both, exits the settled CLI, starts one
fresh session in the same terminal with the recorded launch arguments, and waits
for a matching acknowledgement before rebinding the target. No compact command
is submitted. Active work and draft input delay the transfer.

`handoff`, `handoff-status`, and `restart` expose the same recoverable protocol.
`restart` advances one transition, allowing inspection between operations. A
transport error reserves the operation before delivery and never retries it
blindly. Missing identities, changed terminals, unsupported launch arguments, or
invalid receipts stop the transfer. Native auto-compaction is a separate provider
setting: existing sessions cannot be guaranteed to avoid it. See the limitations.

## Inspect and recover

```sh
hgm overview                 # brief, what needs you, this tab's goals
hgm inbox                    # reviews, questions, approvals, spent budgets
hgm log --since 2h           # what happened while you were away
hgm status --all             # every goal in this folder (--finished: also finished ones)
hgm add --file goal.yaml --validate   # problems and words per field; registers nothing
hgm add --file goal.yaml --replace GOAL_ID   # correct a goal the user has not agreed to
hgm show GOAL_ID
hgm target                   # main's visible screen; --lines 500 reads scrollback
hgm watch
hgm handoff-status
hgm wait GOAL_ID --reason "Need the recipient's decision"
hgm await                    # (manager, in the background) the next wake, past a draft
hgm revise GOAL_ID --feedback "Totals must include tax"
hgm accept GOAL_ID --evidence "Reviewed the verified result"
```

Each goal keeps an append-only timeline (`<goal>.events.ndjson`) next to its
record: registration, confirmation, assignments, observations, evidence, checks,
waits, revisions and acceptance. The record stays the source of truth; the
timeline explains how it got there. The manager receives a command reference
generated from this build, so it does not run `--help` to rediscover commands.

The brief preserves short decisions, one per original tab (one per workspace
outside a Herdr-bound manager). Goals preserve tasks, attempts, checks,
repair history, and acceptance evidence. A successful command or worker report
alone never produces `completed`: explicit acceptance reruns the configured
checks. Runtime state lives under `$XDG_STATE_HOME/herdr-goal-manager` (or
`~/.local/state/herdr-goal-manager`), separated by canonical workspace. Use
`--state-dir` for an isolated run. State is private and outside the repository.

For repeatable command workflows, `init`, `add --file`, and the examples remain
available. Imported goal definitions freeze configured commands; planner output
can select declared runners but cannot invent executables. Runner attempts,
timeouts, and repair budgets are bounded. `herdr-board` opens an optional status
board; it is not required for conversation.

## Factory mode (optional)

Factory mode keeps a local queue of contracts you agreed to and turns them into
goals one at a time, holding back while results wait for your review. It is off
by default; with it off, every command behaves and prints as before and the
`queue` commands refuse with a message naming `factory.enabled`.

```yaml
# ~/.config/herdr-goal-manager/config.yaml (or a project's .hgm/config.yaml)
factory:
  enabled: true
  max_awaiting_review: 2   # results awaiting review before the queue holds (1..=20)
  max_active: 1            # confirmed goals in progress before it holds (1..=10)
  checkout: true           # a checkout per dispatched goal (default: on in a Git repository)
```

A project config may switch the mode on or off but may only lower the limits
below the user config (or the defaults). The defaults let one result sit under
review and one more finish; the original tab runs one agent, so one goal is in
progress at a time.

```sh
hgm queue add --file contract.yaml --evidence "the user's words"
hgm queue list [--all]       # state, agreement age, history
hgm queue park ID [--reason TEXT] | unpark ID | remove ID
hgm queue next               # the oldest queued contract becomes a confirmed goal
```

`queue add` refuses a contract without the user's words, one that fails contract
validation (including `auto` without a command check), and a second copy of the
same content while one is queued, parked, claimed or running; layout, comments
and key order do not make a contract different. `queue next` settles any claim a
stopped dispatch left, refuses with the reason and the goals involved while
results awaiting review reach `factory.max_awaiting_review` or confirmed goals in
progress reach `factory.max_active` (`overview` and `inbox` show the same reason),
and otherwise takes the oldest queued item under an exclusive lock, so two
managers never create two goals from it. The new goal is confirmed with the
stored agreement and belongs to the manager that took it; its first assignment
goes out with an ordinary `send`. A contract whose confirmation is refused (for
example its checkout cannot be created) is marked failed with the reason, and
its unconfirmed goal is cancelled and kept as evidence. Nothing dispatches by
itself: the manager runs `queue next` at startup when `overview` says contracts
are queued and the queue is open, and after a goal is accepted or cancelled.

**One checkout per queued item.** With `factory.checkout` on (the default in a
Git repository), `queue next` gives the new goal its own checkout through the
same mechanism as a contract's `checkout` (`herdr worktree create`): branch
`hgm/<item>` (the item id up to its first eight hex digits, e.g.
`hgm/q-1a2b3c4d`), started from the workspace's current branch (its commit when
HEAD is detached), at a path Herdr chooses. A contract that names a checkout
keeps its own. Linked assignments say to work there and checks run there.
`queue next` prints the checkout's path and branch, and `queue list` and the
packet show it. When the checkout cannot be made (a branch of that name exists
without a worktree, a repository without a commit, Herdr refusing), the item
is marked failed with the reason like any refused dispatch, its goal is
cancelled, and the next item is not held. With `checkout: true` in a folder
that is not a Git repository, the goal works in the workspace and `queue next`
says why; `checkout: false`, or no setting outside Git, behaves and prints as
before. hgm never commits, merges or removes the checkout: remove it with `hgm
checkout remove GOAL` once its work is committed, and the branch stays.

`hgm show` and `hgm status --json` give each goal taken from the queue a run
class, computed only from recorded events: **success** (accepted after agent
work, no failed check, no revision), **flawed** (a check failed at run, resume
or acceptance, a check that passed at an earlier settle now fails, or you
revised the result; it stays after acceptance), **blocked** (stopped now by a
protected path, stop condition, spent budget or needed input; never after
acceptance), **manual** (accepted without agent work, or cancelled without being
blocked), or pending while nothing decides it. Agent reports and reviewer
verdicts never count. Queue state lives in hgm's private state folder: one file
per item and an append-only ledger.

Limits of this first stage:

- Only this folder counts: results awaiting review in other folders do not hold
  this queue.
- The agreement is your words when the contract was queued; hgm records how
  old it is, not which commit it was given for.
- Two contracts with different text but the same intent are not detected as
  duplicates.
- hgm does not check what your acceptance words say.
- Raising `max_active` does not start parallel loops: one goal per original tab
  still runs at a time. With `factory.checkout` on, each goal taken from the
  queue works in its own checkout, so one goal's unfinished change is no longer
  recorded as another goal's check regressing. Goals registered directly
  without a `checkout`, goals in a folder that is not a Git repository, and
  `factory.checkout: false` still share one working tree and see each other's
  half-done edits.
- A factory checkout starts from the workspace's last commit: uncommitted
  changes in the workspace are not in it.
- A goal cancelled after a refused dispatch has no run class, and `queue next`
  with nothing queued exits with an error.

## Upgrade

Update a GitHub install with `herdr plugin update`; for an archive, extract the
new one and run `herdr plugin link` on its folder; for a checkout, pull and
rebuild. A running Goal watch continues in the new build when the binary it runs
is replaced at the same path (written to a temporary file and renamed into
place); otherwise rearm it with `hgm supervise GOAL_ID`. Managers and roles keep
the code and instructions they started with; restart them to pick up a new
guide:

- Reopen Goal Manager with the shortcut after closing the old one, so the
  manager receives the new guide and command reference.
- Restart helper roles (`hgm role stop NAME`, then `hgm role start ...`) so they
  receive updated role guides.

## Limits

This is an experimental local tool; [verification](docs/verification.md)
records what was checked natively and what was not.

- Interrupt delivery (`send --interrupt`, and `correct` or `reanchor` to a
  working agent) reaches Claude Code and Copilot. It stays off for Codex until a
  native check shows that Codex queues input typed during a turn; Codex's native
  checks are pending.
- Copilot roles work only in folders Copilot already trusts. Copilot asks
  whether to trust a folder each session unless told to remember it, and hgm
  neither answers that question nor changes Copilot's settings: the start stops
  within seconds, closes its tab and says why. Start the role with another
  provider instead.
- A Copilot context-handoff restart stops when the successor starts: Copilot
  asks again whether to trust the folder, and the restart waits until you
  answer in that tab (hgm never answers dialogs).
- Copilot managers need a capable model. With `auto`, Copilot picked a small
  model that skipped registering the goal; set `models.copilot` in the config or
  pass `--model`.
- Herdr 0.9.5 lists Copilot as `done` while it runs a tool. hgm reads Copilot's
  status line and treats such a turn as working, so it neither types into it
  nor takes it for finished work.
- A Claude Code main running in its sandbox needs permission to write the
  handoff document and receipts, and to record decisions and follow-ups: they
  live in hgm's private state folder, outside the workspace. In an asking
  permission mode it asks you; allow that folder in its sandbox settings to
  avoid the prompt.
- State, dialogs, drafts and context-rot signs are read from screen text, which
  changes between provider versions; an unfamiliar dialog may only show up as a
  stalled agent.
- Working without progress is judged from the screen. hgm knows the status
  lines of Claude Code, Codex and Copilot; for another provider only the shared
  rules (tips, interrupt hints, token and retry counters) apply, and a status
  line it does not know keeps counting as progress. Claude Code 2.1.325's clock
  at the end of a running tool's line and its blinking bullet do not count. A
  long tool run that prints nothing looks exactly like a hang.
- Checked natively on 2026-11-16 (Herdr 0.9.5, Claude Code 2.1.325, Copilot
  1.0.112): a wake queued for a busy Claude Code manager, a stale wake dropped,
  the prepared step delivered and held, the idle notice, `supervise` retuning,
  no progress during a silent tool run, the native goal, a worker's decision, a
  reviewer on another provider with `loop.require_review`, and a factory
  checkout. A Copilot or Codex manager taking a queued wake, the continue after
  a provider error and a retry notice were not checked natively; see
  [verification](docs/verification.md#native-check-of-the-new-behaviour-2026-11-16).
- Herdr can list a Claude Code manager as working for a minute or more after
  its turn ended on screen (seen once, 2026-11-16). A stop wake reaches it as
  queued input; other wakes wait until Herdr lists it as settled.

Planned:

- The native goal for Codex and Copilot, once their documentation and a native
  check confirm the same set, clear and outcome behaviour as Claude Code's
  `/goal` (see [Native goal](#native-goal-goal)).
- An optional compaction hook for your own main agent that records the
  compaction and asks for a goal check. It never blocks compaction.

## Develop

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
target/release/hgm --help
```

Automated tests use isolated homes and controlled provider/Herdr executables.
Native checks are recorded separately. Rebuild linked installations after source
changes. Remove the keybinding and unlink the plugin to stop using it; keep the
private state for recovery. Closing a manager does not cancel independent work.

[Design and comparison](docs/comparison.md) · [Verification](docs/verification.md)
