# Cedr Monitor: design contract

The frozen contract for Monitor: what `cedr monitor` reads from each coding agent's local logs and plan readings, the local store it keeps, the reports it prints, and the counts it sends to the cedr.dev app. A change to any shape below lands here first, then in the code.

## Scope

1. `cedr monitor` reads each agent's local session logs into a local store and reports tokens, API-equivalent cost, cache use, sessions, tool calls, errors, response times and plan limits, per harness, model, project, session, effort and day.
2. `cedr monitor limits` shows each plan window's use, reset, projection and outlook.
3. `cedr monitor push` sends hourly counts and plan readings to the app, which rolls them up per member, machine, project and organization, keeps their history, and sends budget and plan-limit alerts.
4. `cedr monitor schedule` installs a per-user timer that pushes every hour.

Monitor sends counts only: never prompts, responses, tool arguments or results, file paths, commands, error text or credentials.

## Sources

| Harness | Reads | Usage identity | Notes |
|---|---|---|---|
| `claude` | `~/.claude/projects/**/*.jsonl` (`$CLAUDE_CONFIG_DIR/projects` when set) | `message.id` | Repeated assistant blocks repeat the counters; the last block with usage wins. `cache_creation.ephemeral_1h_input_tokens` is the 1-hour write; the rest of `cache_creation_input_tokens` is 5-minute. `output_tokens_details.thinking_tokens` is reasoning (already inside output). `server_tool_use.web_search_requests` and `web_fetch_requests`. `isSidechain` marks subagent usage. Effort is `perTurnEffort`, else `effort`. `isApiErrorMessage` is an API error; `system` records with subtype `compact_boundary` are compactions; `tool_use` blocks are tool calls and `tool_result` blocks with `is_error` are tool errors. |
| `codex` | `~/.codex/sessions/**/rollout-*.jsonl` and `~/.codex/archived_sessions/**/rollout-*.jsonl` (`$CODEX_HOME` when set) | `token_usage_record.payload.response_id` | Model, effort and cwd from the latest `turn_context`. A `token_usage_record` with a nonempty `thread_id` belongs to that thread's session (a rollout can carry a subagent's or another thread's responses); without one, to the rollout's `session_meta` id. Fresh input is `input_tokens - cached_input_tokens - cache_write_input_tokens`; reasoning is inside output. `event_msg` `token_count` records are never added to totals (they are cumulative); a file with no `token_usage_record` falls back to the `last_token_usage` of each `token_count` whose `total_token_usage` grew, keyed by the cumulative total. `session_meta.payload.source.subagent` marks subagent usage. `function_call` / `custom_tool_call` items are tool calls; `error` and `stream_error` events are API errors; `compacted` is a compaction; `task_complete` `duration_ms` is the turn's wall time. `rate_limits` snapshots become plan readings. |
| `gemini` | `~/.gemini/tmp/*/chats/session-*.jsonl` and `.json` | message `id` within `sessionId` | Records are updates: the last state of each message id wins. Fresh input is `tokens.input - tokens.cached`, cache read `tokens.cached`, output `tokens.output + tokens.thoughts`, reasoning `tokens.thoughts`. `toolCalls[]` are tool calls; a status of `error` is a tool error. |
| `copilot` | `~/.copilot/session-state/*/events.jsonl` (`$COPILOT_HOME`) | session id | Only `session.shutdown` carries usage: its `modelMetrics[<model>].usage` is cumulative per session, so the last shutdown of a session wins (never summed). `tool.execution_complete` events are tool calls, with `success: false` a tool error. Usage of a session that never shut down is unknown, not zero. |
| `opencode` | `opencode.db` in `$XDG_DATA_HOME/opencode/` (`$OPENCODE_DB`), read only, selected columns only | message id | Assistant messages' `tokens.input` (fresh), `tokens.cache.read`, `tokens.cache.write` (5-minute), `tokens.output + tokens.reasoning` (output), `tokens.reasoning`, the provider-reported `cost` as `reportedCostUsd`; an assistant `error` is an API error; tool parts are tool calls, with an `error` state a tool error. Older releases' JSON files under `storage/message/` are read the same way. |
| `muse` | `$XDG_DATA_HOME/muse/sessions/**/session.jsonl` | `usage_id` | Only root journals: `runtime.session.event` of kind `goal_usage_attribution` with `usage_family = "provider"` and `quantity.reported = true` is one response; fresh input `input_tokens - cached_tokens`; reasoning inside output; a `reminder_child` owner is subagent usage. |
| `antigravity` | `conversations/*.db` under `~/.gemini/antigravity*/`, and under `~/.t3/userdata/providers/antigravity/*/` for conversations T3 Code runs | `<conversation>:<generation index>` | Read-only SQLite; `gen_metadata.data` protobuf: field 1, model at 19, usage at 4 (2 fresh input, 3 output, 5 cache read, 10 thinking; output stored as 3 + 10). A conversation with more generations than finished steps that changed in the last 10 minutes is skipped until the next run (it is being written); one unchanged for longer was interrupted, so its finished generations are read and the unfinished ones count as malformed. |
| `grok` | `usage.json` and `events.jsonl` in `~/.grok/sessions/<url-encoded cwd>/<session id>/` (`$GROK_HOME`), never another file there, even under a `[paths]` directory | `<endedAt>:<model>` of a turn | One row per turn and model. Fresh input is `inputTokens - cachedReadTokens - cacheCreationTokens`; reasoning is inside output; `costUsdTicks` / 10^10 is `reportedCostUsd`, null when missing. A fork copies its parent's turns with the same `endedAt`: the first session read keeps them. A parent's usage includes its subagents' model calls, so a session named by a `subagents/<session id>/` directory adds no usage of its own; its tool events count as subagent usage. `tool_started` is a tool call, `tool_completed` with another `outcome` than `success` a tool error. A group name too long to encode has the working directory in `.cwd` beside the sessions. Context size and wall time are unknown. Plan readings come from the `grok` reader ([route.md](route.md), Built-in readers). |
| `cursor` | none | | Cursor keeps no documented local token log; `cedr monitor sources` says so instead of reporting zero. |

