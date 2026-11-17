# Changelog

## Unreleased

Interaction design (verified with controlled executables):

- Attention accounting from each goal's timeline: wakes delivered and how many
  a manager action followed before the next wake, total and longest idle time
  short of completion, notifications to the person, time awaiting review,
  corrections and refused commands. In the packet (`attention`, and one line in
  the text), in every `status --json` row, and per goal in the new `hgm
  attention` (`--all`, `--finished`). A linked send, correction or goal check
  refused before anything is typed is now recorded as `refused`; the refusal
  itself is unchanged. Nothing here completes or accepts anything.
- Prompts revised as a whole: the manager guide is rewritten in the order of
  situations (talking, notebook, contract, assigning, while working, when
  stopped, review, handoff) with one vocabulary, each rule once and every
  code-enforced refusal named where it applies, within its length before
  2026-11-15 (2,135 words, enforced by a test); the impl, reviewer and factory
  guides, the text appended to assignments, corrections, goal checks and the
  handoff request each have a word budget enforced by a test. The guides agree
  that removing a checkout is the user's call.
- Wakes no longer repeat a list of standing rules: they carry what changed, the
  cause and, where the cause lacks one, a single next instruction, with
  pointers to `hgm show` and `hgm log`.
- A goal check asks the worker to restate the goal once its current step is
  done and to record it with `hgm report`; the next wake says when no
  restatement was reported. A correction points to the contract instead of
  repeating its acceptance and invariants; assignments still carry it in full.
- The native goal's condition says the worker may stop by stating plainly that
  a contract boundary, a stop condition or a decision only the user can make
  blocks the work (still under 700 bytes).
- `docs/prompt-checklist.md`: twelve situations from real use, each with the
  action the prompts should lead to.
- `docs/design.md` (linked from the README): the three readers (the person,
  the Goal Manager, the worker), what each is shown and when, and ten
  principles, each with the observation behind it and how it is checked.
- Guard tests: every refusal of admission and contract validation names its
  next step (contract validation now ends with `hgm add --file PATH
  --validate`, and two admission refusals gained one); human-readable times are
  local; a wake about an unchanged state stays at most 1,000 bytes; `add`,
  `confirm`, `complete` and `run` print at most three lines by default.

Usability fixes from a day of use (verified with controlled executables):

- `hgm report` is for main and the manager too, not only `impl`: text or
  `--file PATH`, up to 3,000 words, refused for a reviewer, an unknown task or
  a finished goal. Every linked assignment ends with the exact command. The
  report is kept in full on the timeline: `hgm log` prints it under its line,
  `hgm packet` lists every report (`reports` in JSON), so the manager no
  longer reads provider transcripts. An `impl` report still wakes the manager.
- `hgm target --lines N` (1-5000) returns that many recent lines from Herdr's
  scrollback instead of the visible screen; state is still judged from the
  visible screen.
- Quiet patrols: a patrol that finds the worker working with visible progress
  since the previous patrol and no finding is recorded (`patrol` with
  `quiet: true`) and wakes nobody. `loop.patrol_wake: always` (contract field or
  config default) wakes the manager on every patrol; goals confirmed earlier get
  the quiet default. Goal checks and every other wake are unchanged.
- A wake names the goal and task of the latest linked assignment to the watched
  pane among the goals the watch serves, with that goal's budget line, and a
  linked send for another goal to that pane joins the running watch (both in
  this build since the shared watch; the observation came from v0.1.0).
- Prepared steps queue: up to 5 per role, delivered in order, one each time the
  worker settles, under the same conditions as before. `send --withdraw --step
  N` removes one, `--withdraw` all; a direct send supersedes them all. A step
  stored by an earlier build reads as a queue of one.
