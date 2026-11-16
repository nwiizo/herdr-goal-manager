# Interaction design: readers and principles

hgm has three readers. Each sees different things at different times, and the
prompts are hgm's interface to two of them. This document says what each is
shown and when, and the principles that decide it. Each principle names the
observation behind it (from a day of real use, 2026-11-15, and the native
check of 2026-11-16) and how it is checked: a test, a measurement, or review
only.

## The readers

**The person** (the user) talks with the Goal Manager and owns every decision
that matters: agreement to a contract, answers only they can give, and
acceptance. hgm shows them, unasked, only what needs them: a Herdr
notification when main shows a dialog, a goal is ready for review or waits for
them, a worker sits idle short of completion past `watch.idle_notice_secs`, a
review verdict arrives, a watch stops abnormally, or the spin guard cleared the
manager's `/goal`. On request they read
`inbox`, `overview`, `status`, `attention` and, before accepting, the packet.
They never see patrols or wakes.

**The Goal Manager** is an ordinary agent conversation. It reads the manager
guide once, at launch, with the command reference (standing rules live there
and nowhere else). After that hgm shows it wakes, typed by the watch, or handed
unchanged to its running `hgm await` (a Claude Code manager ends each turn with
one in the background, so a draft in its input holds nothing back), when a
worker settles, works without progress, shows a dialog, regresses a check, has
a goal check due, or a patrol finds something; each carries what changed and
the one instruction its cause needs. Command output is short by default and
every refusal names the next step. The manager reads further detail on demand:
`hgm show`, `log`, `packet`, `target --lines`. A Claude Code manager also gets its
own short native goal while a confirmed goal of the tab is in progress: its
exits are every goal ready for review, waiting, completed or cancelled, or a
decision only the person can make.

**The worker** (main, an `impl` helper, a reviewer) receives what hgm types:
a linked assignment (the manager's text, the contract, the checkout and write
scope, and the commands that record decisions, follow-ups and reports), a
correction that points back to the contract, a goal check that asks for a
restatement recorded with `hgm report`, the native goal condition (a Claude
Code main, queued right behind a linked assignment), and the handoff request. Helpers also read their role guide. A
worker writes back only through hgm (`report`, `decision`, `follow-up`,
`review-submit`), so the manager never needs the provider's transcript.

## Principles

1. **Interrupt only with a decision to make.** A patrol that finds the worker
   progressing with no finding is recorded and wakes nobody; the person is told
   only what needs them.
   *Observed:* about thirty healthy five-minute patrols woke the manager in one
   day, and none led to a correction.
   *Checked:* test `a_patrol_that_finds_progress_and_nothing_else_is_recorded_without_a_wake`;
   measured by attention accounting (wakes, and how many a manager action
   followed) in the packet, `status --json` and `hgm attention`.

2. **A message carries what changed and one instruction.** Standing rules live
   in the guides; wakes do not repeat the contract or a rule list.
   *Observed:* every wake carried a 110-word rule list and the full contract;
   the list changed no action.
   *Checked:* test `wakes_carry_the_full_assignment_once_and_only_new_lines`:
   a wake about an unchanged state stays at most 1,000 bytes, a patrol wake
   under 2,100 and a settle wake under 1,200.

3. **Everything a reader needs is in hgm's record.** Reports, decisions,
   follow-ups and goal-check restatements are recorded in full and readable with
   `hgm log` and the packet; scrollback is one option away.
   *Observed:* the manager read the worker's reports from the provider's
   transcript file, and three goal checks got no reply anyone could see.
   *Checked:* tests `main_files_its_full_report_and_the_manager_reads_it_in_log_and_the_packet`,
   `reports_appear_in_full_in_the_packet_and_the_text_log`,
   `a_goal_check_without_a_reported_restatement_shows_in_the_next_wake`.

4. **Say what a result was checked against.** A check result and a review
   verdict carry the work dir's fingerprint; one on an earlier state is listed
   as not verified.
   *Observed:* a pass at 13:53 still showed as verified after the same tree
   changed; verdicts could outlive the work they judged.
   *Checked:* tests `a_check_that_passed_on_an_earlier_state_is_not_verified_in_show_and_the_packet`,
   `a_verdict_goes_stale_when_the_work_dir_changes_and_require_review_needs_another_provider`.

