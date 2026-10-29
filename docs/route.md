# Cedr Route: design contract

The frozen contract for `cedr route` and the MCP server. A change to any shape below lands here first, then in the code.

## Scope

Route config, readings sources (including built-in readers for Claude Code and Codex), machine checks, pool states, the pick, route tags, `--format t3`, local history, verdicts, stats, sync with the cedr.dev app (history, shared routes and the machine snapshot), and the MCP tools. Council has its own contract: [council.md](council.md).

Out of scope: `run` mode (starting vendor CLIs), hook templates, and the GitHub App.

## Layout

| Path | What |
|---|---|
| `crates/cedr-route/` | Library: config, readings, pool, pick, tag, history |
| `crates/cedr/` | The `cedr` binary: `cedr route ...` and `cedr mcp` over `cedr-route` |
| `fixtures/route/` | Configs and readings for tests |

## Config: `routes.toml`

Found in this order: `--config PATH`, `$CEDR_ROUTES`, the nearest `.cedr/routes.toml` from the working directory upwards, then `$XDG_CONFIG_HOME/cedr/routes.toml` (default `~/.config/cedr/routes.toml`). Routing is per person (it balances their plans), so the user file is the usual home; a repo file overrides it for that repo.

```toml
version = 1

[evaluation]              # optional: when the routes were last re-checked
last = "2026-10-25"
stale_days = 35           # default 35; older evaluations print a warning
models = ["codex/gpt-6.1-sol"]

[thresholds]              # optional; defaults shown
fresh_seconds = 600       # oldest reading treated as live
session_conserve = 85
weekly_conserve = 85
out = 90

[orchestrator]            # optional: the agent that delegates; its pool is shown in every tag
harness = "claude"
model = "opus"

[harnesses.claude]        # one per plan the user pays for; the id is used in targets
expect = ["five_hour", "seven_day"]   # window ids that must be in its reading
effort_option = "effort"  # option id that carries effort in the T3 target; omit when effort is not an option
t3 = "claudeAgent"        # T3 providerInstanceId; default: the harness id
aliases = ["claudeAgent"] # other names a readings feed may use for this harness
brief_prefix = ""         # line a brief must start with after the tag
models = []               # model ids to resolve against when no readings source lists any
billing = "plan"          # plan (limit windows) | api (a key billed per token: no windows, always ready)
command = "claude"        # optional: the program this harness runs; absent on a machine without it (see Machine checks)
reader = "claude-statusline"  # optional: the built-in reader its plan usage should come from (see Machine checks)

[model_windows.seven_day_fable]       # a window that applies only to some models
harness = "claude"
model = "fable"           # applies to models whose id contains this; expected for them

[[readings]]              # zero or more sources, merged in order
kind = "http"             # http | file | command | codex | muse | claude-statusline
url = "https://example.invalid/api/limits"   # http
# path = "readings.json"  # file (relative to the config file)
# command = ["my-limits", "--json"]          # command (argv, no shell)
# harness = "codex"       # codex, muse and claude-statusline: the harness the reading is for
cache_seconds = 60        # http, command, codex and muse; default 60

[sync]                    # optional: sync with the cedr.dev app (see Sync)
org = "acme"              # organization slug
routes = "local"          # local (default) | org: route with the org's shared routes
auto = true               # default true: sync in the background after picks, verdicts and status line captures
# url = "https://api.cedr.dev/v1"

[machine]                 # optional, this machine only
skip = ["antigravity"]    # harnesses this machine's person does not have, even when the routes name them
# name = "work-laptop"    # how the app names this machine; default the host name up to its first dot

[routes."review.code"]
work = "Independent correctness and security review"
enabled = true            # default true; false always keeps the work
role = "review"           # default "general"; passed through to the orchestrator
council = false           # true: every eligible candidate, not one
candidates = [
  { target = "codex/sol", effort = "high", tier = 1 },
  { target = "claude/sonnet", effort = "high", tier = 1 },
  { target = "self" },
]
```