- Human-readable times (`log`, `show`, `status`, `packet`, wakes, run class
  reasons, a removed checkout's message) are local with their offset, e.g.
  `2026-11-15 13:53:00 +09:00`; JSON keeps UTC.
- `hgm add --validate` reports a contract's problems and its words per field
  without registering it (confirmation's own refusal included), and fails when
  there is one; the over-budget refusal of a plain `add` names the words by
  field too. `--check` stays the option that adds a command check.
- `hgm add --replace GOAL` replaces the contract of an unconfirmed goal in place
  (same id, owner and timeline, `contract_replaced` recorded), so no cancelled
  draft is left; a confirmed or finished goal is refused.
- `status` and `list` leave out completed and cancelled goals and say how many
  (`--finished` shows them); `--json` still lists every goal.
- `add`, `confirm`, `complete`, `run` and `resume` print the id, state and next
  command (and the goal's message, such as a `require_review` hold); the global
  `--verbose` prints the whole goal as before; `--json` is unchanged.
- `confirm` warns, without refusing, when other confirmed unfinished goals work
  in the same directory, and names the contract `checkout` option.

Native check (2026-11-16, Herdr 0.9.5, Claude Code 2.1.325, Copilot 1.0.112;
see docs/verification.md) and what it fixed:

- The watch reads the worker again right before every input it types (native
  goal set, continue, goal clear): a Claude Code dialog that opened while the
  settle checks ran took a `/goal` line typed from the earlier observation.
- Input not known to have arrived is recorded as uncertain: a native goal set or
  clear whose input line did not empty or that got no `Goal set:` / `Goal
  cleared:` answer, a continue that was not taken (the manager is told), and
  manager wakes and notices (`prompt_unconfirmed`, `queued_unconfirmed`,
  `direct_unconfirmed`).
- The native goal condition is at most 700 bytes and names long check and
  boundary lists by reference to the contract: Claude Code takes a typed
  `/goal` longer than 800 characters as pasted text, not as a command.
- `Goal paused` is read even though Claude Code keeps `◎ /goal active` above the
  input line afterwards.
- A settle after newer work is no longer dropped with an older held wake as
  stale; a pending wake is dated by the worker's last observed change.
- A running tool's clock and blinking bullet on Claude Code no longer count as
  progress, so a long silent tool run is recognised as no progress.
- A pending no-progress wake keeps one work-dir note when the watch reloads.

Review flow (verified with controlled executables):

- `hgm decision GOAL "what" --why "why" [--task TASK]` records a choice a
  worker made inside the contract on the goal's timeline; every linked
  assignment ends with the exact command (main gets this build's path and
  folder, since it has no `hgm` on its PATH). `impl` may run it too.
- `hgm follow-up GOAL "what" [--task TASK]` records a finding that should not
  block; a reviewer's advisory findings count as follow-ups. Both are listed
  apart from blocking findings.
- `hgm packet GOAL` (and `--json`) gathers acceptance items, tasks, the latest
  result of every registered check, assignment, correction and revision counts,
  decisions, follow-ups, blocking findings, review verdicts and what the
  timeline marks as not verified. It completes nothing and accepts nothing.
- Each review verdict stores a fingerprint of the goal's work dir (Git state in
  a repository, file sizes and modification times elsewhere, build output
  excluded); a verdict whose fingerprint no longer matches is shown as stale in
  `show`, `status` and the packet.
- `loop.require_review: true` (contract field or config default): `hgm run`
  and `hgm resume` move a goal to review only with a current approval from a
  reviewer on another provider than the workers and no current revise; the
  refusal names the next commands. Off by default.
- The reviewer guide and each review request: judge the files and the diff,
  not a summary; block only on a failing command or a wrong line; everything
  else is advisory and becomes a follow-up.
- Check results keep the work dir's fingerprint (`run`, `accept`, settle
  checks); a pass on an earlier state is listed as not verified in the packet
  and `show`; older results without one are `unknown` in the packet.
- `tasks[].write_paths`: linked assignments of such a task say where to write;
  changes outside those paths during its assignment are recorded
  (`scope_exceeded`) once per path, added to the manager's wake and listed in
  the packet. Writes are never blocked.
- A wake about a worker that works without progress says when the working
  directory last changed and whether the files still move.
- hgm's own git reads run with `GIT_OPTIONAL_LOCKS=0`, so they never hold the
  index lock a worker's git needs.
- `factory.checkout` (default: on in a Git repository): `queue next` gives the
  new goal its own checkout on branch `hgm/<item>` from the current branch,
  unless the contract names one; a checkout that cannot be made fails the item
  without holding the next; outside Git an explicit `true` dispatches into the
  workspace and says why; `false` keeps today's behaviour. `queue next`,
  `queue list` and the packet show the checkout's path and branch.

Native goal (verified with controlled executables; native checks pending):

- For a Claude Code main under autonomy auto, the watch sets `/goal` after each
  linked non-plan assignment once main has settled, so it keeps going between
  turns, and clears it when the goal is ready for review, completed, cancelled,
  paused or waiting, when a stop condition, protected path or budget holds, and
  before a handoff. The condition comes from a template and contract fields only.
- New `loop.native_goal` (contract field and config default); `false` turns it
  off. Codex and Copilot stay off.
- The outcome on screen (met, impossible, paused, cleared after an error,
  unavailable) is recorded as an observation and wakes the manager; it never
  completes a task or accepts a goal. `status`, `show` and the Goal watch tab
  show the native goal and its last outcome.
- While the native goal drives main, the watch types no continue and no
  prepared step.
- Design rule in the README and the manager guide: where a provider has an
  official mechanism, hgm uses it as documented.

Fixes from an independent review (Codex, 2026-11-17; each with a test):

- Stop checks run at every patrol, also a quiet one under `patrol_wake:
  finding`; before, a stop condition went unseen until another cause woke the
  manager.
- A native goal that a draft or a dialog kept from clearing when its goal
  stopped is cleared by the ending watch once the input is free (retried for up
  to 30 minutes, then the user is told); before, `wait`, `pause` and `cancel`
  ended the watch and the goal stayed set. The same holds for the Goal
  Manager's goal.
- `/goal clear` goes only to the bound session: a session started in the pane
  since is not typed into (`native_goal_cleared` `delivery: skipped`).
- A stop reads the watch record under the operation lease, so it no longer
  writes back a copy older than a watch step's save.
- A direct send to a watched tab takes the operation lease, like the watch's
  delivery of a prepared step, so the two never type at once and the steps it
  supersedes change under the lease that prepare and withdraw take.
- A prepared step is marked as being typed before the watch types it; a watch
  that stopped in between records it as uncertain on restart and never types it
  again.
- After a second review: a native goal a stop left set is carried to the next
  watch of the pane and cleared unless its goal is watched again, also on a
  shared watch; the ending watch takes the operation lease before the watch
  lease, so a watch `supervise` starts meanwhile is not shut out; after an
  uncertain prepared step the next waits until the manager prepares, withdraws
  or sends; a prepared step of another goal on a shared watch is held at that
  goal's protected paths and time budget; a direct send holds the operation
  lease from admission until the native goal it queues, so a budget spent
  meanwhile refuses it; and stop checks run before a settle-time native goal.

Native goal where it helps (verified with controlled executables; the native
check is in docs/verification.md, 2026-11-16):

- `loop.native_goal` takes `assignment`, `settle` or `false`. A newly
  registered contract gets `assignment`: right after a linked non-plan
  assignment to a Claude Code main under autonomy auto, `hgm send` types the
  `/goal` as queued input under the delivery rules. It is recorded as pending
  (`native_goal_queued`) and becomes set only when `Goal set:` for that goal and
  task is read; when main settles without it, it is uncertain and never typed
  again. Stored contracts keep what they had (no field, or `true`, is `settle`:
  set after main first settles, as before). The settle checks and the manager's
  wake still come when the goal ends and main settles.
- New `hgm await [--timeout SECS]` (default 1800): the manager runs it in the
  background at the end of a turn. While it runs, the watch hands it the wake it
  would have typed, unchanged, so a draft in the manager's input no longer holds
  a wake; the wake is recorded `via: await`, and review notices go to it too. It
  returns at once when the watch has ended. Without an await nothing changes.
  An await counts as running only while its lock stays held for 100 ms: a
  process spawned by another thread keeps a copy of a just-released lock until
  it executes its program, which could have sent a wake to no await.
- The Goal Manager's own native goal: for a Claude Code manager the watch sets a
  short `/goal` (fixed text and the set time, about 330 bytes) while a
  confirmed goal of the tab is short of completion, and clears it when none is
  or the watch ends. Its exits: every goal of the tab ready for review, waiting,
  completed or cancelled, or a statement that only the user can decide. Pending
  until its own `Goal set:` shows; after an end it is set again only after a
  manager action. New config `manager.native_goal` (default on; `false` turns
  it off). Other providers' managers get nothing.
- A native goal that ended where hgm did not see its outcome (natively, a
  queued goal met at the stop that ran it printed `Goal achieved` above its
  `Goal set:`) is recorded as ended once the settled agent shows no `◎ /goal
  active` indicator above its input line: on the Goal Manager and on main
  (`native_goal` with `read: indicator`), named by the outcome line when it is
  still on screen. Checked natively on both.
- Spin guard: three continuations of the manager's goal with no `hgm await`
  running and no manager action clear it once, notify the user (`attention`,
  cause `manager_goal_spinning`) and keep it off for that manager session.
- `status` rows and `show` carry `manager_goal` (`set`, `outcome`, `guarded`)
  next to `native_goal`; the Goal watch tab shows a `manager /goal` line. New
  timeline events `manager_goal_set`, `manager_goal`, `manager_goal_cleared`.
- The manager guide tells a Claude Code manager to end each turn with `hgm
  await` in the background and never to type `/goal`, within its 2,135 words.

Factory mode, first stage (off by default; with it off nothing changes):

- `factory` section in the user and project config (`enabled`,
  `max_awaiting_review` 2, `max_active` 1; a project may only lower the limits).
- `hgm queue add/list/park/unpark/remove/next`: a local queue of user-agreed
  contracts in hgm's private state (one file per item, an append-only ledger),
  duplicate content refused, exclusive claiming so two managers never create two
  goals from one item, recovery of a claim a stopped dispatch left, and a failed
  item (with its unconfirmed goal cancelled) instead of a stuck queue.
- `queue next` holds while results awaiting review or goals in progress reach
  the limits, naming the goals; `overview` and `inbox` show the same reason, and
  `overview` says when queued work is ready.
- A run class (success, flawed, blocked, manual) for each goal taken from the
  queue in `hgm show` and `hgm status`, computed from recorded events only.
- The manager guide and command reference gain the queue only where factory
  mode is on.

Keeping main working (verified with controlled executables; native checks
pending, see docs/verification.md):

- A wake about main stopping after new work skips the wake cooldown and reaches
  a working Claude Code or Copilot manager as queued input under the rules of
  `send --interrupt`; Codex managers still get it when their turn ends. A wake
  older than work sent to the watched pane is dropped.
- `hgm send --prepare` stores the next step per role; the watch delivers it
  through normal admission when main settles, or keeps it and says why.
  `hgm send --withdraw` removes it; a direct send supersedes it.
- After a provider error ends main's turn, the watch types a fixed continue
  itself (at most three an hour) and records it.
- Working without progress is recognized: spinners, timers, token counts, tips
  and retry counters do not count as progress, and a retry notice is named.
- `hgm status`, `hgm inbox` and the Goal watch tab show how long main has been
  idle short of completion; you are told once per stretch past the new
  `watch.idle_notice_secs`.
- The watch's times default to three minutes (`loop.patrol_minutes`,
  `watch.idle_secs`, `watch.cooldown_secs`, `watch.idle_notice_secs`);
  `watch.max_wakes` defaults to 160 (at most 500). Goals confirmed earlier keep
  their stored values. `hgm supervise` applies new timings to a running watch
  (`--idle-secs`, `--cooldown-secs`, `--max-wakes`, `--idle-notice-secs`,
  `--patrol-minutes`) and says which it did not apply.
