# cedr doctor: design contract

`cedr doctor [PATH] [--format text|json]` checks everything cedr needs on this machine and in the repo at `PATH` (default `.`), so a person or their agent can see what is missing and how to fix it. It reads and never writes. It calls the network only once the key checks out: the key check itself, the latest release and the org rule version.

Each check is `{"id", "area", "state": "ok" | "warn" | "fail" | "skip", "message", "fix"}`, where `fix` is the command or step that resolves it (null when `ok`). Text output prints one line per check with its fix indented below, then `cedr: <n> ok, <m> warnings, <k> failures`.

| ID | Area | Checks |
|---|---|---|
| `cli.version` | cli | The running version, and whether a newer release is listed at `https://api.github.com/repos/Sheernox/cedr/releases/latest` (warn when older; skip offline) |
| `key.present` | account | `$CEDR_API_KEY` or `~/.config/cedr/api-key` exists; the file is mode 600 |
| `key.valid` | account | `GET /me` with the key answers 200; names the key's owner, never the key |
| `org.member` | account | The configured org (monitor config, then `[sync] org`) is one the key's owner belongs to (`GET /me` lists them) |
| `route.config` | route | A routes config is found and parses; names its path |
| `route.readers` | route | Route's machine checks, judged on the readings a pick builds (the file, `http`, `command`, `t3` and `claude-statusline` sources; no account probe is started): harnesses skipped or whose program is missing (and T3 Code does not run), and stale usage readers, never an absent harness or one an earlier source covers; warn too when T3 Code has a caches directory on this machine and no `t3` readings source reads it |
| `route.orchestrator` | route | The routes in use set an `[orchestrator]` (names it; warn when none is set, since route tags then leave out the delegating plan's use) |
| `route.targets` | route | Enabled route targets whose harness has a model list (from any source, or the config's `models`) and is present, but whose model resolves to nothing (docs/route.md, Model resolution): warn, naming them |
| `route.statusline` | route | Claude's plan reading: ok when a `t3` source gives Claude a current reading; otherwise `~/.claude/settings.json` (`$CLAUDE_CONFIG_DIR/settings.json` when set) has a status line running `cedr route statusline`, or Claude Code has cached its plan usage (docs/route.md, Built-in readers); warn when Claude is present (on `PATH` or run by T3 Code) and neither holds, or there is no status line and the cache cannot be read or the routes do not have exactly one `claude-statusline` source |
| `route.sync` | route | The last background sync error, if any |
| `mcp.claude`, `mcp.codex`, `mcp.grok` | mcp | `cedr` is registered as an MCP server for that agent (`claude mcp list`, `codex mcp list`, `grok mcp list`; skip when the agent is absent) |
| `monitor.config` | monitor | The monitor config parses |
| `monitor.sources` | monitor | One check per harness from `cedr monitor sources` |
| `monitor.push` | monitor | The last push time (warn after 24 hours with an org set), and the last background push error |
| `monitor.schedule` | monitor | The push timer is installed |
| `sync.config` | sync | In a repo: `.cedr/sync.toml` exists and parses (skip outside a repo) |
| `sync.check` | sync | `cedr sync check` findings, summarised |
| `sync.shadowed` | sync | With T3 Code on the machine and `.cedr/skills/` in the repo: skills an agent T3 Code runs loads from outside the repo (its user folder, a bundle; T3 Code's provider cache lists them) under the name of a skill Sync writes for that agent's target (`.agents/skills/` for `agents`). Warn, naming agent, skill and scope; skip without either |
| `sync.rules` | sync | `.cedr/org-rules.json` is current (warn when the app has a newer version) |
| `guard.lock` | guard | `.cedr/guard.lock` exists and `cedr guard verify` is clean |
| `bench.budget` | bench | The Bench cap in force and where it comes from; fail when `bench_budget_usd` is broken |
| `bench.push` | bench | Whether results are sent automatically and to which org; skip without config or `[sync]`, or when `auto = false`; warn when the config is broken |
| `bench.suite` | bench | One check per suite in `.cedr/bench/`: it plans, with `bench plan`'s warnings (skip when there is none) |

Exit 0 when nothing fails (warnings allowed), 1 when any check fails, 2 on usage failure. `cedr mcp` offers it as the `doctor` tool.