- Every reader is incremental: a cursor per file (device and inode, size, modification time, offset of the last complete line, and the SHA-256 of the 4 KiB before it) resumes where the last run stopped. A shorter file or a changed tail hash rereads the file from the start; a partial last line waits for the next run. SQLite sources are reread when their modification time changes.
- A field that is missing stays null (unknown), never zero. Counters must be non-negative integers; a record with cache plus writes above its input or reasoning above its output is skipped and counted as malformed.
- Paths come from the table, the environment variables named, or `[paths]` in the config. A source that is not on the machine is `absent`, not an error.
- Project: the name of the git repository holding the record's working directory (the directory holding `.git` above it, or the directory itself when there is none), after `projects` in the config (see Config). Records whose working directory matches an `exclude` glob are not stored at all.
- Session ids are stored as `s-` and the first 12 hex characters of SHA-256 over `<harness>:<native session id>`.

## Store

`$XDG_STATE_HOME/cedr/monitor.sqlite` (default `~/.local/state/cedr/`), mode 600, holds one row per response (identity above, time, harness, session, project, model, effort, sidechain, the token counts, web searches and fetches, wall time, context tokens), one row per counted event (tool call, tool error, API error, compaction), plan readings, and the cursors. It never holds text from the logs. `cedr monitor reset` deletes it; the next run rebuilds it from the logs that still exist. When a release changes how a harness's records are attributed, the first run after it rereads that harness's files once from the start (keyed by response, so nothing is counted twice); 0.5.1 does this for `codex`. Whenever a stored response moves to another session, the store records its old key (hour, harness, model, effort, project, session, sidechain) in the same write, for Push; an interrupted reread simply runs again.

## Pricing

