# cedr doctor: design contract

`cedr doctor [PATH] [--format text|json]` checks everything cedr needs on this machine and in the repo at `PATH` (default `.`), so a person or their agent can see what is missing and how to fix it. It leaves configuration unchanged. Vendor CLI checks may write their own diagnostic logs. Cedr calls the network only once the key checks out: the key check itself, the latest release and the org rule version.

Each check is `{"id", "area", "state": "ok" | "warn" | "fail" | "skip", "message", "fix"}`, where `fix` is the command or step that resolves it (null when `ok`, or when a skipped check needs no action). Text output prints one line per check with its fix indented below, then `cedr: <n> ok, <m> warnings, <k> failures`.

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
| `mcp.<client>` | mcp | One registration check per client below. A missing or unreadable registration warns, and an absent client skips |
| `monitor.config` | monitor | The monitor config parses |
| `monitor.sources` | monitor | One check per harness from `cedr monitor sources`: skip when no source directories exist and neither `harnesses` nor `[paths]` configures it; warn when a present or configured source has no files. Saved absent or unsupported sources also skip when not present or configured |
| `monitor.push` | monitor | The last push time (warn after 24 hours with an org set), and the last background push error |
| `monitor.schedule` | monitor | The push timer is installed; skip when no organization is configured |
| `sync.config` | sync | In a repo: `.cedr/sync.toml` exists and parses (skip outside a repo) |
| `sync.check` | sync | `cedr sync check` findings, summarised; with hand-edited generated files (CS002) the message counts them and the fix says to copy the edits into their `.cedr/` sources or discard them with `cedr sync --force` |
| `sync.shadowed` | sync | With T3 Code on the machine and `.cedr/skills/` in the repo: skills an agent T3 Code runs loads from outside the repo (its user folder, a bundle; T3 Code's provider cache lists them) under the name of a skill Sync writes for it: into its own folder when `targets` names it, and always into `.agents/skills/` for the agents that read it. Warn, naming agent, skill and scope; skip without either |
| `sync.rules` | sync | `.cedr/org-rules.json` is current (warn when the app has a newer version) |
| `guard.lock` | guard | `.cedr/guard.lock` exists and `cedr guard verify` reports any drift as a failure or legacy untyped pins as a warning |
| `compare.budget` | compare | The Compare cap in force and where it comes from; fail when `compare_budget_usd` is broken |
| `compare.push` | compare | Whether results are sent automatically and to which org; skip without config or `[sync]`, or when `auto = false`; warn when the config is broken |
| `compare.suite` | compare | One check per suite in `.cedr/compare/`: it plans, with `compare plan`'s warnings (skip when there is none) |

Exit 0 when nothing fails (warnings allowed), 1 when any check fails, 2 on usage failure. `cedr mcp` offers it as the `doctor` tool.

## MCP registration

These checks look for a server named `cedr`. A registration can exist even when the client disables the server. Config checks inspect the indicated workspace and user files up to 4 MiB. This lets Doctor inspect Gemini CLI, OpenCode and Cursor registrations without starting servers or opening a menu.

| Client | Registration check | Add a missing registration |
| --- | --- | --- |
| Claude Code | `claude mcp list` | `claude mcp add cedr -- cedr mcp` |
| Codex | `codex mcp list` | `codex mcp add cedr -- cedr mcp` |
| Grok Build | `grok mcp list` | `grok mcp add cedr -- cedr mcp` |
| Gemini CLI | `.gemini/settings.json` in the workspace and under `$GEMINI_CLI_HOME` (default home), key `mcpServers` | `gemini mcp add --scope user cedr cedr mcp` |
| Cursor | workspace `.cursor/mcp.json` and `~/.cursor/mcp.json`, key `mcpServers` | Merge the snippet below into `~/.cursor/mcp.json` |
| Copilot in VS Code | `$COPILOT_HOME/mcp-config.json` (default `~/.copilot/mcp-config.json`) and workspace `.mcp.json`, key `mcpServers`. Also checks workspace `.vscode/mcp.json` and the default VS Code and Insiders user profile files, key `servers` | Merge the Copilot snippet below into the Copilot user file |
| OpenCode | `opencode.json` and `opencode.jsonc` in the workspace root and `.opencode/`, and under `$XDG_CONFIG_HOME/opencode` (default `~/.config/opencode`), plus `$OPENCODE_CONFIG` when set. Checks the `mcp.servers` map (v2) or the `mcp` map (v1) | Detects the CLI version. OpenCode v2 uses `opencode mcp add cedr --global -- cedr mcp`. For v1 use the snippet below |
| Muse Code | `muse mcp list` | `muse mcp add cedr --user -- cedr mcp` |
| Antigravity | workspace `.agents/mcp_config.json` and `~/.gemini/config/mcp_config.json`, key `mcpServers` | Merge the snippet below into `~/.gemini/config/mcp_config.json` |

Doctor detects clients on `PATH` (`cursor` or `cursor-agent` for Cursor, `code`, `code-insiders` or `copilot` for Copilot, `agy` or `antigravity` for Antigravity). For these three desktop clients, an existing user configuration directory also counts as presence.

For Cursor and Antigravity:

```json
{"mcpServers":{"cedr":{"command":"cedr","args":["mcp"]}}}
```

For Copilot, add the server to the native Copilot configuration. The VS Code `code --add-mcp` command writes editor-managed configuration for the legacy Local harness.

```json
{"mcpServers":{"cedr":{"type":"stdio","command":"cedr","args":["mcp"],"tools":["*"]}}}
```

For OpenCode v1, merge this into `opencode.json`:

```json
{"mcp":{"cedr":{"type":"local","command":["cedr","mcp"]}}}
```
