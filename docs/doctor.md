# cedr doctor: design contract

`cedr doctor [PATH] [--format text|json]` checks everything cedr needs on this machine and in the repo at `PATH` (default `.`), so a person or their agent can see what is missing and how to fix it. It reads and never writes. It calls the network only once the key checks out: the key check itself, the latest release and the org rule version.

Each check is `{"id", "area", "state": "ok" | "warn" | "fail" | "skip", "message", "fix"}`, where `fix` is the command or step that resolves it (null when `ok`, or when a skipped optional account, push or timer check needs no action). Text output prints one line per check with its fix indented below, then `cedr: <n> ok, <m> warnings, <k> failures`.

| ID | Area | Checks |
|---|---|---|
| `cli.version` | cli | The running version, and whether a newer release is listed at `https://api.github.com/repos/Sheernox/cedr/releases/latest` (warn when older; skip offline) |
| `key.present` | account | `$CEDR_API_KEY` or `~/.config/cedr/api-key` exists, and the file is mode 600 and not empty. No key at all is a skip unless an org is configured (monitor config or the user routes file's `[sync]`), when it fails; a key file with the wrong mode, unreadable or empty always fails |
| `key.valid` | account | `GET /me` with the key answers 200; names the key's owner, never the key |
| `org.member` | account | The configured org (monitor config, then `[sync] org`) is one the key's owner belongs to (`GET /me` lists them) |
| `route.config` | route | A routes config is found and parses; names its path. When missing or invalid, reports the loader's actionable error, including the path, field and line for parse failures |
| `route.readers` | route | Route's machine checks, judged on the readings a pick builds (the file, `http`, `command`, `t3` and `claude-statusline` sources; no account probe is started): harnesses skipped or whose program is missing (and T3 Code does not run), and stale usage readers, never an absent harness or one an earlier source covers; warn too when T3 Code has a caches directory on this machine and no `t3` readings source reads it |
| `route.orchestrator` | route | The routes in use set an `[orchestrator]` (names it; warn when none is set, since route tags then leave out the delegating plan's use) |
| `route.targets` | route | For enabled routes, on present harnesses: warn naming the harnesses with no model list (from any source, or the config's `models`; a harness a `codex` readings source reads lists its models at pick time and is not named), since none of their targets resolve, and the targets whose model resolves to nothing in the harness's list (docs/route.md, Model resolution). It also warns naming each enabled candidate on a present harness with a resolved model (`<route> <target>=<effort>`) whose effort is off the ladder (docs/route.md, Model options) and not among the efforts the readings list for its resolved model, since such an effort is passed on as written. `self`, absent harnesses and unresolved models are excluded; when a Codex reader supplies model options only at pick time, efforts are not judged without those options. The fix is `models = [...]` on the harness or a readings source that lists models, and an effort on the ladder unless the model offers your own |
| `route.statusline` | route | Claude's plan reading: ok when a `t3` source gives Claude a current reading; otherwise `~/.claude/settings.json` (`$CLAUDE_CONFIG_DIR/settings.json` when set) has a status line running `cedr route statusline`, or Claude Code has cached its plan usage (docs/route.md, Built-in readers); warn when Claude is present (on `PATH` or run by T3 Code) and neither holds, or there is no status line and the cache cannot be read or the routes do not have exactly one `claude-statusline` source |
| `route.sync` | route | The last background sync error, if any |
| `mcp.claude`, `mcp.codex`, `mcp.grok` | mcp | `cedr` is registered as an MCP server for that agent (`claude mcp list`, `codex mcp list`, `grok mcp list`; skip when the agent is absent) |
| `monitor.config` | monitor | The monitor config parses |
| `monitor.sources` | monitor | One check per harness from `cedr monitor sources`: skip when no source directories exist and neither `harnesses` nor `[paths]` configures it; warn when a present or configured source has no files. Saved absent or unsupported sources also skip when not present or configured |
| `monitor.push` | monitor | The last push time (warn after 24 hours with an org set), and the last background push error |
| `monitor.schedule` | monitor | The push timer is installed; skip when no organization is configured |
| `sync.config` | sync | In a repo: `.cedr/sync.toml` exists and parses (skip outside a repo) |
| `sync.check` | sync | `cedr sync check` findings, summarised; with hand-edited generated files (CS002) the message counts them and the fix says to copy the edits into their `.cedr/` sources or discard them with `cedr sync --force` |
| `sync.shadowed` | sync | With T3 Code on the machine and `.cedr/skills/` in the repo: skills an agent T3 Code runs loads from outside the repo (its user folder, a bundle; T3 Code's provider cache lists them) under the name of a skill Sync writes for it: into its own folder when `targets` names it, and always into `.agents/skills/` for the agents that read it. Warn, naming agent, skill and scope; skip without either |
| `sync.rules` | sync | `.cedr/org-rules.json` is current (warn when the app has a newer version) |
| `guard.lock` | guard | `.cedr/guard.lock` exists and `cedr guard verify` is clean |
| `bench.budget` | bench | The Bench cap in force and where it comes from; fail when `bench_budget_usd` is broken |
| `bench.push` | bench | Whether results are sent automatically and to which org; skip without config or `[sync]`, or when `auto = false`; warn when the config is broken |
| `bench.suite` | bench | One check per suite in `.cedr/bench/`: it plans, with `bench plan`'s warnings (skip when there is none) |

Exit 0 when nothing fails (warnings allowed), 1 when any check fails, 2 on usage failure. `cedr mcp` offers it as the `doctor` tool.
