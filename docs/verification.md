# Verification and remaining limits

Status: local experimental build, not yet approved for publication.

## Automated evidence

The isolated suite covers goal validation, dependency failures, bounded retries,
timeout and interruption, acceptance-time checks, brief persistence, native
provider launch arguments, fixed-target dispatch, draft preservation, bounded
wakeups, session-scoped Claude Stop hooks, and uncertain-delivery recovery.
Handoff tests require a saved document and matching release receipt, refuse to
exit a working agent, preserve launch arguments, prevent a second start, and
require a new session's acknowledgement before rebinding.

Loop tests (2026-11-13) cover: `wait` keeping a result awaiting review (and
`accept` working afterwards), the waiting reason kept out of user guidance, the
"no command checks" review message, `revise` reopening manual work with
superseded evidence and refusing command tasks or finished goals, an ordered
timeline, linked `send` recording the assignment and arming monitoring, one
notification per approval dialog even when Herdr reports `done`, wait
notifications, tab ownership with `--all`, the manager matching the original
tab's client, contract confirmation, `ask` approvals, assignment-budget
escalation to the inbox with a fresh budget after revision, patrol wakes carrying
acceptance, invariants, stop conditions and the last assignment, time-budget
escalation, the configurable handoff threshold, contract advice, `overview`, and
the generated command reference (no hidden commands). Enforcement tests add the contract block and
400-word cap on assignments, the plan-first gate, protected-path and stop-check
stops, context-rot wakes (evidence to the manager, no automatic handoff), the
800-word contract budget (including Japanese),
`auto` refused without a check, revision lessons, and naming only an unnamed
original tab `main`.

Manager behavior tests (2026-11-13) cover: interrupt delivery (`send --interrupt`,
`correct`, `reanchor`) reaching a working Claude Code or Copilot only with an
empty input line, refused for a draft, a dialog, a blocked agent and a provider
not known to queue input (Codex), with consumption confirmed on screen and flagged
when the text stays in the input; corrections within `loop.max_corrections`;
goal checks due every `loop.reanchor_minutes` and after a detected compaction,
a restart or a scope change, not counted against the assignment budget; the new
guide rules reaching the Claude, Codex and Copilot launch paths; pause, cancel
and wait ending watch records with no watch process, and `supervise` recovering
a stale record; provider-error, usage-limit and login classification; holds
after two wakes on an unchanged screen; user and project config overrides.

Watch tests (2026-11-13) use a fake Herdr socket: a dialog reported as `done`
is found by the screen read that each state event triggers and held with one
notification, nothing typed into it; an event stream that ends switches the
watch to 10-second polling, records the switch on the timeline, and the
immediate full re-read finds the change made during the gap; a worker pane
exit ends the watch with a notification; the watch tab shows goal, pane, mode
and, after the end, the reason and `hgm supervise GOAL`, while the worker's
sidebar is set and then cleared. Unit tests cover a settled state left from the
previous turn not counting after a delivery, one context-rot sign counted once
while it stays on screen, and the event-to-signal mapping. Role starts refuse
multi-line arguments and launch lines over the shell's 1024-byte limit before
a tab opens; the fixture refuses multi-line arguments as Herdr does, and Codex
and Copilot roles start through it.

