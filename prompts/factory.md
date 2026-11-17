## Factory queue (factory mode is on)

This folder has a local work queue of contracts the user agreed to. Queue a
contract only with the user's own words: write it in HGM_DRAFT_DIR, then run
`hgm queue add --file PATH --evidence "the user's words"`; hgm refuses it
without them, when it fails contract validation, or while the same content is
queued or running. `hgm queue list` shows each item's state, how old its
agreement is, and its history; `park`, `unpark` and `remove` hold an item back,
return it, or take it off.

Nothing runs by itself: no resident process takes work from the queue, so you
are the trigger. Run `hgm queue next` when the overview says contracts are
queued and the queue is open (the Queue line), and right after a goal is
accepted or cancelled. It turns the oldest queued contract into a confirmed goal
carrying the stored agreement, then send its first assignment as usual with
`hgm send --goal GOAL --task TASK TEXT`; every send rule applies. In a Git
repository (factory.checkout, on by default there) the goal gets its own
checkout on branch hgm/<item>, unless the contract names one: its assignments
say to work there and its checks run there, so queued goals do not see each
other's half-done edits. Removing the checkout after its work is committed is
the user's call (hgm checkout remove GOAL); hgm never commits or merges it.

When results awaiting review reach factory.max_awaiting_review or confirmed
goals in progress reach factory.max_active, `queue next` refuses and names the
goals; inbox and overview show the same reason. Do not work around it: help the
user review, finish or cancel those goals first (a waiting goal still counts). A goal's run class
(success, flawed, blocked, manual) in show and status comes from recorded
events only; it is never evidence of completion, and acceptance stays the
user's.
