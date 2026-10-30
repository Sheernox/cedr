# Changelog

Every release of the `cedr` CLI, newest first. Each entry covers what changed, how the new features work, and where they are documented. The design contracts are [docs/route.md](docs/route.md) (Route and the MCP server) and [docs/council.md](docs/council.md) (Council); the user guide is [cedr.dev/docs/route](https://cedr.dev/docs/route).

## 0.2.0 (2026-10-30)

Route gets three ways to spend models more carefully (holding back untested model versions, cheap drafts checked by a judge, and council revision rounds), and every decision it makes is now recorded well enough to measure. [Release](https://github.com/Sheernox/cedr/releases/tag/v0.2.0) · [all changes since 0.1.0](https://github.com/Sheernox/cedr/compare/v0.1.0...v0.2.0)

### Highlights

**Preview versions: new model versions are tried on small work first.** Routes name model families (`codex/sol`), and a pick resolves a family to the newest version the provider offers. When `[evaluation] models` lists the versions you have evaluated, a newer version that is not on the list is now a *preview*:

- `preview = "trial"` (the default): only `--size small` picks use it, so it builds a track record on low-risk work while medium and large picks stay on the newest evaluated version.
- `preview = "hold"`: never used while an evaluated version is offered.
- `preview = "use"`: always the newest version (the 0.1.0 behaviour).

A pick on a preview version gets ` (preview)` on its reason and `"preview": true` in JSON and history, and `cedr route stats` gives it its own row so its trial can be judged on its own. Add the version to `models` once its verdicts hold up. A family with no evaluated version offered uses the preview for every size, and an exact model id in a route is never a preview. Docs: [Preview versions](docs/route.md#preview-versions), [Drafts, Judges and New Models](https://cedr.dev/docs/route#drafts-judges-and-new-models).

**Cascades: a cheap model drafts, a judge decides whether it stands.** A route can name a `judge` route and an `escalate` route:

```toml
[routes."code.impl"]
judge = "review.judge"
escalate = "code.hard"   # optional; without it, escalated work comes back to your main agent
```

A pick on that route prints `cascade: judge review.judge, escalate code.hard` (JSON: `"cascade": {"judge", "escalate"}`). The orchestrator starts the draft, then picks the judge with `cedr route pick review.judge --judges <draft pick id>`; the judge replies `ACCEPT` or `ESCALATE: <reason>`. On escalation, `cedr route pick code.hard --escalates <draft pick id>` hands the work on with the judge's reason. The draft, judge and escalation picks share one run id in history, and `cedr route stats` reports how many of a route's drafts were judged and escalated. Validation refuses unknown routes, councils, a judge that is itself a cascade, and escalation loops. Docs: [Cascade](docs/route.md#cascade).

**Council revision rounds: a blocked change gets one structured chance to answer.** A council route can set `rounds` (0 to 3, default 1). When `cedr council merge` blocks and a round is left, it reports `next: revise` and writes a revision brief (`--format brief`, or `brief` in JSON) listing every finding, grouped and most severe first, with dissent kept. The author fixes or rebuts each blocking group, the council reviews again, and the second merge runs with `--round 2`. A change still blocked after the last round does not pass. Docs: [Critique and revise](docs/council.md#critique-and-revise).

**Everything Route and Council decide is recorded and measurable.**

- Council plans give every member pick one run id, and `cedr council merge --run <id>` appends a `council` record to history with the gate, round and finding counts (never files, lines or claims). `--no-record` skips it. Council records sync to cedr.dev with picks and verdicts.
- `cedr council stats` reports merges, passes, blocks, changes revised, how many passed after revision, and blocking findings per merge.
- `cedr route stats` adds `--since YYYY-MM-DD`, a separate row for each preview version, `median_minutes` from pick to verdict, and `judged` and `escalated` counts for cascade routes.
- `cedr route history` lists recent picks (with their latest verdict) and council results, newest first.

Docs: [History and verdicts](docs/route.md#history-and-verdicts), [Record](docs/council.md#record), [Activity](https://cedr.dev/docs/route#activity).

**`cedr route import`** converts a T3 Code delegation `routing.toml` (providers by T3 provider id, `[families]` for model windows) into a `routes.toml`, refusing any field it does not know rather than dropping it. Docs: [Import](docs/route.md#import).

### Other changes

- `[evaluation] last` is optional. Without it no stale-evaluation warning prints, so a file can carry only the evaluated models list.
- A pick that keeps the work reports no run.
- New MCP tools: `route_stats`, `route_history` and `council_stats`. `route_pick` takes `judges` and `escalates`; `council_merge` takes `rounds`, `round` and `run` and records its result. `route_list` includes `judge` and `escalate`.

### Upgrading

- **Machines must run 0.2.0 before they use routes from cedr.dev.** The app now writes `judge`, `escalate`, `rounds` and an `[evaluation]` block, which 0.1.0 rejects as unknown fields.
- **Picks can change once you list evaluated models.** With `[evaluation] models` set, the default `trial` mode holds an unlisted newer version back from medium and large picks. Set `preview = "use"` to keep the 0.1.0 behaviour.
- **Output additions.** Pick JSON gains `preview`, `run` and `cascade`. Stats rows gain `preview`, `median_minutes`, `judged` and `escalated`, and text stats gain matching columns. `council plan` text ends with `, run <id>`, and `council merge` text ends with a `next:` line. Scripts that parse exact text output need updating; JSON consumers only see new keys.

### Changes

No pull requests were merged into this repository in this release. Changes land in the cedr.dev source tree and arrive here together as the release export ([`1e6947f`](https://github.com/Sheernox/cedr/commit/1e6947f)); these are the changes it contains:

- Hold unevaluated model versions, add judged cascades and council revision rounds (contract and code).
- Contract linked cascade and council runs, council records, richer stats and import.
- Link cascade and council runs, record council results, and add `cedr route history`, richer `cedr route stats`, `cedr council stats` and `cedr route import`.
- Release 0.2.0.

## 0.1.0 (2026-10-29)

First public release: [Release](https://github.com/Sheernox/cedr/releases/tag/v0.1.0).

- `cedr route`: picks the agent, model and effort for each kind of work from `routes.toml`, balanced by the room left on each plan, with route tags, local history, verdicts, stats and sync with cedr.dev. Docs: [docs/route.md](docs/route.md).
- `cedr council`: plans a multi-vendor review and merges the reports into one gate with a quorum and a blocking severity. Docs: [docs/council.md](docs/council.md).
- `cedr mcp`: the same tools over MCP on stdio.
- Prebuilt binaries for Linux, macOS and Windows on each release.