Continuity tests (2026-11-14) cover: a session the user restarted in the
original tab refused by `target`, `send` and `supervise` with a pointer to
`target --adopt`, adopted only with evidence, in the pinned terminal and tab,
and not while a handoff restart is in flight, which rebinds the watch and
records `restarted` (a goal check is due); a watch process continuing in a build
renamed over its executable (`watch_reloaded`), so a redeploy does not end it;
the manager's Stop hook asking once to rearm when the watch record is active but
no watch process runs, after a 30-second start window; `supervise` reusing a
running or just-armed watch instead of opening a second Goal watch tab; and one
Goal Manager per tab: opening it from that tab or from the manager switches to
the running manager (also one started by an earlier build, found through the
tab's watch record), opening it from the Goal watch tab or a helper's tab is
refused, a second manager started directly stops before its agent starts, and
the next open after the manager has gone starts a new one. Watch tests from the
native checks add: a new watch closing the ended Goal watch tab of the same tab
(only while it still carries the Goal watch label); no manager wake while a
dialog holds the watch; a pause ending a quiet event watch within seconds and
clearing the sidebar; a pane that vanishes behind a status event ending as
`target_exited`; Claude Code's "Ran 2 shell commands" summary lines not
counting as a repeated tool call (only lines that name the command count); and
the tab showing the whole budget before the first assignment, the next patrol
for a settled agent, and a pending wake apart from a hold.

## crates.io release (2026-11-14)

Version 0.1.1 (tag `v0.1.1`, commit `73a2d13`) was published with
`cargo publish --locked`: 48 files, 620.5 KiB (167.5 KiB compressed), and the
packaged crate built before upload. Cargo warned that `package.homepage`
repeats `package.repository`.

| Check | Observed |
| --- | --- |
| `cargo search herdr-goal-manager` | `herdr-goal-manager = "0.1.1"` |
| `cargo install herdr-goal-manager --locked --root TMP` | Built the release profile in 20 seconds and installed `hgm`; `hgm --version` printed `hgm 0.1.1`, `--help` ran. |
| `hgm plugin-folder --dir TMP/plugin` | Wrote `herdr-plugin.toml` (version 0.1.1, actions `open` and `board`) and linked `target/release/hgm` to the installed binary; the link printed `hgm 0.1.1`. `herdr plugin link` was not run, so the existing Herdr plugin stayed in place. |

## Published release (2026-11-14)

Tag `v0.1.0` (commit `3ed45a1`) on the public repository. CI on main passed on
Linux and macOS before the tag was pushed; the tag's release workflow built and
smoke-tested all four targets and then created the release with
`hgm-v0.1.0-{aarch64,x86_64}-{apple-darwin,unknown-linux-gnu}.tar.gz` and
`SHA256SUMS`.

| Check | Observed |
| --- | --- |
| Prebuilt archive | Following the README: `aarch64-apple-darwin` downloaded with `curl`, `shasum -a 256 -c SHA256SUMS --ignore-missing` OK, extracted folder holds `target/release/hgm`, `herdr-plugin.toml`, `README.md`, `LICENSE` and `CHANGELOG.md`; `hgm --version` printed `hgm 0.1.0`, `--help` ran. |
| `herdr plugin install nwiizo/herdr-goal-manager --ref v0.1.0` | After unlinking the local copy: Herdr built the plugin with its build step in 66 seconds; `herdr plugin list` shows `[github:nwiizo/herdr-goal-manager@v0.1.0]`, the checkout is at `3ed45a1`, its `hgm --version` printed `hgm 0.1.0`, and the actions `herdr-goal-manager.open` and `.board` are listed. |
| Goal Manager from the installed plugin | Opened through the plugin's `panel` entrypoint in an owned workspace with temporary state: the manager's `HGM_BIN` was the installed build, it ran `hgm overview` and asked what to move forward. The person then opened Goal Manager with their own shortcut. |

## Release build checks (2026-11-14)

Build: hgm 0.1.0 release (`cargo build --release --locked`, sha256
`ed419bcd6e53deba4453a1b233f2de93cba4d66c615535b48d5b59e64bf890bf`), deployed
by temporary file and rename; the person's running Goal watch continued in it
(same process, `watch_reloaded` 13 seconds after the rename) and stayed the
only one. CLIs: Herdr 0.9.5, Claude Code 2.1.325, Copilot 1.0.112 (worker
`--allow-all --model auto`, manager `claude-sonnet-5.5` through `models.copilot`),
Codex 0.167.2 (pending, see below). Owned Herdr workspace, temporary state, the
validation folder with `orders.csv`; workers ran with full access, reviews by a
Claude reviewer role. Times are UTC from the goal timelines.

Request cycle (contract → confirm → send → review → revise → accept):

| Provider | Timeline | Result |
| --- | --- | --- |
| Claude Code | Contract shown before confirmation; `registered` 06:04:47, `confirmed` 06:05:03 (evidence: the person's words), `assigned` 06:05:10, settle wake 06:05:43, `ready_for_review` 06:05:52, `review_requested` 06:06:19, `reviewed` approve (2 findings) 06:07:28. Rejection ("sort by amount, highest first"): `task_reopened` and the words as guidance, `ready` 06:08:59; the manager judged that the new order contradicted the confirmed acceptance ("CSV order"), cancelled the goal and registered a replacement confirmed with the rejection's words (not shown first): `assigned` 06:09:07, `reviewed` approve 06:10:40, `accepted` 06:15:53 with checks rerun, `completed`. | Pass, across a replacement goal for the changed acceptance. |
| Copilot | Contract shown before confirmation; `registered` 06:07:16, `confirmed` 06:07:27, `assigned` 06:07:31, `ready_for_review` 06:08:07, `review_requested` 06:10:02 (Claude reviewer), `reviewed` approve 06:10:50. Rejection within the acceptance ("same content, indent with two spaces"): `task_reopened`, guidance, `ready` 06:16:17, `assigned` 06:16:18, settle wake 06:16:48, `ready_for_review` 06:16:55, `accepted` 06:18:18 with checks rerun, `completed`. | Pass, in one goal. |

Context handoff restart (handoff → safe-to-restart receipt → restart in the
same tab → successor acknowledgement):

| Provider | Phases and receipts | Result |
| --- | --- | --- |
| Claude Code | `awaiting_handoff` 06:16:22; the handoff document and the receipt `{"safe_to_restart":true}` for session `0e26c6ff…` saved by 06:18:03; `awaiting_exit` 06:18:23; the successor start first returned `start_delivery_uncertain` and was `awaiting_successor` 3 seconds later (06:21:37); `awaiting_acknowledgement` 06:21:44; `complete` 06:22:23. The successor (session `bd94d11f…`, same pane and terminal) wrote the acknowledgement with its session and the handoff document's sha256; `restarted` (via handoff) on the timeline. | Pass. |
| Copilot | `awaiting_handoff` 06:23:36 (the request needs the session identity, which Herdr reported only after Copilot's first turn); receipt `{"safe_to_restart":true}` for session `d66679ef…` at 06:24:00; `awaiting_exit` 06:24:24, Copilot exited to the shell of the same terminal; the successor start at 06:24:45 returned `start_delivery_uncertain` because the new Copilot asked whether to trust the folder; after the person answered, the successor idled with no session identity for over 80 seconds. hgm sends the adoption prompt only to a successor with a new session identity, and Copilot gets one only after a turn, so the transfer cannot advance. | First attempt not passed; stopped without relaxing the successor check. Passed on the fixed build, see below. |

Copilot restart on the fixed build (sha256
`7865b2dcd3734df32baf26a77f49d890ff59a86c0b12c5ea7bbf0c49f7f8d2cd`, deployed by
temporary file and rename; the person's Goal watch continued in it as the same
process and stayed the only one). A successor without a session identity is now
sent the adoption request only as another process in the predecessor's terminal
that accepts input, and its acknowledgement must carry the session Herdr reports
after that first turn, distinct from the predecessor's. Fresh owned workspace
and temporary state; main had one turn first, so Herdr reported its session
(`2633cf66…`, process 48238).

| Phase | Observed (UTC) |
| --- | --- |
| Handoff | `awaiting_handoff` 06:43:02; the handoff document and `{"safe_to_restart":true}` for session `2633cf66…` |
| Exit | `awaiting_exit` 06:44:21; Copilot exited to the shell of the same terminal |
| Restart in the same tab | Successor started 06:44:46 in the same pane and terminal (process 52958); it stopped at Copilot's folder-trust question (`start_delivery_uncertain`) until the person answered it at 06:51:50. The Copilot manager meanwhile proposed adopting the session by hand; told to keep running `restart`, it did. |
| Successor acknowledgement | Adoption request sent 06:51:54 while the successor had no session yet (`awaiting_acknowledgement`); Herdr then reported `2d0e4a34…`; the acknowledgement carried that session, the transfer id and the handoff document's sha256; `complete` 06:52:36, and the manager's binding moved to the successor's session. |

Result: pass. No goal was watched in this run, so the transfer record and the
receipts are the evidence rather than a `restarted` timeline event.

Findings during these checks, and what changed:

- A review verdict on a goal awaiting acceptance, and handoff progress when no
  goal is watched, reached neither the manager nor the person (the watch ends at
  `ready_for_review`); in each case the person asked the manager. Fixed: a
  verdict notifies the person and wakes the manager directly when no watch
  carries it; a handoff in progress keeps the watch advancing it after the goal's
  watch ended, and a handoff requested while no watch runs opens one (bridge
  tests).
- The Claude manager confirmed a replacement goal with the person's rejection
  instead of showing the new contract first. The manager guide now requires
  showing a new contract and confirming it with the person's words about it.
- Claude Code's own sandbox refused the original agent's write of the handoff
  receipt into hgm's private state (outside the workspace); the predecessor and
  the successor each ran that one write outside the sandbox, which their
  full-access mode allowed, and the successor saved a memory note to keep doing
  so, deleted at the person's request. An agent in an asking mode would ask the
  person instead (README, Limits).
- Following the person's rule that main must not stop short of completion: a
  wake about a stopped main says to send the next step now; a network error on
  screen (ENOTFOUND, fetch failed, connection resets after a sleep) wakes the
  manager to continue; repeated stops without a next step notify the person; a
  watch ends only when its goal is completed, cancelled or waiting for the
  person, with passing observation failures retried (bridge and unit tests).
- Codex: native checks pending, as in the feature checks above.

## Compaction in hgm-launched sessions (2026-11-13)

| Provider (version) | Setting hgm applies | Remains uncontrolled |
| --- | --- | --- |
| Claude Code 2.1.323 | `"idleCompaction": false` in the manager's session settings | Automatic compaction near a full context stays on (`DISABLE_AUTO_COMPACT` would end the session at a full context instead); a SessionStart `compact` hook restores saved state afterwards |
| Codex 0.167.2 | None: no idle-compaction setting found | Automatic compaction at `model_auto_compact_token_limit` |
| Copilot 1.0.110 | None: no off switch | Background compaction at `COPILOT_BACKGROUND_COMPACTION_THRESHOLD` (default 0.8) and blocking compaction at 0.95 |

These apply to managers hgm starts. The user's own main agent keeps its own
settings; hgm detects its compaction notice on screen and asks for a goal check.
Claude Code and Codex also offer `PreCompact`/`PostCompact` hooks (Claude can
block compaction from `PreCompact`); hgm does not install hooks into the user's
own sessions.

## Native watch checks (2026-11-14)

Herdr 0.9.5, release builds of 0.1.0 deployed by temporary file and rename.
Unless noted, a check ran in owned tabs against temporary state: a worker pane
whose agent state was driven with `herdr pane report-agent` and whose screen
was printed on demand, a stand-in manager pane that only records wake lines,
and a Goal watch tab opened by `supervise` through the plugin.

| Behavior | Observed |
| --- | --- |
| Dialog while working | An approval dialog printed while the worker stayed `working` was held at once through the output-match event; the user was notified and the manager got no wake. |
| Dialog reported as `done` | With the dialog still shown, the worker went idle and Herdr listed it as `done`; the screen read on that state event kept the hold (`state "done"`), and the manager got no wake. An earlier build still woke the manager here; fixed (a hold now drops the queued wake). |
| Release of a hold | After the dialog left the screen and the worker settled, one wake reached the manager. |
| Polling with a full re-read | A watch without events (`--polling`) found a dialog that appeared with no state change after 6 s and an idle transition after 9 s, both within the 10-second poll. |
| Events dropping mid-stream | Not induced natively: cutting one subscription of the shared Herdr server needs a socket proxy or a debugger (neither available without developer mode) or a server restart, which validation must not do. The switch to polling and the immediate full re-read are covered by the fake-socket test, which speaks Herdr's NDJSON stream protocol. |
| Worker pane exit | Ending the worker's shell ended the watch within a second with `target_exited: bound pane no longer exists`, and the user was notified. An earlier build reported this as `needs_inspection` when a status event came first; fixed. |
| Goal watch tab | Redraws in place: objective, goal, `watching main in <pane>: <state> (events)`, last observation, `held:` or `wake pending:`, `wakes n/max · next patrol in 5m` (or `next patrol when main works (every 5m)`), `time left · assignments` (the whole budget before the first assignment), the last five timeline events, and the sheep with a bubble for holds and notices. After the end it shows `watch ended: <reason>`, `rearm: hgm supervise <goal>`, the last state and `Press Enter to close this tab`. |
| Sidebar | The worker pane carried title `hgm: <objective>` and tokens `goal`, `state` (`held (done)` only for a real hold, `<state> · wake pending` while a wake waits) and `budget` (`wakes 0/20 · 2h 0m left · 0/3 assigned`). A pause cleared title and tokens within a second, since the event wait now reads the local record every 2 s. |
| One Goal watch per tab | A second `supervise` reused the running watch (no new tab). Rearming after a pause closed the ended Goal watch tab before opening the new one. |
| Redeploy | Deploying a new build made the running watches continue in it (`watch_reloaded`, same process id) on their next event or reconcile, 12 to 27 seconds after the rename; exactly one Goal watch kept running for the user's main tab. |
| Session adoption | On the user's own main tab (Claude Code), after the user restarted main in that tab, `target` refused with the pointer to `--adopt`, and the manager's `target --adopt EVIDENCE` rebound the tab, recorded `restarted` (via `adopt`) and re-armed one Goal watch. A stand-in agent cannot be given a session identity (Herdr keeps `agent_session` empty for it), so this was checked on a real session only. |

Reopening Goal Manager and a second impl or reviewer start were checked with
real agents; see the next section.

## Native feature checks (2026-11-14)

Herdr 0.9.5 with Claude Code 2.1.323, Codex 0.167.2 and Copilot 1.0.110, in an
owned Herdr workspace with temporary state and a validation folder holding a
three-row `orders.csv`. The worker of each provider ran in its own tab in the
provider's asking permission mode (Claude Code manual mode, Copilot Manual
Approval), so real approval dialogs appeared; managers and roles ran with full
access as hgm starts them. The build was this goal's working tree, run from a
copied binary; where a check depended on the new watch code, the Goal watch tab
opened by `supervise` (the deployed build) was replaced by the same build's
`watch-loop`. The person's part (requests, answers, approving dialogs) was typed
into the owned tabs.

| Feature | Claude Code | Copilot | Codex |
| --- | --- | --- | --- |
| Event-based dialog detection | A Bash approval dialog was held while main was still `working` (output-match event) and kept held when Herdr moved it to `blocked`; the manager got no wake, and the manager itself declined to type while the dialog stood, asking the person to answer it. | A file-creation approval dialog was held on the `blocked` state event; no wake. | Pending (see below). |
| Interrupt delivery | During main's `sleep 180`, `send --interrupt` was admitted with `interrupted: true, consumed: true`; Claude Code queued it ("Press up to edit queued messages") and ran it after the sleep. | Text typed during a tool run was listed above Copilot's input line, the input line emptied, and Copilot ran it after the step. Enabled for Copilot on that evidence; then the Copilot manager's `send --interrupt` during main's `sleep 150` was admitted with `interrupted: true, consumed: true`. | Off until a native check shows that Codex queues input typed during a turn (pending, see below); `send --interrupt` and corrections to a working Codex are refused (bridge test). |
| Interpretation-needed request | "That one, only the expensive ones; make the name clear too": the manager inspected `orders.csv` and asked two questions (which threshold, what "name" meant) with the data's options. Later "drop the expensive ones, by the threshold we set": it applied the earlier unit price ≥ 200 from its brief and sent a concrete assignment naming the file, the rows to keep and what stays unchanged. | "The other order after all, and drop the top one": the manager sent a concrete assignment (descending unit price, without the most expensive item), choosing the reading itself instead of asking which "top" was meant. | Pending. |
| impl/reviewer routing | The manager started impl (Claude) and a reviewer, sent `--to impl`; impl reported with `hgm report` (wake), the watch moved to impl, and the reviewer's verdict (`approve`) arrived on the timeline after `hgm review`. | The Copilot manager started impl (Claude) and sent `--to impl`; impl reported back. A Copilot role could not start in the folder (see below). | Pending. |

Reopening and duplicates (Claude Code tab):

- Reopening Goal Manager from main while a manager ran: `herdr-open` switched to
  the running manager's tab and opened none. The focus call was recorded by a
  Herdr wrapper instead of executed, so the person's own view did not move.
- A second manager launched directly for the same tab stopped with "Goal Manager
  is already open for this tab" within 2 seconds and left no manager context.
- Two managers launched for one tab at the same moment: one started, the other
  waited for the tab's launch lease, found the first (registered, its agent not
  yet shown) and stopped within 2 seconds. An earlier build had no lease and
  took a manager without a visible agent as gone; fixed.
- A second `role start impl` and a second `role start reviewer` (Claude and
  Copilot tabs) stopped with "already runs in pane …" and opened no tab.

Findings fixed during these checks:

- Claude Code 2.1.323 draws a suggested next prompt in faint text in its input
  line. hgm read it as a draft, so a 5-minute check wake waited about an hour
  until the time budget ended. Screens are now read with styles and faint text
  after the input prompt is left out; afterwards wakes reached a manager showing
  such a suggestion (twice).
- Herdr 0.9.5 listed Copilot as `done` while it ran a tool, so the watch took a
  just-started turn for a settled one and woke the manager with failing checks
  within a second of the assignment (the manager recognized it and sent
  nothing). Copilot's status line (`ctrl+c interrupt`) now counts as working;
  afterwards the Goal watch showed `working` while Herdr listed `done`.
- One dialog that stayed while the state moved (working, then blocked) produced
  up to three notifications; it now notifies once until the dialog leaves.
- A settled worker's check result waiting for the cooldown was replaced by a
  generic "no observed change" wake; the waiting result is now kept.
- A role whose provider stopped at its own folder-trust question failed to start
  and left its tab, whose agent kept the role's Herdr name taken; hgm now closes
  that tab and says why (checked: Copilot impl, tab closed within 4 seconds).

Limits found:

- Copilot roles are off in any folder Copilot does not already trust. Copilot
  asks whether to trust a folder each session unless the person makes it
  remember the folder, and Herdr reports a Copilot stopped at that question as
  not ready, so the role is never registered (answering in the role tab does
  not help). hgm neither answers that question nor changes the person's Copilot
  settings: the start stops within seconds, closes its tab and says why, and the
  manager starts the role with another provider. The checks ran roles on Claude.
- The account's configured Copilot default model was unavailable. With `auto`,
  Copilot picked a small model for the manager, which skipped registering the
  goal and waited with its own `sleep`; with `claude-sonnet-5.5` set in
  `.hgm/config.yaml` (`models.copilot`) the manager followed the guide. Set a
  capable model for Copilot managers.
- Codex (2026-11-14): native checks pending, by the person's decision: the
  account's weekly Codex limit was used up, and further turns would be paid from
  credits. Whether they run after the limit resets (2026-11-16) or are dropped
  from the release criteria is decided at the final review. Codex keeps the
  behavior of the earlier checks (2026-11-12) and the bridge tests. Meanwhile interrupt delivery to Codex stays off: until a
  native check shows that Codex queues input typed during a turn,
  `send --interrupt` and corrections to a working Codex are refused, since text
  typed into a working Codex is not known to be kept.

## Native checkout checks (2026-11-14)

Herdr 0.9.5 on macOS, with a development build of hgm and a throwaway setup
(a new Git repository with one commit on `main`, and a temporary hgm workspace
and state directory), so no goal or workspace of the person was touched.

- `confirm` of a contract without a command check under `autonomy: auto` was
  refused before any Herdr call: no checkout is made for a contract that cannot
  be confirmed.
- `confirm` with `checkout: {repository, base: main, branch, path}` created the
  worktree with `herdr worktree create --no-focus`. `herdr workspace list`
  showed the new workspace with `focused: false`, labelled `goal g-…`; `git
  worktree list` showed the branch at the path. The goal recorded path, branch,
  base and `workspace_id`.
- A second goal on the same branch reopened it with `herdr worktree open`: the
  reply carries `type: worktree_opened`, `already_open: true`,
  `workspace.workspace_id` and `worktree.path` (the real path, e.g. under
  `/private/tmp` on macOS), and the goal recorded the same workspace.
- `hgm checkout remove` with an untracked file in the checkout was refused with
  `1 uncommitted change(s) … (nothing was removed)`. After the file was deleted
  it succeeded: the workspace closed, `git worktree list` no longer showed the
  checkout, its folder was gone, the branch stayed, and the goal recorded
  `removed_at` and a `checkout_removed` event.
- `herdr worktree create` also opened the source repository as an unfocused
  workspace when none was open for it. It stays after `checkout remove`, since
  the person may use it; the throwaway one was closed with
  `herdr workspace close`.

After removal, any command or check of the goal stops with "goal …'s checkout
… was removed with hgm checkout remove at …" instead of a missing-directory
error (covered by `a_goal_checkout_is_a_herdr_worktree_where_work_and_checks_run`).

## Wake size (2026-11-14)

Every wake used to repeat the acceptance, invariants, stop conditions and the
full last assignment, and to quote the whole visible screen. Managers on
2026-11-14 spent much of their context on that repetition. Wakes now carry the
goal, task and one-line objective, the budget line, the assignment in full only
on its first wake (its first line afterwards), events since the previous wake,
and only screen lines added since then; the contract is one `hgm show GOAL`
away. Notices for the person (dialogs, spent budgets) are unchanged.

Measured with the fake Herdr in `tests/bridge.rs`
(`wakes_carry_the_full_assignment_once_and_only_new_lines`; run it with
`-- --nocapture` to print the sizes): a contract with six acceptance items,
three invariants, two out-of-scope items and one stop condition, a 618-character
linked assignment and a 30-line screen. The patrol is the first wake after the
assignment; the settle wake follows when the worker finishes.

| Wake | Before (62eed89) | After | Change |
|---|---|---|---|
| Patrol (first wake after the assignment) | 5,505 bytes | 2,393 bytes | −57% |
| Settle (later wake, one new screen line) | 5,457 bytes | 1,479 bytes | −73% |

The patrol stays larger because it carries the full assignment once and, with
no earlier wake to compare against, the last 8 screen lines.

## Watch measurement: polling and events (2026-11-13, counted 2026-11-14)

Herdr calls and wake latency were measured on 2026-11-13 with Herdr 0.9.5 in
owned tabs against temporary state: a worker pane whose state was switched with
`herdr pane report-agent`, a stand-in manager pane that recorded each wake line
with its arrival time, and one watch per mode (`--polling` for polling). The
watch's own Herdr call counter splits calls by the worker's state; each state
was observed for about two minutes. Latency runs from the state switch to the
wake line in the manager pane.

| Mode | Herdr calls/h while working | Herdr calls/h while settled | Median wake latency |
| --- | --- | --- | --- |
| Polling before the event watch (by code) | 2160 (6 calls every 10 s) | 2160 | 0 to 10 s by poll phase; not measured |
| Polling (`--polling`: 10-second poll with a full re-read) | 836 (33 calls in 142 s) | 832 (35 calls in 152 s) | 3.40 s over 5 transitions (2.75 to 4.15 s) |
| Events | 359 (12 calls in 120 s) | 269 (9 calls in 120 s) | 0.53 s over 6 transitions (0.48 to 0.78 s) |
| Events, the user's main watch on the deployed build (2026-11-14) | 544 (57 calls in 377 s) | 479 (218 calls in 1639 s) | not measured |

Polling latency depends on where a change falls in the poll interval (about
10.45 s); the five transitions moved 0.37 s later each, so they cover only part
of that range. The last row is the same counter in the watch of a real Claude
Code main over 34 minutes without a switch to polling; no polling counterpart
was recorded on a real worker.

Wakes per assignment and their actionable share come from real goals, not the
harness. Builds before the event watch record no `wake` timeline event, and the
event build records one only for a wake typed into the manager pane, not for a
wake delivered through the manager's Stop hook (all four event-mode wakes
below). Wakes were therefore counted in the Goal Manager's Claude Code
transcripts as user turns that begin with "Goal Manager observation" or "Stop
hook feedback"; assignments are the goal timeline's `assigned` events in the
same window. A wake is actionable when the manager ran a state-changing `hgm`
command (`send`, `correct`, `reanchor`, `wait`, `complete`, `note`,
`supervise`, `pause`, `cancel`, `approve-plan`) before the next wake.

