# Cedr Council: design contract

The frozen contract for `cedr council` and its MCP tools. A change to any shape below lands here first, then in the code. Council reuses Route's config, readings and pick ([route.md](route.md)); it adds a quorum, a report format and the merge that turns several reviews into one gate.

## Scope

`cedr council plan` names the members an orchestrator should start; the orchestrator runs each review itself (as with `pick`, Cedr starts no agent); `cedr council merge` reads their reports, merges findings that point at the same place, decides the gate, and, when the gate blocks with a revision round left, writes one revision brief for the change's author (critique and revise). Out of scope: the GitHub Action and App, posting comments, and running reviewers.

## Config

A council is a route with `council = true`. It may set `quorum`, the fewest members whose reports must be merged before the council can pass: default 2, or the number of candidates when there are fewer; `block`, the least severe finding that blocks: `P0`, `P1` (default), `P2` or `P3`; and `rounds`, how many times a blocked change goes back to its author for revision before the gate stands: 0 to 3, default 1. Validation fails when `quorum` is below 1 or above the number of candidates, when `rounds` is outside 0-3, or when any of them is set on a route that is not a council.

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

## Plan

`cedr council plan <route> [--size small|medium|large] [--avoid H]... [--format text|json|t3]` runs Route's pick on a council route (an error for any other route) and records each member's pick. Every member pick shares one new run id (route.md, History). Text and `t3` output are the pick's, followed by `quorum <n>: <eligible> eligible, run <run>`. JSON is the pick JSON plus `"quorum": n`, `"quorum_reachable": bool` and `"run": "r-..."`. Exit 0 when the eligible members reach the quorum, 3 when some are eligible but fewer than the quorum (start them anyway; the merge will report the council incomplete), 1 when none are, 2 on error.

## Reports

One report per member, in either form:

- JSON: `{"member": "<harness>/<model>", "pick": "p-...", "findings": [{"file": "src/a.rs", "line": 12, "severity": "P1", "claim": "...", "scenario": "..."}]}`. `pick` and `scenario` are optional; `findings` may be empty (reviewed, nothing found).
- Text: lines of the form `<file>:<line> - <P0|P1|P2|P3> - <claim>` with an optional ` - <scenario>`, as review briefs ask for. Other lines are ignored. The member comes from the argument (`<member>=<path>`).

`P0` is the most severe. A report that cannot be read or parsed is an error, not an empty review. Reports are capped at 256 KiB each.

## Merge

`cedr council merge [--route R] [--run RUN] [--quorum N] [--block P0|P1|P2|P3] [--rounds N] [--round N] [--no-record] [--format text|json|brief] <report>...`, where each report is `<path>` (JSON) or `<member>=<path>` (text, or JSON whose own `member` is then overridden). `-` reads one report from stdin. The quorum, blocking severity and rounds come from the flags, else the route's `quorum`, `block` and `rounds`, else 2, P1 and 1. `--round` is the review being merged: 1 (the default) for the first, 2 after one revision, and so on.

1. Findings are grouped by file. Within a file, sorted by line, a finding joins the previous group when its line is at most 3 after that group's last line; otherwise it starts a new group.
2. A group's severity is the most severe of its findings, its line the lowest, and its members the distinct members that reported in it. Every finding is kept with its member, severity, line, claim and scenario: dissent is shown, never voted away.
3. Reviewers are the distinct members across all reports, findings or not.
4. Gate: `block` when any group is at or above the blocking severity, whoever reported it and however many reviewed; otherwise `pass` when reviewers reach the quorum; otherwise `incomplete`, a neutral result that is never a silent pass.

5. Next: `revise` when the gate is `block` and `round` is at most `rounds`; otherwise `done`.

Exit codes: 0 pass, 1 block, 3 incomplete, 2 error.

Text output starts with `council: <gate> (<reviewers> of quorum <n> reviewed, <block> and above block: <members>)`, then one line per group, most severe first, then by file and line: `<severity> <file>:<line> [<members>] <claim of the most severe finding>`, followed by an indented `<member> <severity> :<line> <claim>` line for every finding when the group holds more than one. It ends with `next: revise (round <round> of <rounds>): give the author the revision brief (--format brief), then review again with --round <round + 1>` or `next: done`.

JSON output:

```json
{"gate": "block", "quorum": 2, "block": "P1", "round": 1, "rounds": 1, "next": "revise", "brief": "...",
 "reviewers": ["claude/claude-opus-5-5", "codex/gpt-6.1-sol"],
 "groups": [{"file": "src/a.rs", "line": 12, "severity": "P1", "members": ["codex/gpt-6.1-sol"],
   "findings": [{"member": "codex/gpt-6.1-sol", "severity": "P1", "line": 12, "claim": "...", "scenario": null}]}]}
```

`brief` is present only when `next` is `revise`.

## Record

Unless `--no-record`, every merge that produces a gate appends a `council` record to Route's history (route.md, History) and prints nothing extra; `--run` names the plan's run (the `run` of `council plan`), and a run not in this machine's history is an error (exit 2). The merge JSON gains `"id": "c-..."` when recorded, null otherwise. Council records sync with picks and verdicts.

`cedr council stats [--route R] [--since YYYY-MM-DD] [--format text|json]` summarises recorded merges per route: `{"route", "merges", "pass", "block", "incomplete", "revised", "passed_after_revision", "blocking_findings_per_merge"}`, where `revised` counts runs with a round 2 or later, `passed_after_revision` those whose last merge passed, and `blocking_findings_per_merge` the mean number of groups at or above each merge's `block`, rounded to 0.1.

## Critique and revise

When `next` is `revise`, the orchestrator gives the change's author (the delegate that wrote it, or itself) the revision brief, the Markdown that `--format brief` prints and JSON carries as `brief`:

```markdown
# Council revision brief (round 1 of 1)

Blocking: P1 and above. Fix each blocking group or rebut it with evidence; the others are advisory.
Reply with one line per group: `<file>:<line> fixed|rebutted - <reason>`.

## P1 src/a.rs:12 (codex/gpt-6.1-sol, claude/claude-opus-5-5)
- codex/gpt-6.1-sol P1 :12 <claim> - <scenario>
- claude/claude-opus-5-5 P2 :14 <claim>
```

Groups are in the merge's order (most severe first, then file and line); every finding is listed, so dissent reaches the author. The `--format brief` exit code is the gate's. After the revision the orchestrator plans the council again (fresh reviews; each member's brief adds the author's reply lines) and merges with the next `--round`. A gate that still blocks once `round` exceeds `rounds` stands: `next` is `done` and the change does not ship.

## MCP tools

| Tool | Arguments | Returns |
|---|---|---|
| `council_plan` | `route`, `size?`, `avoid?`, `models?` | The plan JSON |
| `council_merge` | `reports` (array of report JSON objects), `route?`, `run?`, `quorum?`, `block?`, `rounds?`, `round?` | The merge JSON (recorded) |
| `council_stats` | `route?`, `since?` | Council stats rows |

A gate of `block` or `incomplete` is a normal result, not a tool error.
