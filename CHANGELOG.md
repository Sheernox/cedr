# Changelog

Every release of the `cedr` CLI, newest first. The GitHub release notes for a version are its entry here. The design contracts are [docs/route.md](docs/route.md) (Route and the MCP server), [docs/council.md](docs/council.md) (Council), [docs/design.md](docs/design.md) (Guard), [docs/sync.md](docs/sync.md) (Sync), [docs/monitor.md](docs/monitor.md) (Monitor) and [docs/doctor.md](docs/doctor.md); the user guides are at [cedr.dev/docs](https://cedr.dev/docs).

## v0.4.2 (2026-10-30)

**v0.4.2** · 4 changes from 1 contributor

### Highlights
Release archives are named by operating system and architecture instead of the Rust target triple: `cedr-linux-x86_64.tar.gz`, `cedr-linux-arm64.tar.gz`, `cedr-macos-arm64.tar.gz`, `cedr-macos-x86_64.tar.gz` and `cedr-windows-x86_64.zip`, each unpacking to a folder of the same name. The GitHub Action downloads the new names; earlier releases keep their old ones. Release notes are now this changelog's entry for each tag.

### Fixes
- Archive names no longer carry the target triple's `unknown`, `apple` and `pc` vendor fields.
- Each GitHub release now publishes its changelog entry instead of a generated one-line compare link.

<details>
<summary>Full changelog</summary>

#### What's Changed
* ci(release): publish each tag's changelog entry as its release notes
* build(release): name archives by os and architecture
* docs: changelog for 0.4.2
* chore(release): cedr 0.4.2

**Full Changelog**: https://github.com/Sheernox/cedr/compare/v0.4.1...v0.4.2
</details>

## v0.4.1 (2026-10-30)

**v0.4.1** · 5 changes from 1 contributor

### Highlights
Fixes from running the GitHub Action, the hourly push timer and Sync end to end on real repos, Linux and Windows. No upgrade step; if you pinned the action, move to `Sheernox/cedr@v0.4.1`.

### Fixes
- `cedr sync` no longer writes settings files that hold only empty lists when a repo has no permissions or hooks, and says once, instead of per entry, when MCP servers, permissions and hooks reach no agent.
- `cedr monitor push` checks its organization and key before reading any log, so a machine with nowhere to send counts says so at once.
- Scheduled pushes start up to ten random minutes late, so machines do not all push on the hour, and Windows no longer leaves the task XML in the home folder.
- `cedr outdated --format github` puts each annotation on `.cedr/guard.lock` instead of the workflow file.

<details>
<summary>Full changelog</summary>

#### What's Changed
* fix(sync): write no empty settings files, and note unused servers once instead of per entry
* fix(monitor): check where a push goes before reading logs, spread scheduled pushes, and tidy the windows task
* fix(guard): put cedr outdated annotations on the lock file instead of the workflow
* docs: changelog for 0.4.1
* chore(release): cedr 0.4.1

**Full Changelog**: https://github.com/Sheernox/cedr/compare/v0.4.0...v0.4.1
</details>

## v0.4.0 (2026-10-30)

**v0.4.0** · 14 changes from 1 contributor

### Highlights
Two new tools and a checkup. **Sync** keeps one agent setup in `.cedr/` and writes every agent's own files from it (AGENTS.md, Claude Code, Codex, Cursor, Gemini CLI, Copilot, opencode and Antigravity), fails CI on drift, and applies your organization's rules. **Monitor** reads each coding agent's local logs and plan readings and reports tokens, spend at API list prices, cache use, sessions, errors and plan limits, and sends counts (never prompts or code) to the app's Monitor page. **`cedr doctor`** checks everything cedr needs and prints the fix for each problem. Guard now reads all eight agents' configs and shows what a newer server version would change before you take it. No upgrade step: existing routes, history and locks are unchanged.

### Fixes
- Monitor's push reads its organization only from your own `~/.config/cedr/monitor.toml` or `routes.toml`, never from a repo's `.cedr/routes.toml`, so a cloned repo cannot choose where your API key goes.
- Claude Code tool calls written without a timestamp are counted in the hour they happened instead of at 1970.

### Features
- **`cedr sync`.** `init` imports a repo's existing agent files into `.cedr/` (replacing literal secrets with `${NAME}` references); `cedr sync` compiles rules, skills, MCP servers, permissions and hooks into each agent's native files, merging settings files instead of replacing them; `check` fails on drift, hand edits and broken org rules (CS001-CS105), with `--format github`; `pull` vendors org rules into `.cedr/org-rules.json`; `push` reports each repo to the app's Sync page. ([docs](docs/sync.md), [guide](https://cedr.dev/docs/sync))
- **`cedr monitor`.** Incremental collectors for Claude Code, Codex, Gemini CLI, Copilot CLI, opencode, Muse Code and Antigravity into a local store; reports by model, agent, project, day, hour, session or effort; `sessions`, `limits` (projection and outlook per plan window), `sources`, `push` (hourly counts to the app) and `schedule` (an hourly user timer on Linux, macOS and Windows). ([docs](docs/monitor.md), [guide](https://cedr.dev/docs/monitor))
- **`cedr doctor`.** Checks the CLI version, API key and organization, Route, MCP registrations, Monitor's collectors and schedule, and Sync and Guard in the current repo, each with its fix; `--format json` for agents. ([docs](docs/doctor.md))
- **`cedr outdated`.** Shows newer versions of pinned npm servers and what each would change (tools, descriptions, schemas, hosts, capabilities) without installing or running anything; `cedr push --updates` sends them to the Guard page. ([docs](docs/design.md#updates))
- **Guard reads every agent.** Copilot, opencode and Antigravity MCP configs and the Gemini, Cursor, Copilot and opencode skill folders are linted, locked and verified like the rest. ([docs](docs/design.md#discovery))
- **GitHub Action.** New inputs `sync`, `sync-fix` (a scheduled pull request that repairs Sync drift) and `outdated`; `push-org` also reports Sync and server updates. The action is now named `cedr`.
- **MCP tools.** `cedr mcp` adds `sync_check`, `sync_plan`, `monitor_report`, `monitor_limits`, `monitor_sources` and `doctor`.

<details>
<summary>Full changelog</summary>

#### What's Changed
* docs: add the sync, monitor and doctor design contracts
* feat(monitor): add the shared api price table
* docs(guard): contract every agent's config files, cedr outdated and update reports
* docs(monitor): state the per-row counter ceiling and the app's per-member caps
* feat(monitor): read every coding agent's local usage into a store, report it and push hourly counts
* fix(monitor): send counts the app accepts, read the org only from the person's own config, and explain every flag
* fix(monitor): place untimed records at the file's last time instead of the epoch
* feat(sync): compile agent setup and diagnose local configuration
* fix(sync): retain invalid-source findings and native import notes
* fix(sync): keep front matter on the first line of generated rules
* docs(sync): state the default targets and when doctor goes online
* feat(guard): discover all clients and report npm surface updates
* docs: changelog for 0.4.0
* chore(release): cedr 0.4.0

**Full Changelog**: https://github.com/Sheernox/cedr/compare/v0.3.0...v0.4.0
</details>

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