- `hgm approve-plan` without `--task` approves the one task whose plan is
  pending; the plan-first refusal names the exact command with `--task`.
- The manager guide, linked assignments and the impl and reviewer guides ask
  for autonomous work inside the contract, reporting choices afterwards;
  every code-enforced refusal is unchanged.
- `hgm --help` no longer mixes the approve-plan and inbox descriptions.

## 0.1.1 (2026-11-14)

- Published on crates.io as a fallback install: `cargo install herdr-goal-manager`
  installs `hgm`, and the new `hgm plugin-folder` makes a Herdr plugin folder
  around it (the manifest and a link to the installed binary) for
  `herdr plugin link`. It leaves a folder holding another plugin or an hgm build
  alone.
- README: installing from crates.io, and uninstalling it.

## 0.1.0 (2026-11-14)

First public version.

- Goal Manager tab for Herdr (`herdr-goal-manager.open`): an ordinary Claude Code,
  Codex or Copilot conversation that matches the original tab's agent and keeps
  goals, evidence and a brief outside the conversation.
- Loop contracts: observable acceptance, invariants, boundaries, open questions,
  checks, and a `loop` policy (assignment, correction and time budgets, autonomy,
  plan-first, protected paths, stop checks, 5-minute checks of main, goal checks
  every 45 minutes, handoff at 85% context). Confirmation, word budgets and
  budgets are enforced in code.
