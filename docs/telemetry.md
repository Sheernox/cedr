# cedr telemetry: design contract

cedr sends anonymous usage events to cedr.dev so the team can see how many people use it, on which platforms and with which agents. It is on by default, says so before it sends anything, and turns off with one command. The public description is [cedr.dev/docs/cli/telemetry](https://cedr.dev/docs/cli/telemetry); this file is the contract the CLI and the app's ingest both follow.

## Consent

- **Notice.** The first command that would record an event prints this notice to stderr, records that it was shown, then records the event:

  ```
  cedr: cedr sends anonymous usage data (command names, exit codes, timing, version, OS,
  cedr: which coding agents are installed, and a few counts) to improve cedr. It never sends
  cedr: arguments, paths, repo names or code. Turn it off: cedr telemetry off (or CEDR_TELEMETRY=0).
  cedr: cedr also updates itself once a day: cedr update --auto off turns that off.
  cedr: Details: https://cedr.dev/docs/cli/telemetry
  ```

  CI runs never print it. `cedr mcp` never prints it either (an agent keeps its stderr) and records nothing until a command run directly has shown it.
- **Off switches**, any one of which wins: `cedr telemetry off`; `CEDR_TELEMETRY` set to `0`, `false` or `off`; `DO_NOT_TRACK` set to anything but empty, `0`, `false` or `off` (so `DO_NOT_TRACK=false` allows tracking, as other tools read it). `cedr telemetry off` also deletes the install ID and the queue. `cedr telemetry on` prints the notice and turns it back on with a new install ID.
- `cedr telemetry status [--format text|json]` prints whether telemetry is on, what turned it off, the install ID and the number of queued events. JSON: `{"enabled": bool, "reason": "config" | "CEDR_TELEMETRY" | "DO_NOT_TRACK" | null, "install": string | null, "queued": integer, "ci": bool}`.

## Settings

`$XDG_CONFIG_HOME/cedr/cli.toml` (default `~/.config/cedr/cli.toml`) holds the CLI's own settings; a missing file means the defaults. Unknown keys are kept on rewrite. A file that cannot be read or does not parse turns telemetry and auto-update off for that run and warns once on stderr, rather than overwriting it.

| Key | Default | Meaning |
|---|---|---|
| `telemetry` | `true` | Send usage events |
| `install_id` | generated | Random UUID v4, created on the first recorded event; never derived from the machine, user or account |
| `notice` | absent | `1` once the notice above was shown |
| `auto_update` | `true` | See [update.md](update.md) |
| `bench_budget_usd` | absent | Bench's cap; see [bench.md](bench.md) |

## Events

Every command records one event when it ends, except `completions`, `route statusline` (it runs on every prompt render), `telemetry` itself and the hidden background commands. An event is appended as one JSON line to `$XDG_STATE_HOME/cedr/telemetry.jsonl` (default `~/.local/state/cedr/`); an event is dropped instead of queued while the queue file is over 512 KiB, a flush sends at most the newest 1,000, and events older than 7 days are dropped unsent.

| Field | Type | Meaning |
|---|---|---|
| `at` | string | ISO-8601 UTC time the command ended |
| `version` | string | The cedr version that ran it |
| `command` | string | The subcommand path from cedr's own command list, at most two words: `sync`, `route pick`, `monitor push` |
| `exit` | integer | The exit code (0 clean, 1 findings or changes, 2 usage or I/O failure) |
| `ms` | integer | Wall time in milliseconds |
| `counts` | object | Optional counts a command reports, from the list below |

Counts (each an integer from 0 to 100,000; a command reports only what it already computed):

| Key | Reported by | Meaning |
|---|---|---|
| `sync_agents` | `sync` | Agents the setup compiles to |
| `guard_skills`, `guard_servers` | `lock` | Skills and MCP servers pinned |
| `route_routes` | `route pick` | Routes in the config used |
| `monitor_agents` | `monitor` and its subcommands that read usage | Agents whose usage was found |
| `doctor_warn`, `doctor_fail` | `doctor` | Warning and failing checks |

Nothing else is ever recorded: no arguments or flag values, file or repo names, host or user names, error text, prompts, code or environment.

## Sending

A recorded event starts `cedr telemetry flush` (hidden) in the background when the last flush began more than an hour ago, and always in CI. The flush takes a lock (one at a time per machine), moves the queue to `telemetry.sending` (unless a flush that did not finish left one there, which goes first), and sends it in batches of at most 200 with a 5-second timeout per request. It drops what the app accepted (2xx) or refused as invalid (400, 413), stops at the first other failure, after 60 seconds, or once telemetry is turned off (then deleting both files), and leaves what was not sent in `telemetry.sending` for the next flush. It never prints, and a command never waits for it or changes its output or exit code because of it.

`POST https://api.cedr.dev/v1/telemetry`, no authentication, `Content-Type: application/json`, at most 256 KiB:

```json
{
  "install": "0d6a2c1e-7b3f-4a8e-9c51-2f4e8b7d6a10",
  "ci": false,
  "os": "linux",
  "arch": "x86_64",
  "agents": ["claude", "codex"],
  "events": [
    { "at": "2026-10-30T18:02:11Z", "version": "0.5.0", "command": "route pick", "exit": 0, "ms": 41, "counts": { "route_routes": 12 } }
  ]
}
```

- `install`: the install ID; in CI a new random ID per process, so CI never counts as an active install.
- `ci`: true when `CI` (not `false` or `0`), `GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE`, `CIRCLECI`, `TF_BUILD` or `JENKINS_URL` is set.
- `os`: `linux`, `macos`, `windows` or `other`; `arch`: `x86_64`, `aarch64` or `other`.
- `agents`: which of `claude`, `codex`, `gemini`, `copilot`, `opencode`, `muse`, `antigravity`, `grok`, `cursor` have their home directory on this machine (`~/.claude` or `$CLAUDE_CONFIG_DIR`, `~/.codex` or `$CODEX_HOME`, `~/.gemini`, `~/.copilot` or `$COPILOT_HOME`, `$XDG_DATA_HOME/opencode`, `$XDG_DATA_HOME/muse`, `~/.gemini/antigravity`, `~/.cursor`). Found by checking that the directory exists; nothing is read or run.
- `events`: 1-200 events as above.

`CEDR_TELEMETRY_URL` replaces the endpoint for tests; it must be https or loopback http. Builds run from the source tree (`cargo test`, `cargo run`) set `CEDR_TELEMETRY=0` and `CEDR_NO_AUTO_UPDATE=1` in `.cargo/config.toml`.

Replies: `200 {"accepted": n, "skipped": m}` (events outside the window below are skipped), `400 {"error": "bad_request", ...}` for anything off-contract (the whole batch is refused), `413` over 256 KiB, `429` when the install sent more than 60 batches in an hour.

## The app

The app validates every field against this contract (commands match `^[a-z][a-z0-9-]{0,31}( [a-z][a-z0-9-]{0,31})?$`), skips events dated more than 7 days back or more than an hour ahead, and stores each event with the batch's fields and no IP address or other request metadata. Raw events are kept 90 days; the nightly job keeps one rollup row per UTC day indefinitely. Active installs are distinct non-CI install IDs per UTC day (DAU), over the 7 days (WAU) and 30 days (MAU) ending that day; CI is counted as runs.
