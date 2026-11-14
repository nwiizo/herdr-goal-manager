# Herdr Goal Manager

Talk through a problem with Claude Code, Codex, or Copilot, ask the agent in
your original tab to do the work, and keep evidence of the result. The manager
opens a separate Herdr tab. Your original target stays pinned when focus changes.
Git, a software project, and ccswarm are optional.

This is an experimental local tool. It has bounded monitoring and a durable
handoff protocol; it does not provide a complete software-factory/PR service.
See [verification and limits](docs/verification.md) before unattended use.

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
herdr plugin install nwiizo/herdr-goal-manager --ref v0.1.1
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
base=https://github.com/nwiizo/herdr-goal-manager/releases/download/v0.1.1
mkdir -p ~/.local/share && cd ~/.local/share
curl -fLO "$base/hgm-v0.1.1-$target.tar.gz"
curl -fLO "$base/SHA256SUMS"
shasum -a 256 -c SHA256SUMS --ignore-missing  # Linux: sha256sum -c SHA256SUMS --ignore-missing
tar -xzf "hgm-v0.1.1-$target.tar.gz"
"hgm-v0.1.1-$target/target/release/hgm" --version  # hgm 0.1.1
herdr plugin link "$PWD/hgm-v0.1.1-$target"
```

On macOS, Gatekeeper may block an archive downloaded with a browser; download it
with `curl` as above, or clear the attribute with
`xattr -dr com.apple.quarantine "hgm-v0.1.1-$target"`.

**From a checkout** (Rust 1.89 or later):

```sh
git clone --branch v0.1.1 https://github.com/nwiizo/herdr-goal-manager
cd herdr-goal-manager
cargo build --release --locked
target/release/hgm --version  # hgm 0.1.1
herdr plugin link "$PWD"
```

**From crates.io**, if the ways above do not work for you (Rust 1.89 or
later). `cargo install` installs only the `hgm` binary; `hgm plugin-folder`
then makes a plugin folder around it (the manifest and a link to the binary,
by default in `~/.local/share/herdr-goal-manager/plugin`) for Herdr to link:

```sh
cargo install herdr-goal-manager --locked
hgm --version  # hgm 0.1.1
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
`ln -s ~/.local/share/hgm-v0.1.1-$target/target/release/hgm ~/.local/bin/hgm`
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
  patrol_minutes: 5       # check main every 5 minutes while it works; 0 disables
  stop_when: ["Auth changes are required"]
  autonomy: auto          # ask: every assignment needs your recorded approval
  handoff_percent: 85     # original-agent context that triggers handoff
```

The original agent makes; the manager and registered checks verify. `auto`
autonomy needs at least one command check. Every linked assignment stays under
400 words and carries the binding contract; with `plan_first`, implementation
waits for `approve-plan`. `protected_paths` and machine `stop_checks` stop the
loop mechanically. A failed check becomes a linked fix assignment within budget. A spent budget, a stop
condition, an approval dialog in the original tab, a result awaiting review, or
monitoring that stops abnormally becomes a Herdr notification and an `inbox`
entry. `revise ID --feedback` reopens work you reject; acceptance is always yours.

The watch tab wakes the manager when the worker settles, changes state, shows no
observed progress, or a patrol is due, with the acceptance, invariants, stop
conditions and last assignment. It does not replay prompts or treat an idle agent
as completed work. Defaults: a check of main every five minutes while it works, a wake after five
minutes without change, five minutes between wakeups, 100 wakeups (about 8 hours of 5-minute checks) per goal/context. `wait ID --reason` pauses wakeups
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
with `hgm report`, which wakes the manager; a reviewer's verdict and findings
arrive on the timeline apart from the work and are input to your decision, not
acceptance.

Overrides live in `$XDG_CONFIG_HOME/herdr-goal-manager/config.yaml` (or
`~/.config/...`) and in a project's `.hgm/config.yaml`: `agent`, `models`
(per provider), `prompts` (guide additions, opener, text appended to wakes and
assignments), `loop` defaults and `watch` settings. A project config may only add
to the guide. No config can switch off confirmation, budgets, word caps or human
acceptance. `hgm config` shows what applies.

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
hgm status --all             # every goal in this folder
hgm show GOAL_ID
hgm target
hgm watch
hgm handoff-status
hgm wait GOAL_ID --reason "Need the recipient's decision"
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
  handoff document and receipts: they live in hgm's private state folder,
  outside the workspace. In an asking permission mode it asks you; allow that
  folder in its sandbox settings to avoid the prompt.
- State, dialogs, drafts and context-rot signs are read from screen text, which
  changes between provider versions; an unfamiliar dialog may only show up as a
  stalled agent.

Planned:

- Set the provider's native goal on main (`/goal` in Claude Code and Codex) when a
  goal is confirmed, coordinated with hgm wakes so two continuation loops do not
  spin. Copilot stays off until a native check finds an equivalent.
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
