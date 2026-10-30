# Changelog

Every release of the `cedr` CLI, newest first. The GitHub release notes for a version are its entry here. The design contracts are [docs/route.md](docs/route.md) (Route and the MCP server), [docs/council.md](docs/council.md) (Council) and [docs/design.md](docs/design.md) (Guard); the user guides are [cedr.dev/docs/route](https://cedr.dev/docs/route) and [cedr.dev/docs/guard](https://cedr.dev/docs/guard).

## v0.3.0 (2026-10-30)

**v0.3.0** · 9 changes from 1 contributor

### Highlights
Guard is back, in preview. Check the skills and MCP servers your agents load on every pull request with the new GitHub Action, keep all of Guard's files in `.cedr/`, tune lint per repo with `.cedr/guard.toml`, and send each repo's pins and findings to the app's new Guard page with `cedr push`. Upgrade step: run `cedr lock` once in each locked repo; it moves `cedr.lock` to `.cedr/guard.lock` (0.3.0 still reads the old path). The hosted MCP server moved to `https://mcp.cedr.dev/mcp`.

### Fixes
- Findings, registry errors and config parse errors no longer show a URL's user info, query or fragment, and parse errors no longer quote the offending line.
- A skill folder that links outside the repo is reported as CG010 and skipped, instead of stopping the whole run.
- `cedr lock` writes through a temporary file and never writes through a link that leaves the repo.
- Piping output into a command that exits early (`cedr route history | head`) ends quietly instead of panicking.
- Every flag has help text, and a missing `routes.toml` points at a starter file.

### Features
- **GitHub Action.** `uses: Sheernox/cedr@v0.3.0` installs a released `cedr` checked against `SHA256SUMS`, then runs `cedr lint` and `cedr verify` with findings as pull request annotations. Inputs: `version`, `path`, `strict`, `verify` and `push-org`. ([docs](docs/design.md#github-action), [guide](https://cedr.dev/docs/guard#check-every-pull-request))
- **`--format github`.** `cedr lint` and `cedr verify` print GitHub Actions workflow commands with a summary line. ([docs](docs/design.md#lint-rules))
- **`.cedr/` repo files and `.cedr/guard.toml`.** The lock lives at `.cedr/guard.lock` next to `.cedr/surfaces/`. `guard.toml` adds skill folders and skips lint rules everywhere or under some paths; `verify` is never suppressed. ([docs](docs/design.md#repo-files))
- **`cedr push`.** Sends a repo's pinned skills and servers, its lint findings and its drift from the lock to your cedr.dev organization's Guard page. It never sends file contents, env values, launch commands or URL queries; in CI, `--org` alone never reads the checkout's config. A list cut to the app's limits prints a warning. ([docs](docs/design.md#push))
- **Old text in `cedr diff`.** Description and schema changes carry `before`, and text output prints it on a `was:` line. ([docs](docs/design.md#diff))
- **`cedr completions <shell>`.** Completion scripts for bash, zsh, fish, PowerShell and Elvish, plus examples in `cedr --help`.

<details>
<summary>Full changelog</summary>

#### What's Changed
* docs(guard): contract the .cedr/ repo files, guard.toml, CG010, github annotations, cedr push and the action
* feat(guard): keep guard files in .cedr/, add guard.toml, CG010, github annotations, cedr push and the action
* docs(guard): describe the .cedr/ lock, cedr push and the github action in the readmes
* fix(guard): redact url secrets in findings, keep lock writes inside the repo, and stop ci config redirecting the push key
* fix(cli): keep url secrets out of registry and toml errors, explain every flag, and end quietly on a closed pipe
* fix(guard): match the app's 2 MiB push limit and record its per-org storage cap
* feat(guard): warn when cedr push cuts a list to the app's limits
* docs: changelog for 0.3.0
* chore(release): cedr 0.3.0

Changes land in the cedr.dev source tree and arrive here together in one export commit, so this release has no pull requests of its own.

**Full Changelog**: https://github.com/Sheernox/cedr/compare/v0.2.0...v0.3.0

</details>

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
