# Cedr Council: design contract

The frozen contract for `cedr council` and its MCP tools. A change to any shape below lands here first, then in the code. Council reuses Route's config, readings and pick ([route.md](route.md)); it adds a quorum, a report format and the merge that turns several reviews into one gate.

## Scope

`cedr council plan` names the members an orchestrator should start; the orchestrator runs each review itself (as with `pick`, Cedr starts no agent); `cedr council merge` reads their reports, merges findings that point at the same place, and decides the gate. Out of scope: the GitHub Action and App, posting comments, and running reviewers.

## Config

A council is a route with `council = true`. It may set `quorum`, the fewest members whose reports must be merged before the council can pass: default 2, or the number of candidates when there are fewer; and `block`, the least severe finding that blocks: `P0`, `P1` (default), `P2` or `P3`. Validation fails when `quorum` is below 1 or above the number of candidates, or when either is set on a route that is not a council.

```toml
[routes."review.council"]
council = true
quorum = 2
block = "P1"
role = "review"
candidates = [
  { target = "codex/sol", effort = "high" },
  { target = "claude/opus", effort = "high" },
]
```

## Plan

`cedr council plan <route> [--size small|medium|large] [--avoid H]... [--format text|json|t3]` runs Route's pick on a council route (an error for any other route) and records each member's pick. Text and `t3` output are the pick's, followed by `quorum <n>: <eligible> eligible`. JSON is the pick JSON plus `"quorum": n` and `"quorum_reachable": bool`. Exit 0 when the eligible members reach the quorum, 3 when some are eligible but fewer than the quorum (start them anyway; the merge will report the council incomplete), 1 when none are, 2 on error.

## Reports

One report per member, in either form:

- JSON: `{"member": "<harness>/<model>", "pick": "p-...", "findings": [{"file": "src/a.rs", "line": 12, "severity": "P1", "claim": "...", "scenario": "..."}]}`. `pick` and `scenario` are optional; `findings` may be empty (reviewed, nothing found).
- Text: lines of the form `<file>:<line> - <P0|P1|P2|P3> - <claim>` with an optional ` - <scenario>`, as review briefs ask for. Other lines are ignored. The member comes from the argument (`<member>=<path>`).

`P0` is the most severe. A report that cannot be read or parsed is an error, not an empty review. Reports are capped at 256 KiB each.

## Merge

`cedr council merge [--route R] [--quorum N] [--block P0|P1|P2|P3] [--format text|json] <report>...`, where each report is `<path>` (JSON) or `<member>=<path>` (text, or JSON whose own `member` is then overridden). `-` reads one report from stdin. The quorum and blocking severity come from the flags, else the route's `quorum` and `block`, else 2 and P1.

1. Findings are grouped by file. Within a file, sorted by line, a finding joins the previous group when its line is at most 3 after that group's last line; otherwise it starts a new group.
2. A group's severity is the most severe of its findings, its line the lowest, and its members the distinct members that reported in it. Every finding is kept with its member, severity, line, claim and scenario: dissent is shown, never voted away.
3. Reviewers are the distinct members across all reports, findings or not.
4. Gate: `block` when any group is at or above the blocking severity, whoever reported it and however many reviewed; otherwise `pass` when reviewers reach the quorum; otherwise `incomplete`, a neutral result that is never a silent pass.

Exit codes: 0 pass, 1 block, 3 incomplete, 2 error.

Text output starts with `council: <gate> (<reviewers> of quorum <n> reviewed, <block> and above block: <members>)`, then one line per group, most severe first, then by file and line: `<severity> <file>:<line> [<members>] <claim of the most severe finding>`, followed by an indented `<member> <severity> :<line> <claim>` line for every finding when the group holds more than one.

JSON output:

```json
{"gate": "block", "quorum": 2, "block": "P1", "reviewers": ["claude/claude-opus-5-5", "codex/gpt-6.1-sol"],
 "groups": [{"file": "src/a.rs", "line": 12, "severity": "P1", "members": ["codex/gpt-6.1-sol"],
   "findings": [{"member": "codex/gpt-6.1-sol", "severity": "P1", "line": 12, "claim": "...", "scenario": null}]}]}
```

## MCP tools

| Tool | Arguments | Returns |
|---|---|---|
| `council_plan` | `route`, `size?`, `avoid?`, `models?` | The plan JSON |
| `council_merge` | `reports` (array of report JSON objects), `route?`, `quorum?`, `block?` | The merge JSON |

A gate of `block` or `incomplete` is a normal result, not a tool error.