API-equivalent cost uses `crates/cedr-monitor/prices.json`: per model, USD per million tokens for `input`, `output`, `cacheRead`, `cacheWrite5m` and `cacheWrite1h` (OpenAI's 30-minute write price is `cacheWrite5m`). A model matches the longest `match` prefix of its id; dated suffixes and `-trial`, `-n`, `-a` are ignored. An unpriced model costs 0 and is reported as unpriced. The table holds Grok's under-200k prompt tier, so a larger prompt is under-counted. `cost = (input * input + cacheWrite5m * cacheWrite5m + cacheWrite1h * cacheWrite1h + cacheRead * cacheRead + output * output) / 1e6`. This is what the same tokens would cost on the vendor's API at list price, not what a plan charges. The app keeps the same table and prices pushed counts itself, so history is always priced with the current table.

## Config: `~/.config/cedr/monitor.toml`

```toml
version = 1
org = "org-1a2b3c4d5e6f"   # where push sends; default [sync] org of your own ~/.config/cedr/routes.toml (never a repo's)
projects = "name"          # name (repo directory name) | hash (p- and 12 hex) | none
exclude = ["~/clients/private/**"]   # working directories never collected
harnesses = []             # only these; empty is every harness found
auto = true                # push in the background after status line captures, at most every 30 minutes
# url = "https://api.cedr.dev/v1"

[paths]                    # optional overrides, globs allowed
# claude = "~/.claude/projects"
```

Every key is optional; a missing file is the defaults. An unknown key or `version` fails with exit 2 naming the file and key. The key and URL rules are Route's ([route.md](route.md), Sync); `--org` without `--config` reads no routes config.

## Commands

- `cedr monitor [report] [--since 7d|30d|<YYYY-MM-DD>] [--until <date>] [--by harness|model|project|day|hour|session|effort] [--harness H] [--project P] [--format text|json]` collects, then prints a totals block (cost, tokens by type, responses, sessions, cache hit rate, net cache saving, tool calls and errors, API errors, error-free share, active time) and one row per group with the same columns. Default `--since 7d --by model`. Days are local days unless `--utc`.
- `cedr monitor sessions [--since] [--limit 15]`: the costliest sessions: harness, project, models, first and last time, responses, tokens, cost.
- `cedr monitor limits [--format text|json]`: the latest reading of each plan window from the built-in readers (Claude Code's status line capture, Codex's app server and rollout snapshots, Muse's usage command, Grok Build's agent): used percent, reset time, elapsed share of the window, projected use at reset (`used / elapsed` once 3% has elapsed and use is above zero), the time it would run out, and an outlook: `idle`, `early`, `on track`, `tight` (projection 70% or more), `limit risk` (projection 100% or more), `at limit` (used 100%), `unknown`.
- `cedr monitor sources [--format]`: per harness: found or absent, files, responses, first and last response, malformed records, and why a harness has no data.
- `cedr monitor push [--org ID] [--config FILE] [--quiet] [--full] [--start all|now|<YYYY-MM-DD>|<RFC 3339>]`: see Push.
- `cedr monitor schedule [--every <minutes>] [--remove]`: installs (or removes) a user-level timer running `cedr monitor push --quiet` every 60 minutes by default: a systemd user timer on Linux (`~/.config/systemd/user/cedr-monitor.{service,timer}`), a launch agent on macOS (`~/Library/LaunchAgents/dev.cedr.monitor.plist`), a scheduled task on Windows (`schtasks`, task `cedr monitor`). It prints what it wrote and how to check it. No root.
- `cedr monitor reset`: deletes the store.

Exit 0 success, 2 usage or I/O failure. Reports never fail on findings.

## Push

`POST /monitor/usage` with hourly rows since the last accepted push (minus a 2-hour overlap, since late lines can change an hour), or everything in the store with `--full`, in batches of at most 5,000 rows:

```json
{"org": "org-1a2b3c4d5e6f", "machine": "m-1a2b3c4d5e6f", "name": "work-laptop", "at": "2026-10-30T18:02:11Z", "cli": "0.4.0",
 "rows": [{"hour": "2026-10-30T17:00:00Z", "harness": "claude", "model": "claude-opus-5-5", "effort": "high", "project": "web",
           "session": "s-1a2b3c4d5e6f", "sidechain": false, "responses": 42, "input": 1200, "cacheWrite5m": 30000, "cacheWrite1h": 0,
           "cacheRead": 900000, "output": 25000, "reasoning": 8000, "webSearches": 0, "webFetches": 1, "toolCalls": 61, "toolErrors": 2,
           "apiErrors": 0, "compactions": 0, "wallMs": 512000, "wallSamples": 40, "activeMs": 1830000, "contextPeak": 182000,
           "reportedCostUsd": null, "responseSet": "9f2c4e1a7b3d5f60a1b2c3d4e5f60718"}],
 "limits": [{"harness": "claude", "window": "five_hour", "kind": "session", "percent": 42.0, "resetsAt": "2026-10-30T21:00:00Z",
             "windowMinutes": 300, "at": "2026-10-30T18:01:58Z"}],
 "sources": [{"harness": "claude", "state": "ok", "files": 312, "responses": 48211, "first": "2026-03-02T09:14:00Z",
              "last": "2026-10-30T18:01:40Z", "malformed": 0}]}
```

- One row per `hour` (UTC), `harness`, `model`, `effort`, `project`, `session` and `sidechain`; the row carries the whole hour's counts, so a resent hour replaces the earlier one. `effort` and `project` may be null. `activeMs` is the sum of gaps between the session's consecutive responses in the hour, each capped at 5 minutes (an estimate). `wallMs` over `wallSamples` is the mean response time where the harness reports it.
- `responseSet` identifies the responses behind a row, so the same usage reported by two machines can be told from distinct usage: the first 32 hex characters of the SHA-256 over each response's identity (the Sources table) in sorted order, each followed by a newline, or null for a row with no responses. It covers what the row counts, so a `--start` inside an hour narrows it. It depends only on the logs, so resends and `--full` repeat it, and identities themselves are never sent. Rows from two machines hold the same usage when `hour`, `harness`, `model`, `effort`, `session`, `sidechain` and `responseSet` all match; `project` is left out because machines may name a repo differently. Rows that only partly overlap have different digests, so a receiver counts both rather than risk dropping usage.
- `limits` are the readings taken since the last push (at most 500, newest first). `kind` is `session` (5 hours or less), `weekly`, `model` (a window for some models only) or `other`.
- `sources` is the `cedr monitor sources` view (state `ok`, `absent`, `unsupported` or `error`).
- `machine` and `name` follow Route's machine snapshot. Strings are cut as for Route (model 128, project 128, window 64); counters are at most 10,000,000,000 per row (the CLI caps them); the app refuses a body over 4 MiB or a value over these limits, and keeps at most 2,000,000 rows, 200,000 readings and 20 machines per member, dropping the oldest.
- Repairs: after responses moved to another session (see Store), the next push to each org also resends every affected hour at or after its start in full (repairs before the start wait until the start moves back), and sends a row with every count 0 for each old key that no longer has usage, so the app's copy of the old key is replaced rather than counted beside the new one. The app treats such a zero row as no usage for that key.
- Start: `--start` chooses where the org's usage begins: `all` (everything in the store, the default), `now` (only usage from this moment), or a date (UTC midnight) or time. Responses before it are never sent, by any later push (manual, scheduled, automatic or `--full`); the choice is kept in `$XDG_STATE_HOME/cedr/monitor-start.json` per org and URL, saved before anything is sent, so an interrupted push keeps it. A start inside an hour sends that hour's usage from the start on, and its active time counts only gaps between responses after it. A first push without `--start` sends everything and says so. Moving the start earlier resends from it once; moving it later stops sending older usage but does not remove what the app already has, and on a machine that has pushed it takes effect at the next whole hour (UTC), since the app replaces whole hourly rows. Setup (the app's or an agent's) passes `--start` on the first push.
- The reply is `{"accepted": n, "skipped": n, "historyDays": 90}`: rows older than the plan's history are skipped. The push time is kept in `$XDG_STATE_HOME/cedr/monitor-push.json` per org and URL and advances only after every batch is accepted.
- Exit 0 sent, 2 on failure (the message names the HTTP status and the API's error text, never the key).

With `auto = true` and an org, `cedr route statusline` (which records the Claude Code readings) starts `cedr monitor push --quiet` in the background at most every 30 minutes, the way Route's automatic sync starts.

## MCP tools

`cedr mcp` adds:

| Tool | Arguments | Returns |
|---|---|---|
| `monitor_report` | `since?`, `by?`, `harness?`, `project?` | The report JSON |
| `monitor_limits` | none | The limits JSON |
| `monitor_sources` | none | The sources JSON |

## JSON

`cedr monitor --format json` prints `{"since", "until", "totals": <group>, "groups": [<group>...], "unpriced": [<model ids>]}`, where a group is `{"key", "responses", "sessions", "input", "cacheWrite5m", "cacheWrite1h", "cacheRead", "output", "reasoning", "costUsd", "cacheHitRate", "netCacheSavingUsd", "toolCalls", "toolErrors", "apiErrors", "compactions", "activeMs", "meanWallMs"}`. `cacheHitRate = cacheRead / (input + cacheRead + cacheWrite5m + cacheWrite1h)`; `netCacheSavingUsd = cacheRead * (input price - read price) - cacheWrite5m * (write5m price - input price) - cacheWrite1h * (write1h price - input price)`, over priced models.

## Wording

Report what was measured and how: costs are API-equivalent estimates, active time is estimated, and a harness without data is unknown, never zero.
