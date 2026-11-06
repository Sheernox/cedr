# Run a Cedr Council

Several agents review a change on their own, and `cedr` merges their findings into one gate.

1. **Get the change ready.** Address outstanding comments on its pull request, fix problems you already know about, and run the checks that cover it. Skip fixes to anything being thrown away.
2. **Plan.** Run `cedr council plan review.council --size <small|medium|large> --format json`, or call `council_plan`. It names the members to start, each with a `tag`, a model and a `t3` target, plus `fixes`, `advisory` and `rounds`. Exit 3 means fewer members than the quorum: start them anyway. Exit 1 means none: stop and say so. If the person named members, add `--member <harness>/<model>` for each.
3. **Start every member at once.** Each gets its own delegate and the same context: the whole pull request diff when there is a pull request, otherwise the uncommitted changes or the last commit. Each brief starts with the member's `tag` (and its `prefix` if it has one), asks for a review of this change only and no edits, and asks for one line per finding, `<file>:<line> - <P0|P1|P2|P3> - <claim> - <how it fails>`, ending with `Score: <n>/10`. A member with no findings says what it checked and writes `No findings.`. Use the requested one-line finding format; do not put severity labels on headings or separate lines (for example `P0/P1: none.`). Mention an earlier fix as `<file>:<line> fixed - <reason>`, without a severity label. Members must not see each other's replies.
   - In T3 Code: call `orchestrator_capabilities` once, then `delegate_task` per member with `role` from the plan, `mode: "async"`, the member's `t3` object as `target` and the brief as `task`. Give the person each delegate's `link`. Follow what each task returns instead of polling. A model the short list omits may still exist: check with `search` before calling it absent.
   - Elsewhere: use the member's own CLI or your delegation tool.
   - If a member fails partway, plan again with `--avoid <harness>`: a spare may take its seat. Never replace a member yourself; report it as absent.
4. **Merge.** Save each reply to its own file and run `cedr council merge <member>=<file>... --route review.council --run <run> --round <n> --format json --previous <last merge JSON>` (omit `--previous` on round 1), or call `council_merge`. Save the JSON for the next round.
5. **Act on the gate.**
   - `block` with `next: revise`: if `fixes` is `agent`, fix each blocking group you agree with and rebut the rest in one line each, `<file>:<line> fixed|rebutted - <reason>`. If `fixes` is `ask`, show the brief and wait.
   - `pass` or `block`: findings below the blocking level follow `advisory`: `apply` means apply what you agree with and this change needs, `follow-up` means list them, `ask` means show them and wait.
   - Fix only what this change needs. Anything else goes on a follow-up list, however many reviews raise it.
   - If you changed code and rounds remain, review again from step 2 with the next `--round`, adding your reply lines to each brief. A pass describes the code that ships.
   - `next: done` after a block, or `stalled: true`: stop. The change does not ship until a person decides.
   - `incomplete`: say which members were missing.
6. **Record the scores.** Each full review is one column; a route's `rounds` counts revisions, so it allows `rounds + 1` reviews. Keep this section in the pull request's description, not a comment; with no pull request, put it in your final reply. Write `no score` for a member that gave none, and never invent one.

   ## Council review

   Reviews: 2

   | Member | Review 1 | Review 2 |
   | --- | --- | --- |
   | codex/sol | 7/10 | 9/10 |

For a question or a design rather than a change, ask each member the same question read only, and report where they agree and disagree. Do not merge: there is no gate.
