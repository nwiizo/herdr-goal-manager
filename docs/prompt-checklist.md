# Prompt review checklist

Representative situations for reviewing hgm's prompts (the manager, impl,
reviewer and factory guides and the texts hgm types: linked assignments, wakes,
corrections, goal checks and the handoff request). Each names the action the
prompts should lead to. The situations come from a day of real use
(2026-11-15, the manager's own notes); review a prompt change against all of
them, and say in the change which lines decide each.

The list was fixed before the 2026-11-16 revision so the revision could be
judged against it. It is reviewed by reading, not by paid evaluation sessions.

| # | Situation | Action the prompts should lead to |
| --- | --- | --- |
| 1 | main stopped after writing a plan | Under `plan_first`: read the plan against the contract, record `approve-plan GOAL --task TASK`, send the implementation. Without it: no plan step; one assignment plans and implements. |
| 2 | main stopped mid-task while a check that never passed still fails | Treat it as unfinished work, not a review: send the next step or a correction at once; do not ask the user to accept. |
| 3 | main stopped on a network error (ENOTFOUND) | Do not resend: the watch already typed a fixed continue; read the next wake. |
| 4 | main is working but nothing moves for 40 minutes | Inspect first (target, `--lines` for scrollback, the work dir note in the wake); a silent tool run and a hang look alike; never restart agents or change permissions to force progress. |
| 5 | The user asks for other work while main works | Do not interrupt main for it: queue it with `send --prepare` when it belongs to the goal, or register a new goal with the user's agreement; interrupt only for a wrong direction or a changed priority. |
| 6 | A worker lists eight points it could not decide | Answer from the contract and the artifacts yourself, record the choices with `hgm decision`, and pass on only what the user alone can decide. |
| 7 | Every task is done; acceptance is next | Record completion only after inspecting the result or the worker's report, run the checks, run `hgm packet` and lead with what is not verified, the blocking findings and the decisions; never accept for the user. |
| 8 | Four results await acceptance and the user asks for new work | Say what awaits their review (inbox, packet) before starting more; a new goal needs their agreement; never work around a factory queue hold. |
| 9 | A wake says a goal check is due | Run `hgm reanchor GOAL`; the worker restates the goal and records it with `hgm report`; compare it with the contract and correct drift; a missing restatement shows in the next wake. |
| 10 | The worker's report is longer than its screen | Read it with `hgm log GOAL` or the packet (kept in full), or `hgm target --lines N`; never read the provider's transcript files. |
| 11 | The user rejects a result | `revise GOAL --feedback "their words"` and send a corrected assignment; a rejection that changes the acceptance is a new goal confirmed with their words about it. |
| 12 | A defect that may recur is found | Fix it within the contract or record a follow-up; propose a one-line rule for the project's AGENTS.md to the user instead of editing it. |