| Mode | Sample (2026-11-13, UTC) | Wakes | Assignments | Wakes per assignment | Actionable |
| --- | --- | --- | --- | --- | --- |
| Polling (build before the event watch) | 02:31 to 09:08, one goal, two manager sessions | 6 | 4 | 1.5 | 4 of 6 (67%) |
| Events | 16:14 to 17:12, that goal and its successor, one manager session | 4 | 2 | 2.0 | 3 of 4 (75%) |

One assignment in each window went to impl, which the watch did not observe;
counting main's assignments only gives 2.0 and 4.0 wakes per assignment.

- Polling wakes: `done` right after a send (the previous turn's state; the only
  command was a note calling the wake useless), two 5-minute patrols with no
  action, a due goal check (`reanchor`), `idle` after a plan (`approve-plan`
  and two corrections), and `done` before a handoff (restart, then `send`).
- Event wakes: main settled with checks passing (`wait`, time budget nearly
  spent); two context-rot signs that were a false positive from Claude Code's
  "Ran N shell commands" lines, since fixed (`note`, `pause`, rearm); main
  settled 15 s after the manager had completed that task from its own status
  check (its next command, 26 minutes later, followed that check, not the
  wake); and no change for 27 minutes while a user draft blocked delivery (no
  command; the manager had acted 11 s before).

