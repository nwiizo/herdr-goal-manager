# Changelog

## 0.1.0 (unreleased)

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
