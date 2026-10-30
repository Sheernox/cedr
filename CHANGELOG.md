# Changelog

Every release of the `cedr` CLI, newest first. The GitHub release notes for a version are its entry here. The design contracts are [docs/route.md](docs/route.md) (Route and the MCP server) and [docs/council.md](docs/council.md) (Council); the user guide is [cedr.dev/docs/route](https://cedr.dev/docs/route).

## v0.2.0 (2026-10-30)

**v0.2.0** · 4 changes from 1 contributor

### Highlights
Try new model versions on small work before they take everything. Let a cheap model draft and a judge decide whether the draft stands or goes to a stronger model. Give a change the council blocked one round to answer before the gate stands. See how every route and council performs, with time to verdict, trial results and revision outcomes. Update every machine to 0.2.0 before using routes from cedr.dev: 0.1.0 rejects the new route fields.

### Fixes
- A route file without an `[evaluation] last` date no longer warns that it is stale.
- A pick that keeps the work no longer reports a run id that nothing records.

### Features
- **Preview versions.** With `[evaluation] models` set, a model version it doesn't list only takes `--size small` picks (`preview = "trial"`, the default), so it builds a track record before medium and large work moves to it; `hold` keeps it off every pick and `use` always takes the newest. Such picks show ` (preview)` in their reason and get their own row in stats. ([docs](docs/route.md#preview-versions), [guide](https://cedr.dev/docs/route#drafts-judges-and-new-models))
- **Cascades.** A route can name a `judge` route and an `escalate` route. Pick the judge with `cedr route pick <judge> --judges <draft pick>`; it replies `ACCEPT` or `ESCALATE: <reason>`, and an escalation is picked with `--escalates <draft pick>`. All three picks share one run in history. ([docs](docs/route.md#cascade))
- **Council revision rounds.** Councils set `rounds` (0 to 3, default 1). A blocked merge with a round left reports `next: revise` and writes one revision brief with every finding, most severe first and dissent kept (`cedr council merge --format brief`); review again with `--round 2`. ([docs](docs/council.md#critique-and-revise))
- **Council records and stats.** Council plans share one run id; `cedr council merge --run <id>` records the gate, round and finding counts (never findings) and syncs them to cedr.dev. `cedr council stats` reports passes, blocks, revisions and passes after revision. ([docs](docs/council.md#record))
- **Richer route stats and history.** `cedr route stats` adds `--since`, trial rows, median minutes to a verdict, and judged and escalated counts for cascades; `cedr route history` lists recent picks and council results. ([docs](docs/route.md#history-and-verdicts))
- **`cedr route import`.** Converts a T3 Code delegation `routing.toml` into a `routes.toml`, refusing fields it does not know. ([docs](docs/route.md#import))
- **MCP tools.** New `route_stats`, `route_history` and `council_stats`; `route_pick` takes `judges` and `escalates`, and `council_merge` takes `rounds`, `round` and `run`. ([docs](docs/route.md#mcp-tools))

<details>
<summary>Full changelog</summary>

#### What's Changed
* feat(route): hold unevaluated model versions, add judged cascades and council revision rounds
* docs(route): contract linked cascade and council runs, council records, richer stats and import
* feat(route): link cascade and council runs, record council results, add history, richer stats and import
* chore(release): cedr 0.2.0

Changes land in the cedr.dev source tree and arrive here together in one export commit ([`1e6947f`](https://github.com/Sheernox/cedr/commit/1e6947f)), so this release has no pull requests of its own.

**Full Changelog**: https://github.com/Sheernox/cedr/compare/v0.1.0...v0.2.0

</details>

## v0.1.0 (2026-10-29)

**v0.1.0** · first release

### Highlights
Route picks the coding agent, model and effort for each kind of work, balanced by the room left on each plan you pay for. Council turns several vendors' reviews into one gate. Prebuilt binaries for Linux, macOS and Windows.

### Features
- **`cedr route`.** Picks from `routes.toml` with route tags, local history, verdicts, stats and sync with cedr.dev. ([docs](docs/route.md))
- **`cedr council`.** Plans a multi-vendor review and merges the reports into one gate with a quorum and a blocking severity. ([docs](docs/council.md))
- **`cedr mcp`.** The same tools over MCP on stdio. ([docs](docs/route.md#mcp-tools))

<details>
<summary>Full changelog</summary>

**Full Changelog**: https://github.com/Sheernox/cedr/commits/v0.1.0

</details>