Judged by whether the wake itself led to a new decision, polling has 3 of 6 and
events 1 of 4, plus one that only cleared a false alarm. Both samples are too
small to compare the modes. The share follows the wake policy (patrol wakes,
a stale `done` after a send, false context-rot signs), which changed together
with the event watch, more than the event transport. During the event sample
the manager also watched main with its own status loop, which noticed two of
these four conditions before hgm woke it.

## Native checks (2026-11-12)

All checks use owned validation tabs and small bounded tasks. Native startup and
response were observed with Claude Code 2.1.322, Codex 0.167.1, and Copilot 1.0.109.
Claude completed the saved-handoff, process exit/restart, and successor receipt
cycle in the same terminal. The Codex and Copilot restart cycles are not yet
verified; startup success is not recorded as task success.

CSV task check: one manager per provider (Claude Code, Codex, Copilot) shared
one folder with a three-row `orders.csv`. Each manager registered a goal, sent
the task to its pinned original agent with `send`, inspected the written
`<provider>-summary.json`, recorded task evidence, and paused for human
acceptance without accepting. All three files held `item_count=3`,
`total_quantity=6`, `total_amount=1760`, matching an independent recount.
Copilot's supervise call opened the watch tab and its wakeup reached the
manager after the worker finished.

Observations that changed the build:

- Copilot asked before every `"$HGM_BIN" ...` call and draft write, about
  fifteen approvals for one small task. Managers now run plain `hgm` from a
  session PATH entry, and Copilot pre-approves only `shell(hgm:*)` and writes
  under the draft folder.
- All managers in one folder overwrote a single brief. Briefs are now keyed by
  the original terminal.
- Without its own task, the Copilot manager read the other managers' goals and
  guessed an ID. The prompt now limits action to goals this conversation
  created or the user names.
- Copilot and Codex received the whole guide as the first user message. Copilot
  then loaded a user-level Herdr control skill and tried to call `herdr`
  directly. The guide now travels as Codex `developer_instructions` and as a
  Copilot `.github/instructions/*.instructions.md` file in a private
  `COPILOT_CUSTOM_INSTRUCTIONS_DIRS` directory (Copilot 1.0.109 applied a test
  rule from that form; it ignored `AGENTS.md` and `copilot-instructions.md`
  there). The prompt also keeps original-tab control inside `hgm target`/`send`.
- After the PATH change, plain `hgm` calls and draft writes ran without
  prompts. Remaining prompts came from `printf` separators and
  `printf ... | hgm brief --save -`; `printf` is now pre-approved and briefs
  are written to a draft file first.

Recheck with the instruction-based launch (2026-11-13): a fresh Copilot manager
for the same original tab received only the short opening message, recovered
the previous decisions from that tab's brief, and ran `hgm` and draft writes
without prompts. For a new task (`copilot-price.json`, rows with unit price
200 or more) it registered, sent, supervised, recorded evidence and paused for
review. The only manager prompt was its own independent `python3` check.
It did not load Herdr control skills. The Codex `developer_instructions` launch
has unit coverage only; no live Codex turn was run in this recheck.
- `supervise` failed with `plugin_pane_not_found` while Herdr still held an
  older linked manifest without the `monitor` entrypoint; relinking fixed it.
  The error now names `herdr plugin link`.

