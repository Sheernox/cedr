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
| `route.readers` | route | Route's machine checks: harnesses whose program is missing, and stale usage readers |
| `route.statusline` | route | Claude Code's `~/.claude/settings.json` status line runs `cedr route statusline` (warn when Claude Code is installed and it does not) |
| `route.sync` | route | The last background sync error, if any |
| `mcp.claude`, `mcp.codex` | mcp | `cedr` is registered as an MCP server for that agent (`claude mcp list`, `codex mcp list`; skip when the agent is absent) |
| `monitor.config` | monitor | The monitor config parses |
| `monitor.sources` | monitor | One check per harness from `cedr monitor sources` |
| `monitor.push` | monitor | The last push time (warn after 24 hours with an org set), and the last background push error |
| `monitor.schedule` | monitor | The push timer is installed |
| `sync.config` | sync | In a repo: `.cedr/sync.toml` exists and parses (skip outside a repo) |
| `sync.check` | sync | `cedr sync check` findings, summarised |
| `sync.rules` | sync | `.cedr/org-rules.json` is current (warn when the app has a newer version) |
| `guard.lock` | guard | `.cedr/guard.lock` exists and `cedr guard verify` is clean |

Exit 0 when nothing fails (warnings allowed), 1 when any check fails, 2 on usage failure. `cedr mcp` offers it as the `doctor` tool.