5. **A refusal names the next step.** Every refusal of admission and contract
   validation says what to do instead, and nothing was changed.
   *Observed:* a plan approval without `--task` was refused without saying
   which command would work; four contract refusals said only the word total.
   *Checked:* tests `every_refusal_names_its_next_step`,
   `add_validate_reports_problems_and_words_per_field_without_registering`.

6. **Short by default, detail on request.** `add`, `confirm`, `complete` and
   `run` print the id, state and next command; `status` and `list` leave out
   finished goals and say how many.
   *Observed:* those commands printed the whole goal and the manager filtered
   them with grep; two cancelled drafts stayed in `status` all day.
   *Checked:* tests `add_confirm_complete_and_run_print_a_short_result_and_confirm_warns_on_a_shared_tree`
   (at most three lines), `status_and_list_hide_finished_goals_and_say_how_many`.

7. **One clock per reader.** Human-readable output shows local time with its
   offset; JSON keeps UTC.
   *Observed:* wakes showed local time and `hgm log` UTC; the manager read a
   one-minute stop as four.
   *Checked:* test `human_readable_times_are_local_with_their_offset_and_json_stays_utc`.

8. **Typed input is confirmed or recorded as uncertain.** hgm reads the worker
   again right before it types, and counts input as delivered only when the
   input line emptied (and, for a slash command, its answer appeared).
   *Observed natively:* a `/goal` typed from a three-second-old observation went
   into a Claude Code dialog; a `/goal` longer than 800 characters arrived as
   pasted text; both were recorded as set.
   *Checked:* tests `a_dialog_that_opens_while_the_settle_checks_run_gets_no_native_goal`,
   `input_that_the_worker_or_manager_did_not_take_is_recorded_as_uncertain`;
   measured natively (docs/verification.md, 2026-11-16).

9. **Rules grow by replacement.** Each prompt has a word budget; the manager
   guide may not exceed its length before 2026-11-15 (2,135 words).
   *Observed:* four goals added paragraphs to the guides one at a time; the
   manager guide grew from 2,135 to 2,950 words in a day, and nobody read it
   through.
   *Checked:* tests `each_guide_stays_within_its_word_budget`,
   `every_text_hgm_types_stays_within_its_word_budget`; review only:
   `docs/prompt-checklist.md`, twelve situations with the action the prompts
   should lead to.

10. **Observations never decide.** Reports, decisions, verdicts, native goal
    outcomes, run classes and attention accounting complete no task and accept
    no goal; acceptance is the person's, recorded in their words.
    *Observed:* a native goal's "Goal achieved" and a reviewer's approval both
    read like completion.
    *Checked:* tests `accounting_counts_wakes_acted_on_idle_stretches_notices_review_and_refusals`
    and the report and review tests above, each asserting that the task stays
    open or the goal unaccepted.

11. **An agent waits on a command, not on its input line.** Where the provider
    offers a native goal, hgm sets it where it keeps work moving: on main right
    behind the assignment, and on a Claude Code manager while a confirmed goal
    of the tab is in progress. The manager waits for news with `hgm await` in
    the background, which takes the wake the watch would have typed; a manager
    goal that keeps continuing with neither an await nor an action is cleared
    once and the person is told.
    *Observed:* on 2026-11-15 the manager typed `/goal` on main by hand four
    times, once too late and once too long to register; a wake waited behind a
    draft in the manager's input; a manager with a native goal and a background
    command waiting for news did not spin.
    *Checked:* tests `a_new_contract_queues_the_native_goal_with_the_assignment_and_reads_it_back`,
    `a_claude_manager_gets_its_own_goal_while_a_confirmed_goal_runs`,
    `a_running_await_takes_the_wake_past_a_draft_and_prints_it`,
    `the_spin_guard_clears_a_manager_goal_that_continues_with_no_await_once`;
    measured natively (docs/verification.md, 2026-11-16, native goal placement).

## Changing hgm

A change that adds something a reader sees names the principle it serves, and
a prompt change is reviewed against `docs/prompt-checklist.md`. A rule added to
a guide replaces words within its budget. Nothing here changes goals already
recorded: new fields have defaults that keep earlier behaviour readable, and
`--json` keeps its fields.