## Limits that matter

- The timeline is an audit and catch-up stream, not event sourcing: goal records
  remain the source of truth, and a crash between saving a record and appending
  its events can drop those events.
- Notifications are best-effort (`herdr notification show`, 3-second bound) and
  only fire inside a Herdr-bound manager context. Approval detection reads known
  dialog phrases; an unfamiliar dialog may only appear as a stalled agent.
- Enforced in code (2026-11-13): the 800-word contract budget (two CJK
  characters per word) unless `long_spec_reason`; `auto` only with a command
  check; confirmation, `ask` approval, plan approval under `plan_first`, and the
  assignment budget for linked sends; a 400-word cap on every assignment; the
  contract appended to every linked assignment; protected-path changes,
  succeeding `stop_checks`, the time budget, and the handoff threshold stop or
  hand off; two new context-rot signs (reflexive agreement, a command line
  shown three times) wake the manager with the screen evidence, and the
  manager decides whether to start a handoff with `hgm handoff` (the watch
  never hands off on them, since the same phrases occur in ordinary work);
  revision feedback is listed as candidate AGENTS.md rules.
- Still judged by the manager: free-text `stop_when`, "meaningful progress" per
  round, other context-rot signs (drift, broken rules, sloppy edits), whether an
  inspection was thorough, and proposing AGENTS.md rules. Budgets count linked
  assignments, not provider turns or tokens; no token budget is enforced.