- Linked assignments to the pinned original agent (`send --goal --task`), short
  corrections (`correct`), goal checks (`reanchor`) and interrupt delivery to a
  working Claude Code or Copilot with an empty input line. Interrupt delivery
  stays off for Codex; Codex's native checks are pending.
- Monitoring in a background tab: wakes on settled work, patrols, holds after
  repeated unchanged observations, separates provider errors from usage limits,
  sign-in and dialogs, runs checks when the worker settles, and notifies through
  Herdr. Durable context handoff and restart in the same tab.
- Event-based watching over Herdr's socket with a reconcile read and a
  10-second polling fallback; a Goal watch tab redrawn in place, with an
  optional running sheep that repeats notifications (`watch.sheep`), and goal
  state in the original pane's Herdr sidebar. Watch, roles and handoff state
  belong to the original terminal and survive a reopened Goal Manager.
- Helper roles: an implementer and reviewers in their own tabs (`role start`),
  one owner per task, recorded reviews and reports. A role that never starts
  (a provider stopped at its own folder-trust question) closes its tab.
- One Goal Manager per original tab: reopening switches to the running one, and
  two launches at the same moment start one. A running role is not started
  twice. One Goal watch per tab: a new watch closes the ended one, and a
  running watch continues in a redeployed build.
- Work keeps moving: a stopped main wakes the manager to send the next step, a
  network error on screen (e.g. ENOTFOUND after a sleep) wakes it to continue,
  repeated stops without a next step notify the person, and a watch ends only
  when the goal is completed, cancelled or waiting for the person (passing
  observation failures are retried). A review verdict notifies the person and
  wakes the manager even while no watch runs, and a context handoff continues
  after its watch ended.
- Context handoff restart for providers that report a session only after a
  first turn (Copilot): the successor is adopted as another process in the same
  terminal, and its acknowledgement must carry the session Herdr reports.
- Screen reading that tells Claude Code's faint input suggestions from a typed
  draft, treats Copilot as working while its status line offers to interrupt
  (Herdr lists it `done`), and notifies once for a dialog that stays on screen.
- User and project configuration for guide additions, opener, wake and
  assignment text, and loop and watch defaults (`config`).
- Timeline, inbox and overview for catching up.

Known limits: Copilot roles need a folder Copilot already trusts, and Copilot
managers need an explicitly chosen capable model. Native checks on Codex are
pending (see `docs/verification.md`).
