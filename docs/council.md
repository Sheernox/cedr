# Cedr Council: design contract

The frozen contract for `cedr council` and its MCP tools. A change to any shape below lands here first, then in the code. Council reuses Route's config, readings and pick ([route.md](route.md)); it adds a quorum, a report format and the merge that turns several reviews into one gate.

## Scope

`cedr council plan` names the members an orchestrator should start; the orchestrator runs each review itself (as with `pick`, Cedr starts no agent); `cedr council merge` reads their reports, merges findings that point at the same place, decides the gate, and, when the gate blocks with a revision round left, writes one revision brief for the change's author (critique and revise). Out of scope: the GitHub Action and App, posting comments, and running reviewers.

## Config

A council is a route with `council = true`. It may set `quorum`, the fewest members whose reports must be merged before the council can pass: default 2, or the number of regular members when there are fewer; `block`, the least severe finding that blocks: `P0`, `P1` (default), `P2` or `P3`; and `rounds`, how many times a blocked change goes back to its author for revision before the gate stands: 0 to 3 or `"unlimited"`, default 1. `rounds` counts revisions, not reviews: the first review plus `rounds` revisions makes `rounds + 1` reviews at most. Validation fails when a council's candidates are all spares, when `quorum` is below 1 or above the number of regular members, when `rounds` is outside 0-3 or `"unlimited"`, or when any of them is set on a route that is not a council.

Like any route, a council may set its own `thresholds` and `harness_thresholds` (route.md, Thresholds), which decide which members are eligible, and a member may set `sizes`, the work sizes it reviews, so large changes can get more reviewers than small ones. `sizes` must leave at least the quorum of members for every size; validation fails otherwise, since a council with too few members could never pass that work.

```toml
[routes."review.council"]
council = true
quorum = 2
block = "P1"
rounds = 1
role = "review"
candidates = [
  { target = "codex/sol", effort = "high" },
  { target = "claude/opus", effort = "high" },
]
```

## Council settings

- `rounds`: 0 to 3, or the string `"unlimited"`. Default stays 1 for hand-written files.
- `fixes = "agent" | "ask"`, default `"agent"`: who acts on blocking findings.
- `advisory = "apply" | "follow-up" | "ask"`, default `"apply"`: what happens to findings below the blocking level.
- `spares = "all" | "quorum"`, default `"all"`: how many spares step in.
- A candidate may set `spare = true`. A spare on a non-council route, or on `self`, is a validation error.
- Spares do not count towards `quorum` validation (quorum must be within the regular members) or the `sizes` shortfall check. The default quorum is over regular members.

## Plan

`cedr council plan <route> [--size small|medium|large] [--avoid H]... [--member H/M]... [--no-record] [--format text|json|t3]` runs Route's pick on a council route (an error for any other route) and records each member's pick. Every member pick shares one new run id (route.md, History). `--no-record` records nothing, for a question that will not be merged. Text and `t3` output are the pick's, followed by `quorum <n>: <eligible> eligible, run <run>`. JSON is the pick JSON plus `"quorum": n`, `"quorum_reachable": bool` and `"run": "r-..."`. Exit 0 when the eligible members reach the quorum, 3 when some are eligible but fewer than the quorum (start them anyway; the merge will report the council incomplete), 1 when none are, 2 on error.