- Protected paths are content digests of workspace files, compared on each watch
  tick; large protected trees make the tick slower. A change detected while no
  watch runs is found at the next watch.
- The manager guide plus command reference is long; rules buried in it may be
  followed less reliably than the short wake notices.
- Agents hgm launches (manager, impl, reviewer) run with the provider's
  allow-everything flag by user decision (2026-11-13), so they have no native
  permission prompts. The original agent keeps its own options: its prompts
  (for example Copilot's file-creation dialog) still wait for a human, and the
  manager reports them instead of answering.
- Claude Code's sandbox blocks Unix sockets it does not list, and every `hgm`
  and `herdr` call uses Herdr's socket; the CLI is not a way around it. A Claude
  manager's settings allow `HERDR_SOCKET_PATH`; another sandboxed agent must
  allow that socket itself (`hgm` explains a refused socket in its error).
- Herdr 0.9.5 reported a Copilot "Create file" approval dialog as `done`, not
  `blocked`. The manager found it by reading the target screen; state labels
  alone do not reveal every approval prompt.
- Herdr 0.9.5 event stream (observed 2026-11-13): one `events.subscribe` per
  connection; `pane.output_matched` fires once per subscription and also for
  text already on screen, so hgm re-arms it after each state change; a process
  exit arrives as `pane_exited`, but closing a tab sends no event, so a closed
  worker pane is found by the reconcile read (`watch.reconcile_secs`, default
  30 s) rather than at once. A state change without an event, or an ended
  stream, switches the watch to polling until a new subscription works.
- The Herdr sidebar has no expiry. hgm clears it when the watch ends, moves to
  another role, or is recovered as stale; a watch process killed from outside
  leaves the last values until the goal is supervised again.
- Herdr types an agent launch into the pane's shell: arguments cannot contain
  line breaks and the line must stay under 1024 bytes. Role guides therefore go
  through a file (Claude `--append-system-prompt-file`, Copilot instruction
  folder) or, for Codex, a private `codex` launcher first on the role tab's PATH.
  A provider's own folder-trust question (Claude, Copilot) still appears the
  first time a role starts in a new folder and waits for the user.
- Context-rot detection reads screen phrases, so a screen that shows the phrase
  list itself (for example a diff of this code) counts as one sign.
- Herdr input admission and completion are different. Composer checks are
  conservative screen parsing, not atomic input locks. A simultaneous human
  keystroke can race inspection. Unknown UI formats require manual inspection.
- Context telemetry currently recognizes observed Codex/Copilot footers and the
  Claude `ctx` status-line format. Missing telemetry is unknown, not zero. It
  cannot interrupt an active turn at exactly 85%.
- The 85% handoff covers the original agent. The manager does not start a
  successor for itself: goals, evidence, the brief and watch state are saved
  outside its conversation, so reopening Goal Manager from the same tab resumes
  them. Its own provider may still auto-compact.
- No compact command is sent, but provider auto-compaction remains independent.
  In particular Copilot documents background compaction at 80% by default.
  The 85% policy is not yet an enforced no-compaction guarantee for all providers.
- Codex shared background servers can retain another terminal's environment.
  Manager launch uses `--no-daemon` and removes inherited `CODEX_THREAD_ID`.
  Existing agents without a trustworthy native session ID cannot restart safely.
- Restart reproduces validated process launch arguments. Interactive changes to
  model/permissions are not automatically exported. Unsupported launch forms
  retain the predecessor instead of guessing equivalent permissions.
- Watch state survives CLI exits; server restart recovery requires inspection
  and rearming. This is not a system service or a durable fleet scheduler.
- Pause stops manager wakeups; independently running native agents require their
  own cooperative stop. Completed work is not silently replayed.

Publication remains gated on task/restart checks and an honest usable context
policy. No all-provider unattended reliability claim is made.
