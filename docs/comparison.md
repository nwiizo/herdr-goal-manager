# Scope and comparison

Sources reviewed on 2026-11-12:

- [TAKT manager article](https://zenn.dev/nrs/articles/d3548bbddc182a)
- [herdr-quartermaster README](https://github.com/travisjeffery/herdr-quartermaster)
- [herdr-tasks README](https://github.com/dlv-gold/herdr-tasks)

These are documented capabilities, not exhaustive claims about uninspected code.

| Tool | Documented emphasis | Consequence for this project |
|---|---|---|
| TAKT manager | Goal contracts, decomposition, dependencies, integration checks, repair/recovery and PR/merge lifecycle | Preserve the distinction between finishing tasks and accepting the goal; defer PR/merge automation |
| herdr-quartermaster | Coordinator/worker dispatch, Beads, worktrees, PR feedback, notes/inbox and non-PR investigations/operations | Noncoding work alone is not a differentiator; avoid rebuilding its Git/worker machinery |
| herdr-tasks | Daily/weekly missions, progress checkboxes, reporting and accepted AI suggestions | Provide execution and verification state alongside human work, rather than another daily reporting view |
| Herdr Goal Manager | Explicit acceptance contract, manual and configured command tasks in one dependency graph, durable attempts, bounded repair and acceptance-time rechecks | Offer the same workflow from any directory, with Herdr as an optional terminal interface |

The initial implementation deliberately covers a smaller execution model:
one active manager per canonical workspace, sequential ready tasks, configured
executors, local evidence, and explicit acceptance. The optional factory mode
(first stage) adds the parts a software factory needs before scale: a local
queue of user-agreed contracts, exclusive claiming so one contract never becomes
two goals, kept evidence of every item's history, and dispatch that holds while
human review is behind, with a run class per goal computed from recorded
events. It still does not create, update or merge pull requests, take work from
issue trackers or chat, or start several agent loops or worktrees in parallel,
so it is not TAKT's complete lifecycle or Quartermaster's worker fleet.

## State transitions

Imported goals are ready. Running may lead to needs_input (human work,
dependency blockers, failures or exhausted attempts), paused, cancelled, or
ready_for_review. Only explicit human acceptance with currently passing checks
produces completed. Cancelled and completed goals are terminal.

A task's successful exit is evidence of execution, not proof of its business
criterion. Executable checks and human acceptance supply that separate decision.
Acceptance text is an accountable record, not an identity/authentication system.

## Data and trust boundaries

Goal YAML is a trusted, user-authored executable configuration. Planner output
is untrusted structured data: exact fields, bounded task count, allowed runner
names, dependency validity and acyclicity are checked before dispatch.
Commands and acceptance checks remain fixed in the imported contract.

A queued contract is the same trusted, user-authored file with the user's
agreement attached when it is queued. hgm stores its text verbatim with the
`loop` defaults in effect, records a content fingerprint in the append-only
ledger, and refuses to dispatch it when the stored text no longer matches that
fingerprint; the goal is built from that text alone, so nothing a model writes
becomes executable. Factory settings cannot switch off confirmation, budgets,
word caps or acceptance, and a run class is an observation, never evidence of
completion.

Prompts and evidence are data passed through stdin/environment, not shell source.
AI runners still need their own sandbox and external-service permissions.
Persistence retains the imported contract, task attempts, plan attempts, check
rounds, repair attempts, user guidance and final acceptance. Interruptions do
not reset the attempt budget. No rollback of external effects is implied.

## Next additions require a concrete use case

Useful follow-ons include a richer panel, artifact references, executor profiles,
and an adapter to an existing PR workflow. A generic daemon or parallel scheduler
should follow demonstrated needs rather than precede basic recovery and acceptance.

## What came from TAKT manager (2026-11-14)

A day of managing hgm and ccswarm work showed which TAKT manager ideas Herdr
already supports, so they were adopted without new machinery:

- An isolated checkout per goal: `herdr worktree create` gives it, and the
  checkout appears as a Herdr workspace the person can open. hgm only records
  it, points assignments and checks at it, and removes it on request.
- One watch per pane serving several goals, each with its own wake budget,
  instead of refusing the second goal.
- Compact wakes that carry what changed, with the contract one `hgm show` away.
- Settle checks that ask for a correction only when a check regressed.

PR and merge automation stays out: hgm's job ends at evidence and the person's
acceptance, and the repository's own review and merge flow (or an adapter to
it) decides what lands. A daemon stays out too: the watch runs in a visible
Herdr tab per manager and stops with its goals, so there is no hidden process
to supervise, restart or secure.

## Conversation-first direction

The current interface prioritizes the recipient's ability to proceed and a small
next action, following [oi-owarasero](https://github.com/nwiizo/oi-owarasero)
(reviewed source revision `2e5f40bc1e63302d62574a513536def448efc5e7`). It opens a
native agent rather than reproducing TAKT's TUI. TAKT source was also reviewed at
`58c59360e2c22e1eebada2a0cda0214737f989a6`; its broader daemon and PR lifecycle are
outside the present implementation.

The user job is: when work stalls or loses context, decide the smallest useful
next step, send it to the existing working tab, and retain enough evidence to
resume without repeating completed work. A persistent graph alone does not
satisfy that job. The validation therefore includes native conversation, original
target dispatch, artifact inspection, pause, and context transfer.

The local ISUCON harness informed the observation/decision/action/verification
cycle, bounded wakeups, and Stop-hook recursion protection. Its competition-specific
instructions and fleet machinery are not copied into this generic tool. Idle
state is a reason to inspect, not a completed-result signal.