A spare stays out unless a regular member cannot take this review. A vacancy is a regular member that takes this size but is avoided, unresolved, or whose plan state does not take this size (including absent, not installed, conserve for this size, out). A member whose `sizes` leave out this size is not a vacancy: it chose not to review.
- `spares = "all"`: one spare per vacancy. `spares = "quorum"`: only enough to bring eligible members up to the quorum.
- Order: as listed, preferring a harness (vendor) not already among the delegates. A spare must itself take this size and state; otherwise try the next. Pair each spare with a vacancy in vacancy order.
- JSON: each delegate gains `"spare_for": "<harness>/<family or model of the vacant member>"` when it is a spare (omit otherwise). The plan gains `"fixes"`, `"advisory"`, `"rounds"` (number, or null for unlimited).
- Text and t3 output: a line `  spare for <member>` after a spare's delegate line.
- `--member <harness>/<model>` (repeatable; MCP `members?`): plan exactly these. A member in the candidates keeps its effort; otherwise no effort. An unknown harness is a usage error (exit 2). One that is unavailable goes in `skipped` and is never replaced: no spares run with `--member`. Same exit codes and quorum as a normal plan.
- Warning (in the plan's `warnings`, never an error, exit code unchanged): when one vendor alone could reach the quorum among the delegates. A vendor is the harness id with any `-api` suffix removed. Text: `quorum <n> can be reached by <vendor> alone; different vendors miss different things`.

## Reports

One report per member, in either form:

- JSON: `{"member": "<harness>/<model>", "pick": "p-...", "score": 8, "findings": [{"file": "src/a.rs", "line": 12, "severity": "P1", "claim": "...", "scenario": "..."}]}`. `pick`, `score` and `scenario` are optional; `findings` may be empty (reviewed, nothing found).
- Text: lines of the form `<file>:<line> - <P0|P1|P2|P3> - <claim>` with an optional ` - <scenario>`, as review briefs ask for. A line `Score: <n>/10` sets the score; the last such line counts. Other lines are ignored. The member comes from the argument (`<member>=<path>`).

`P0` is the most severe. A score is the member's own view of the change, from 0 to 10, and is shown but never decides the gate; a JSON score off that scale is an error. A report that cannot be read or parsed is an error, not an empty review. Reports are capped at 256 KiB each.

## Merge

`cedr council merge [--route R] [--run RUN] [--quorum N] [--block P0|P1|P2|P3] [--rounds N] [--round N] [--previous FILE] [--no-record] [--format text|json|brief] <report>...`, where each report is `<path>` (JSON) or `<member>=<path>` (text, or JSON whose own `member` is then overridden). `-` reads one report from stdin. The quorum, blocking severity and rounds come from the flags, else the route's `quorum`, `block` and `rounds`, else 2, P1 and 1. `--round` is the review being merged: 1 (the default) for the first, 2 after one revision, and so on, up to `rounds + 1`. With numbered rounds, a later `--round` is a usage error (exit 2) and nothing is recorded, because the gate already stood; a new review of the change starts a new run.

1. Findings are grouped by file. Within a file, sorted by line, a finding joins the previous group when its line is at most 3 after that group's last line; otherwise it starts a new group.
2. A group's severity is the most severe of its findings, its line the lowest, and its members the distinct members that reported in it. Every finding is kept with its member, severity, line, claim and scenario: dissent is shown, never voted away.
3. Reviewers are the distinct members across all reports, findings or not.
4. Gate: `block` when any group is at or above the blocking severity, whoever reported it and however many reviewed; otherwise `pass` when reviewers reach the quorum; otherwise `incomplete`, a neutral result that is never a silent pass.

5. Next: `revise` when the gate blocks, is not stalled and rounds remain (or are unlimited); otherwise `done`.

Exit codes: 0 pass, 1 block, 3 incomplete, 2 error.

Text output starts with `council: <gate> (<reviewers> of quorum <n> reviewed, <block> and above block: <members>)`, then, when any member gave a score, `scores: <member> <n>/10, <member> no score`, then one line per group, most severe first, then by file and line: `<severity> <file>:<line> [<members>] <claim of the most severe finding>`, followed by an indented `<member> <severity> :<line> <claim>` line for every finding when the group holds more than one. It ends with `next: revise (revision <round> of <rounds>): give the author the revision brief (--format brief), then review again with --round <round + 1>` or `next: done`.

JSON output:

```json
{"gate": "block", "quorum": 2, "block": "P1", "round": 1, "rounds": 1, "next": "revise", "stalled": false, "spares": {}, "brief": "...",
 "reviewers": ["claude/claude-opus-5-5", "codex/gpt-6.1-sol"],
 "scores": {"claude/claude-opus-5-5": 8, "codex/gpt-6.1-sol": null},
 "groups": [{"file": "src/a.rs", "line": 12, "severity": "P1", "members": ["codex/gpt-6.1-sol"],
   "findings": [{"member": "codex/gpt-6.1-sol", "severity": "P1", "line": 12, "claim": "...", "scenario": null}]}]}
```

`brief` is present only when `next` is `revise`. `scores` has every reviewer, null when it gave none.

- The last line that looks like a score is the only candidate: an off-scale last line gives no score, never an earlier one. Accept `Score: 7/10`, `**Score:** 7/10`, `## Score: 7/10`, `> Score: 7/10`, `Score: 7/10 (solid)`, `Score: **7**/10`, `Overall score: 7/10`, `| Score | 7/10 |`, `SCORE 7 / 10.`. Reject `score: 8/10 once the tests pass`, `score/a.rs:1 - P1 - x`, `score.rs:12 - P1 - 3/10 of the time`, and a list item `- Score: 7/10` only when it is not the last non-empty line of the reply (a quoted example). Normalize -0 to 0.
- `cedr council merge --previous <file>` (MCP `previous`): the JSON output of the previous round's merge. Stall: the gate blocks, the previous gate blocked, and every blocking group now matches a blocking group then (same file, lines at most 10 apart). A stalled merge has `next: done` and `"stalled": true` (false otherwise; always present in JSON), text `next: done (stalled: the same blocking findings returned, so this goes to a person)`, no brief.
- With `rounds = "unlimited"`, a merge at round 2 or later without `--previous` is a usage error (exit 2), because nothing could ever stop it. Numbered rounds without `--previous` just skip the check.
- Unlimited: `rounds` in merge JSON and in the history record is null, `next` is `revise` while blocked and not stalled, text says `revision <n> of unlimited`, the brief title says `(revision <n> of unlimited)`.
- JSON report gains optional `spare_for`. Merge JSON gains `"spares": {"<spare member>": "<member it stood in for>"}`: from report fields, plus, with `--run`, from that run's picks in local history (the pick record gains optional `spare_for`).
- Council history records carry optional `scores` (non-null only), `spares`, and `stalled` (written only when true), and sync them. Numbers and member names only: never files, lines or claims. `rounds` becomes nullable.

## Procedure

`cedr council skill` prints the Council procedure as plain Markdown (exit 0, text only). It describes independent reviews, acting on `fixes` and `advisory`, saving the previous merge and recording member scores.

## Record

Unless `--no-record`, every merge that produces a gate appends a `council` record to Route's history (route.md, History) and prints nothing extra; `--run` names the plan's run (the `run` of `council plan`), and a run not in this machine's history is an error (exit 2). The merge JSON gains `"id": "c-..."` when recorded, null otherwise. Council records sync with picks and verdicts.

`cedr council stats [--route R] [--since YYYY-MM-DD] [--format text|json]` summarises recorded merges per route: `{"route", "merges", "pass", "block", "incomplete", "revised", "passed_after_revision", "blocking_findings_per_merge"}`, where `merges`, `pass`, `block` and `incomplete` count merges, `revised` counts runs (not revisions) with a round 2 or later, `passed_after_revision` those runs whose last merge passed, and `blocking_findings_per_merge` the mean number of groups at or above each merge's `block`, rounded to 0.1. Text prints one line per route with the same counts, labelling the run counts `revised_runs` and `passed_after_revision_runs`.

## Critique and revise

When `next` is `revise`, the orchestrator gives the change's author (the delegate that wrote it, or itself) the revision brief, the Markdown that `--format brief` prints and JSON carries as `brief`:

```markdown
# Council revision brief (revision 1 of 1)

Blocking: P1 and above. Fix each blocking group or rebut it with evidence; the others are advisory.
Fix only what this change needs; anything else goes on a follow-up list, not into this revision.
Reply with one line per group: `<file>:<line> fixed|rebutted - <reason>`.

## P1 src/a.rs:12 (codex/gpt-6.1-sol, claude/claude-opus-5-5)
- codex/gpt-6.1-sol P1 :12 <claim> - <scenario>
- claude/claude-opus-5-5 P2 :14 <claim>
```

Groups are in the merge's order (most severe first, then file and line); every finding is listed, so dissent reaches the author. The `--format brief` exit code is the gate's. After the revision the orchestrator plans the council again (fresh reviews; each member's brief adds the author's reply lines) and merges with the next `--round`. For numbered rounds, a gate that still blocks once `round` exceeds `rounds` stands: `next` is `done` and the change does not ship.

## MCP tools

| Tool | Arguments | Returns |
|---|---|---|
| `council_plan` | `route`, `size?`, `avoid?`, `models?`, `members?` | The plan JSON |
| `council_merge` | `reports` (array of report JSON objects), `route?`, `run?`, `quorum?`, `block?`, `rounds?`, `round?`, `previous?` | The merge JSON (recorded) |
| `council_stats` | `route?`, `since?` | Council stats rows |

A gate of `block` or `incomplete` is a normal result, not a tool error.
