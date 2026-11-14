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
executors, local evidence, and explicit acceptance. It does not provide TAKT's
complete software-factory lifecycle or Quartermaster's worker fleet.

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

Prompts and evidence are data passed through stdin/environment, not shell source.
AI runners still need their own sandbox and external-service permissions.
Persistence retains the imported contract, task attempts, plan attempts, check
rounds, repair attempts, user guidance and final acceptance. Interruptions do
not reset the attempt budget. No rollback of external effects is implied.

## Next additions require a concrete use case

Useful follow-ons include a richer panel, artifact references, executor profiles,
and an adapter to an existing PR workflow. A generic daemon or parallel scheduler
should follow demonstrated needs rather than precede basic recovery and acceptance.

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