- A target is `self` (the orchestrator keeps the work) or `<harness>/<model>`, where the harness is defined and the model is a family name (`sol`) or an exact id. `self` ends the walk, so it takes no `tier`. Validation fails on anything else, on an empty candidate list, and on an unknown `version`.
- Consecutive candidates with the same `tier` form one tier; a candidate without `tier` stands alone.
- The rules hash is the first 7 hex characters of SHA-256 over the file bytes.
- `command` is a program name (no `/` or `\`, no arguments) or, in a machine's own file only, an absolute path; `reader` is `codex`, `muse` or `claude-statusline`. Both are allowed in shared routes because neither runs anything: Cedr only looks the program up and compares the hint with the local readings sources. Shared routes refuse a path, which would tell whoever edits them which files exist on members' machines. `[machine] skip` names defined harnesses or none (an unknown name is a warning, not an error, so an org dropping a harness never breaks a machine).

## Readings

A JSON document from each source:

```json
{
  "harnesses": [
    {"harness": "codex", "fetchedAt": "2026-10-29T01:37:14Z", "error": null,
     "windows": [{"id": "primary", "kind": "weekly", "percent": 4.0, "resetsAt": "2026-11-04T15:00:04Z"}]}
  ],
  "models": {"codex": ["gpt-6.1-sol", "gpt-6-luna"]}
}
```

- `providers` is accepted for `harnesses` and `provider` for `harness`, so feeds that name the plan's provider work unchanged. A name matches a harness by id or alias.
- A window id may carry an `@account` suffix; matching uses the part before `@`. `kind` is `session` or `weekly`.
- Sources merge in order: the first entry for a harness wins; model lists append without duplicates.
- `http` and `command` replies are cached in `$XDG_CACHE_HOME/cedr/readings-<hash>.json` for `cache_seconds`. A failed fetch prints a warning to stderr and uses the last cache, whose readings then age into `unknown`; with no cache the harness has no reading. Replies over 256 KiB are refused. A command has 10 seconds to exit and close its output (a descendant holding stdout open counts against it). HTTP is HTTPS only, redirects included, with certificate checks and a 5-second timeout. Failure messages name a URL by scheme and host and a command by its program name only, since paths, queries and arguments may carry tokens.
- `--readings FILE` (or `-` for stdin) replaces the configured sources and must be a readings object within the 256 KiB cap; `--models FILE` replaces the model lists with a `{"<harness>": ["<id>", ...]}` object (an orchestrator's own list).

### Built-in readers

Both read through the vendor's own CLI or the data it hands to its user's scripts; neither reads credentials or calls a vendor API itself.

- `codex`: runs `codex app-server` (from `PATH`), sends `initialize`, then `account/rateLimits/read` and `model/list`, and closes it within the 10-second command deadline. Each window of `rateLimits` (`primary`, `secondary`) becomes a window with that id: `percent` from `usedPercent`, `resetsAt` from the epoch seconds, `kind` session when `windowDurationMins` is 300 or less, else weekly. Visible model ids from `model/list` become the harness's model list. Cached like `command`.
- `muse`: runs `muse usage --json` with `MUSE_NO_AUTO_UPDATE=1` (so a probe never replaces the installed CLI). Its undocumented `schemaVersion` 1 reply gives `allowance.window` (session) and `allowance.weekly` with `usedPercent` and `resetsAtMs`; no allowance yet (before a window's first turn) or another format is an error, so the plan reads as unknown, never as unused. Cached like `command`.
- Antigravity has no built-in reader: its plan quota is only available through its OAuth credentials, which Cedr never reads. Without a readings source its plan is unknown, which still takes small and medium work.
- `claude-statusline`: reads the file `cedr route statusline` last wrote (`$XDG_STATE_HOME/cedr/readings/<harness>.json`). Claude Code passes `rate_limits.five_hour` and `rate_limits.seven_day` (`used_percentage`, `resets_at` in epoch seconds) to its status line command; `cedr route statusline` turns them into windows `five_hour` (session) and `seven_day` (weekly) with `fetchedAt` set to the time of capture, so the reading goes stale ten minutes after the last active Claude Code session. A missing file is no reading. Claude Code sends `rate_limits` only to Pro and Max subscribers after a session's first response.

## Machine checks

An org's shared routes assume every member has the same plans; each machine checks that for itself, every time it builds pools (`pick`, `pools`, `council plan`, the sync snapshot):

- **Absent**: a harness listed in `[machine] skip`, or with a `command` that is not an executable file (an absolute path) or not found in any `PATH` directory, is `absent` on this machine, whatever its readings say. Summary `<harness> skipped on this machine` or `<harness> not installed (<command>)`. Nothing is run to check.
- **Stale machine file**: a harness with `billing = "plan"` and a `reader` hint, not absent, is stale when the local file has no `[[readings]]` source of that kind for it and the merged readings have no entry for it (so an `http`, `file` or `command` feed that covers it is enough). It means the routes gained a plan after this machine was set up. Each stale harness adds the warning `this machine has no <reader> usage reader for <harness>, which the routes expect: rerun "Set up this machine" in the app, or add [[readings]] kind = "<reader>", harness = "<harness>"` to stderr and to a pick's `warnings`. The harness keeps routing as `unknown`; the warning is the only change.

## Pool state

For a harness and model, over every window that applies to the model (shared windows plus `model_windows` whose `model` is in the id):

| State | Condition |
|---|---|
| absent | the machine check above; nothing else is read |
| unknown | no reading, a reading with an error, `fetchedAt` older than `fresh_seconds`, an expected window missing, or a window without a percent or past its reset |
| out | any window at `out` or more |
| conserve | any window at its conserve threshold or more (session windows use `session_conserve`) |
| ready | otherwise |

A harness with `billing = "api"` has no windows: it is always ready, with the summary `<harness> api key` and no use.

The worst state wins in the order ready < unknown < conserve < out; absent overrides all of them. A pool's use is the highest percent across its usable windows, and none when the state is unknown. The summary is `<harness> s5% w33%` (`s` session, `w` weekly, the `model_windows` model name for model windows, `?` for an unusable window), plus `missing <ids>`, or `<harness> no reading`, `probe error`, `stale`.

## Model resolution

An id the readings list for the harness is kept as named. Otherwise the name is a family: model ids split on `-` into words and version numbers (`gpt-6.1-sol` is words `gpt sol`, version 6.1); the family matches ids whose words end with the name's words, and the newest version wins. No list, no match, or a name matching more than one family resolves to nothing (`unresolved`).

## Pick

`pick(route, size, avoid)` walks the tiers in order:

1. A disabled route keeps the work (`disabled`). A `self` tier ends the walk.
2. Each candidate: skipped as `<harness>=avoided` when avoided, `=unresolved` when its model does not resolve, `=<state>` when not eligible. Eligible: ready always; conserve only for `small`; unknown for anything but `large`; out and absent never.
3. Council routes take every eligible candidate of every tier, with reason `council`.
4. Otherwise the tier's candidates are ranked: ready plans, then API keys (so plan headroom is used before per-token spend), then plans whose state is unknown, then conserving ones; within each, the lowest use wins (unknown use last). Reason: `fallback:<skipped,...>` when anything was skipped so far; `balanced:<winner use>` plus `<<other>=<use>,...` when the tier has more than one candidate; else `primary`. Use shows as `N%`, `?` when unknown, or `key` for an API key.
5. No delegate: reason `no eligible delegate: <skipped>` or `orchestrator-only route`.

`manual(<harness>/<model>, effort)` tags a model the user named: a known id is kept, a family resolves to its newest version, an unknown name with no model list is kept as named, and an unknown name with a model list is an error. Reason `user-named`.

## Route tag

```
[route review.code -> codex/gpt-6.1-sol high | primary | codex w4%; claude s5% w33% | rules 8ff065e | pick p-1a2b3c4d5e6f]
```

Route, target and effort, reason, the delegate's pool summary, the orchestrator's pool summary (omitted with its `; ` when no orchestrator is set), the rules hash, and the pick id. Each delegate gets its own pick id: `p-` and 12 lowercase hex characters.

## Commands

All take `--config PATH`; picks also take `--readings`, `--models`, `--no-record`. Exit codes: 0 delegate to the printed target, 1 keep the work, 2 usage, config or I/O error.

| Command | Does |
|---|---|
| `cedr route pick <route> [--size small\|medium\|large] [--avoid H]... [--format text\|json\|t3]` | Pick and record |
| `cedr route manual <harness>/<model> [--effort E] [--format ...]` | Tag a named model and record |
| `cedr route pools [--format text\|json]` | Each harness's state, plus one row per `model_windows` model a route uses |
| `cedr route list` | Routes with their tier chains |
| `cedr route verdict <pick-id> accepted\|reworked\|redone\|rejected [--task ID] [--note TEXT]` | Record a verdict |
| `cedr route stats [--route R] [--format text\|json]` | Picks and verdicts per route and model |
| `cedr route statusline [-- <command>...]` | Claude Code status line: capture `rate_limits` from stdin for the `claude-statusline` reader, then print `claude s23% w41%`, or run `<command>` with the same stdin and print its output instead |
| `cedr route sync [--push-only\|--pull-only] [--quiet]` | Push history, pull the org's shared routes and send the machine snapshot (see Sync); `--quiet` prints only errors |
| `cedr mcp` | MCP server on stdio |

Text output of a pick: the tag, then `  <harness>/<model> role=<role>`, then `  brief starts: <prefix>` when set. `t3` output is for orchestrators that start work through T3 Code's `delegate_task`: the tag, `  delegate_task role=<role> target=<json>` with `{"providerInstanceId", "model", "options"}` where options is `{<effort_option>: <effort>}` or `{}`, then the prefix line. When the work is kept: `keep: <route> (<reason>); <orchestrator summary>` and exit 1. A council with skipped members adds `absent (report, never substitute): <skipped>`. Warnings (stale evaluation, readings failures) go to stderr, and `cedr mcp` writes them to its stderr as well.

JSON output of a pick:

```json
{"route": "review.code", "work": "...", "role": "review", "rules": "8ff065e", "orchestrator": "claude s5% w33%",
 "delegates": [{"id": "p-1a2b3c4d5e6f", "target": "codex/gpt-6.1-sol", "harness": "codex", "model": "gpt-6.1-sol",
   "effort": "high", "state": "ready", "reason": "primary", "summary": "codex w4%", "tag": "[route ...]",
   "prefix": "", "t3": {"providerInstanceId": "codex", "model": "gpt-6.1-sol", "options": {"reasoningEffort": "high"}}}],
 "skipped": [], "reason": null, "warnings": []}
```

`warnings` repeats the stale-evaluation and readings-source warnings, so an orchestrator calling over MCP sees them too.

## History and verdicts

Append-only JSON lines in `$XDG_STATE_HOME/cedr/route-history.jsonl` (default `~/.local/state/cedr/`), one line per write:

```json
{"type": "pick", "id": "p-...", "at": "2026-10-29T01:40:00Z", "route": "review.code", "rules": "8ff065e", "target": "codex/gpt-6.1-sol", "effort": "high", "reason": "primary", "state": "ready", "size": "medium"}
{"type": "verdict", "id": "p-...", "at": "...", "verdict": "accepted", "task": "...", "note": "..."}
```

- A verdict needs a recorded pick id; the latest verdict for an id counts. Notes are one line of at most 120 characters. History holds no briefs, prompts, code or credentials.
- `stats` groups picks by route and target, and reports picks, each verdict count, verdicts recorded, and accepted percent (accepted over verdicts recorded). A cell with fewer than 10 verdicts is marked provisional.

## Sync

`cedr route sync` talks to the cedr.dev app API (`[sync] url`, default `https://api.cedr.dev/v1`; plain `http://` only for `localhost` during development) with a personal API key from `$CEDR_API_KEY` or the file `$XDG_CONFIG_HOME/cedr/api-key` (refused unless mode 600). The key never appears in output, history or errors. `[sync] org` names the organization; the key's owner must be a member.

- **Push**: history records not yet sent, in order, in batches of at most 500: `POST /route/history` with `{"org": "<slug>", "records": [<history lines>]}`. The reply `{"accepted": n, "skipped": n}` counts records stored and records refused (a verdict on a pick the org does not have, a malformed record). The byte offset reached is kept in `$XDG_STATE_HOME/cedr/route-sync.json` per org and URL, and advances only after a batch is accepted, so a failed push is retried from the same record. Picks are stored once per id; a verdict replaces the pick's earlier verdict when its `at` is newer.
- **Pull**: `GET /route/routes?org=<slug>` returns `{"org", "routes", "rules", "updatedAt", "updatedBy"}`, where `routes` is a `routes.toml` text; 404 means the org has no shared routes, and 403 with the error `plan_required` that its plan does not include them; both remove any local copy, so a pick never follows routes the org no longer shares, and neither is a failure. Any other 403 (`not_a_member`, a refused key) is a failure and keeps the copy. The text is validated, refused if it has `[[readings]]`, `[sync]` or `[machine]` (a shared file must never make members' machines run a command or fetch a URL, and what a machine has is for that machine to say), and written to `$XDG_CONFIG_HOME/cedr/org-routes/<slug>.toml`.
- **Machine snapshot**: after the pull (skipped with `--pull-only`), `PUT /route/machine` with this machine's current view, built from the routes in use and fresh readings without recording anything:

  ```json
  {"org": "acme", "machine": "m-1a2b3c4d5e6f", "name": "work-laptop", "at": "2026-10-29T18:02:11Z", "cli": "0.1.0",
   "rules": "8ff065e", "routes": "org",
   "pools": [{"harness": "codex", "family": "", "state": "ready", "summary": "codex w4%", "use": 4.0}],
   "next": [{"route": "review.code", "targets": ["codex/gpt-6.1-sol"], "reason": "primary"}],
   "stale": [{"harness": "codex", "reader": "codex"}]}
  ```

  `machine` is `m-` and 12 lowercase hex characters, made once and kept in `$XDG_STATE_HOME/cedr/machine-id`; `name` is `[machine] name` or the host name up to its first dot, at most 64 characters. `routes` is the `[sync] routes` mode. `pools` are the rows of `cedr route pools --format json`, absent harnesses included. `next` has one entry per route, sorted by route id: the delegates a `medium` pick would choose now (every eligible member for a council route), or none with the reason the work is kept. `stale` lists the machine check's stale harnesses. Strings are cut to the app's limits (names and route ids 64 characters, targets 128, summaries 256, reasons 512) and lists to 64 entries (16 targets, 16 stale), so a large local file never gets the snapshot refused. The reply is `{"ok": true}`. The snapshot is the app's view of the machine, not the sync: when it cannot be built or sent, the sync still exits 0 and prints `history synced, but the plan state was not sent: <reason>` as a warning (a background run logs it). With `routes = "org"` and no shared routes pulled there is nothing to report, so none is sent. The snapshot holds no history, briefs, code, paths or credentials, and the app keeps only the latest one per machine.
- **Automatic sync**: with `[sync]` set and `auto` not false, recording a pick or verdict and a `cedr route statusline` capture start `cedr route sync --quiet --config <the file in use>` in the background (detached, stdin and stdout closed, not waited for) when the last automatic start on this machine was more than 10 minutes ago (or its time is in the future, after a clock change), kept as the modification time of `$XDG_STATE_HOME/cedr/route-sync-auto`. That time is checked first and touched as soon as a start is due, so a status line render costs one file check until then, and the config is read at most once a window; callers racing past the check are kept apart by the lock. The background run writes its last error, if any, to `$XDG_STATE_HOME/cedr/route-sync.log` (replaced each run, removed after a clean run), and `cedr route sync` prints that error as a warning on its next manual run. Two syncs never overlap: `cedr route sync` takes `$XDG_STATE_HOME/cedr/route-sync.lock` (created exclusively with a random nonce; a lock older than 2 minutes is taken over by renaming it away, so of two syncs taking over one stale lock only one succeeds, and a holder removes the file on exit only while it still holds its own nonce) and, when it is held, exits 0 at once under `--quiet` and 2 with `another sync is running` otherwise. A missing key or org only fails that background run; it never delays or fails the pick.
- **Using shared routes**: with `[sync] routes = "org"`, a pick reads the pulled file for everything but `[[readings]]`, `[sync]` and `[machine]`, which stay local. Without a pulled file the pick fails with an error naming `cedr route sync`. The rules hash is the org file's.
- Exit codes: 0 synced, 2 any failure (the message names the step, the HTTP status and the API's error text; never the URL's query or the key).

## MCP tools

`cedr mcp` serves on stdio. Each call reads the config afresh, so edits apply on the next call.

| Tool | Arguments | Returns |
|---|---|---|
| `route_pick` | `route`, `size?`, `avoid?`, `models?` | The pick JSON above |
| `route_manual` | `target`, `effort?` | The pick JSON |
| `route_verdict` | `id`, `verdict`, `task?`, `note?` | `{"recorded": true}` |
| `route_pools` | none | Pool rows |
| `route_list` | none | Routes with tiers |

Errors return as tool errors with the same message the CLI prints.
