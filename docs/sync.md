# Cedr Sync: design contract

The frozen contract for Sync: one agent setup in `.cedr/`, compiled into every coding agent's own files, checked in CI, and governed by organization rules from the cedr.dev app. A change to any shape below lands here first, then in the code.

## Scope

1. `cedr sync init` imports a repo's existing agent files into `.cedr/` sources.
2. `cedr sync` compiles the sources into each target agent's native files.
3. `cedr sync check` fails CI when the native files drifted from the sources, or the setup breaks an organization rule.
4. `cedr sync pull` vendors the organization's rules into the repo; `cedr sync push` reports the repo's state to the app.
5. The GitHub Action runs the check on pull requests and can open a pull request that repairs drift.

Out of scope: user-level (home directory) configuration, which stays each person's own; agent settings with no project-level file.

## Repo files

| Path | What |
|---|---|
| `.cedr/sync.toml` | Targets, MCP servers, permissions and hooks |
| `.cedr/rules/*.md` | Instructions, one rule per file, compiled in file-name order |
| `.cedr/skills/<name>/` | Skills: a `SKILL.md` and any files beside it |
| `.cedr/org-rules.json` | The organization's rules as last pulled (see Organization rules); never edited by hand |
| `.cedr/sync.lock` | What the last `cedr sync` wrote: every generated path and its hash |

Commit all of them, and the generated files too: agents read the generated files from the checkout, and CI compares them.

### `.cedr/sync.toml`

```toml
version = 1
targets = ["agents", "claude", "codex", "cursor", "gemini", "copilot", "opencode", "antigravity", "grok"]

[mcp.github]                       # one table per MCP server; the key is its name
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github@2025.4.8"]
env = { GITHUB_TOKEN = "${GITHUB_TOKEN}" }
agents = ["claude", "codex"]       # optional: only these targets get the server

[mcp.docs]
url = "https://mcp.example.com/mcp"
headers = { Authorization = "Bearer ${DOCS_TOKEN}" }

[permissions]
allow = ["shell:npm test", "shell:git status", "fetch:docs.example.com"]
deny = ["shell:rm -rf", "read:.env", "read:**/*.pem"]

[[hooks]]
event = "after-edit"               # before-shell | after-edit | session-start | stop
command = "npm run format --silent"
agents = ["claude", "cursor"]      # optional
```

- `targets` names the agents to write files for: `agents` (the shared `AGENTS.md` and `.agents/skills/`), `claude`, `codex`, `cursor`, `gemini`, `copilot`, `opencode`, `antigravity`, `grok`. Without it, the default is `["agents"]`; an empty list, duplicates and unknown names fail.
- A server has `command` (with optional `args` and `env`) or `url` (with optional `headers`), never both. Names match `[A-Za-z0-9_-]{1,64}`.
- `env` and `headers` values may only reference variables (`${NAME}`, `NAME` matching `[A-Z_][A-Z0-9_]*`) around literal text; a value that looks like a secret (a known key prefix such as `sk-`, `ghp_`, `cedr_`, `xox`, or 32+ characters of base64 or hex) fails with exit 2 naming the key, never its value. Each target gets the variable reference in its own syntax (see Targets).
- Permissions use one vocabulary for every agent: `shell:<command prefix>`, `read:<glob>`, `edit:<glob>`, `fetch:<domain>`, `mcp:<server>` or `mcp:<server>/<tool>`. `shell:*` means any command. Each target gets what it can express; an entry a target cannot express is listed in `cedr sync` output as `not supported by <target>`, never dropped silently.
- `agents` (servers and hooks) and a rule's `agents` front matter name targets from `targets`.
- Every key is optional except `version`; an unknown key or `version` fails with exit 2 naming the file and key.

### Rules: `.cedr/rules/*.md`

```markdown
---
description: Database migrations
paths: ["migrations/**", "src/db/**"]   # optional: the rule applies only to matching files
agents: ["claude"]                     # optional: only these targets get the rule
---
Every migration runs in one transaction and has a rollback.
```

- Front matter is optional. A rule without `paths` applies everywhere; with `paths` it is written to targets that scope rules by path (Cursor, Copilot) as scoped rules, and to the shared `AGENTS.md` under a `## When working in <paths>` heading.
- A rule with `agents` is left out of `AGENTS.md` and written only to those targets' own instruction files (for example `CLAUDE.md` for `claude`), which are generated only when such a rule exists.
- Rules compile in file-name order; name them `10-style.md`, `20-testing.md` to order them. The file name without `.md` is the rule's id.

### Skills: `.cedr/skills/<name>/`

Each directory holds a `SKILL.md` with `name` and `description` front matter (the name matches the directory) and any files it refers to. `cedr sync` copies the directory, byte for byte, into each target's skill folder. Symlinks are refused, and a skill over 1 MiB in total fails.

## Targets

What each target receives. Generated Markdown starts with the line `<!-- Generated by cedr sync from .cedr/. Edit those files and run cedr sync. -->`, or carries it on the line after its YAML front matter when it has some, since agents read front matter only on a file's first line.

| Target | Instructions | Path rules | Skills | MCP servers | Permissions | Hooks |
|---|---|---|---|---|---|---|
| `agents` | `AGENTS.md` | `## When working in <paths>` sections of `AGENTS.md` | `.agents/skills/<name>/` | | | |
| `claude` | `CLAUDE.md` (`@AGENTS.md`, then its own rules) only when a rule is scoped to `claude`; Claude Code reads `AGENTS.md` itself otherwise | `.claude/rules/<id>.md` with `paths` front matter | `.claude/skills/<name>/` | `.mcp.json` `mcpServers` (`${VAR}`) | `.claude/settings.json` `permissions.allow`, `permissions.deny` | `.claude/settings.json` `hooks` |
| `codex` | `AGENTS.md` (shared) | sections of `AGENTS.md` | `.agents/skills/` (shared) | `.codex/config.toml` `[mcp_servers.<name>]` | `.codex/rules/cedr.rules` (`shell:` only) | `.codex/hooks.json` |
| `cursor` | `AGENTS.md` (shared); `.cursor/rules/<id>.mdc` for rules scoped to `cursor` | `.cursor/rules/<id>.mdc` (`globs`) | `.agents/skills/` (shared) | `.cursor/mcp.json` `mcpServers` (`${env:VAR}`) | `.cursor/cli.json` `permissions` | `.cursor/hooks.json` |
| `gemini` | `.gemini/settings.json` `context.fileName = ["AGENTS.md"]`, plus `GEMINI.md` when a rule is scoped to `gemini` | sections of `AGENTS.md` | `.agents/skills/` (shared) | `.gemini/settings.json` `mcpServers` (`httpUrl` for remote, `${VAR}`) | `.gemini/settings.json` `tools.allowed` (`shell:` allow only) | `.gemini/settings.json` `hooks` |
| `copilot` | `AGENTS.md` (shared); `.github/copilot-instructions.md` when a rule is scoped to `copilot` | `.github/instructions/<id>.instructions.md` (`applyTo`) | `.agents/skills/` (shared) | `.vscode/mcp.json` `servers` (`${env:VAR}`) | `.vscode/settings.json` `chat.tools.terminal.autoApprove` (`shell:` only) | `.github/hooks/cedr.json` |
| `opencode` | `AGENTS.md` (shared) | sections of `AGENTS.md` | `.agents/skills/` (shared) | `opencode.json` `mcp` (`{env:VAR}`) | `opencode.json` `permission.bash` (`shell:` only) | not supported |
| `antigravity` | `AGENTS.md` (shared); `.agents/rules/<id>.md` (`trigger: always_on`) when a rule is scoped to `antigravity` | `.agents/rules/<id>.md` (`trigger: glob`, `globs`) | `.agents/skills/` (shared) | `.agents/mcp_config.json` `mcpServers` (`serverUrl` for remote) | not supported | `.agents/hooks.json` |
| `grok` | `AGENTS.md` (shared); `.grok/rules/<id>.md` for rules scoped to `grok` | sections of `AGENTS.md` | `.grok/skills/<name>/` | `.grok/config.toml` `[mcp_servers.<name>]` (`${VAR}`) | `.grok/config.toml` `[permission] rules` | `.grok/hooks/cedr.json` |

Each target is written for the agent's current release (`opencode.json` in its first config format; VS Code's `.vscode/mcp.json`). Details:

- **Shared files.** `AGENTS.md` and `.agents/skills/` are written once for every target that reads them, whatever subset of `targets` asks for them.
- **Variables.** `${NAME}` in a source becomes `${NAME}` (claude, gemini, grok), `${env:NAME}` (cursor, copilot) or `{env:NAME}` (opencode). Codex has no interpolation: an `env` value that is exactly `${NAME}` with the same key becomes `env_vars = ["NAME"]`, a header `Authorization = "Bearer ${NAME}"` becomes `bearer_token_env_var = "NAME"`, another header that is exactly `${NAME}` goes in `env_http_headers`, a literal header in `http_headers`; anything else is `not supported by codex` and the server is left out of that target. Antigravity has no interpolation: a server using any variable is left out of `antigravity` with a note.
- **Permissions.** `shell:<prefix>`: claude `Bash(<prefix>)` and `Bash(<prefix> *)`; codex `prefix_rule(pattern = [<words>], decision = "allow" | "forbidden")`; cursor `Shell(<prefix>)` for a one-word prefix only; gemini `run_shell_command(<prefix>)` in `tools.allowed` (allow only); copilot `"<prefix>": true` for allow and `false` (always ask) for deny; opencode `"<prefix> *": "allow" | "deny"`; grok two `tool = "bash"` rules, `pattern = "<prefix>"` and `"<prefix> *"`. `shell:*` is claude `Bash`, codex none (note), cursor `Shell(*)`, grok `tool = "bash"` with no pattern. `read:<glob>`: claude `Read(<glob>)`, cursor `Read(<glob>)`, grok `tool = "read"`. `edit:<glob>`: claude `Edit(<glob>)`, cursor `Write(<glob>)`, grok `tool = "edit"`. `fetch:<domain>`: claude `WebFetch(domain:<domain>)`, grok `tool = "webfetch"`, `pattern = "domain:<domain>"`. `mcp:<server>[/<tool>]`: claude `mcp__<server>[__<tool>]`, grok `tool = "mcp"`, `pattern = "<server>__<tool>"` (`<server>__*` for a whole server). Anything else is a `not supported by <target>` note.
- **Hooks.** `before-shell`: claude, codex and grok `PreToolUse` matcher `Bash`, cursor `beforeShellExecution`, gemini `BeforeTool` matcher `run_shell_command`, antigravity `PreToolUse` matcher `run_command`. `after-edit`: claude `PostToolUse` matcher `Edit|Write|MultiEdit`, codex `PostToolUse` matcher `apply_patch`, cursor `afterFileEdit`, gemini `AfterTool` matcher `write_file|replace`, antigravity `PostToolUse` matcher `write_file|replace_file_content`, grok `PostToolUse` matcher `Edit|Write`. `session-start`: `SessionStart` (claude, codex, gemini, antigravity, grok), `sessionStart` (cursor, copilot). `stop`: `Stop` (claude, codex, antigravity, grok), `stop` (cursor), `AfterAgent` (gemini), `sessionEnd` (copilot). Copilot has no tool matcher, so `before-shell` and `after-edit` are not supported there. Timeouts are 60 seconds (gemini in milliseconds). Hook commands run with the repo root as the working directory. Grok Build takes a command that is only a relative path from the hook file's folder, so `bin/check.sh` is written as `../../bin/check.sh`.
- **Grok Build.** It holds back a repo's instructions, skills, MCP servers and hooks until the folder is trusted (`grok --trust` or `/hooks-trust`), and `cedr sync` says so each time it writes this target. A rule scoped to `grok` is written whole to `.grok/rules/`, with its `paths` as a `## When working in <paths>` heading, since Grok Build has no path scoping for rules. A server name that cannot form `<server>__<tool>` (a leading digit or hyphen, a trailing `_`, or `__` inside it) is `not supported by grok`.
- **Grok Build with `claude` in `targets`.** Grok Build always reads `CLAUDE.md`, and by default `.claude/rules/*.md`, `.claude/settings.json` permissions and hooks, and `.mcp.json`. A rule scoped to both agents is therefore written only to `CLAUDE.md`. `cedr sync` notes a rule scoped to `claude` alone (it reaches Grok Build anyway) and a rule with `paths` (Grok Build also loads its `.claude/rules/` copy, for every file). Same-named servers in both files use the `.grok/config.toml` entry; a hook named in both files runs once; deny rules from either file apply. Grok Build does not read generated `.cursor/rules/*.mdc` files; with `cursor` in `targets` it reports the entries of `.cursor/hooks.json` as hook load errors and does not run them, which leaves the hooks in `.grok/hooks/cedr.json` as the ones it runs. `cedr sync` owns the whole `[permission]` table of `.grok/config.toml`, as it owns `permissions` in `.claude/settings.json`, and keeps every other table there.

JSON and TOML files are merged, not replaced: `cedr sync` owns only the keys in the table (for example `mcpServers` in `.mcp.json`, `permissions` and `hooks` in `.claude/settings.json`) and keeps every other key and its order. Removing the last owned key leaves the file with its other keys, and removes the file when nothing else is in it.

## Compile: `cedr sync`

`cedr sync [PATH] [--dry-run] [--force]` reads `.cedr/`, applies `.cedr/org-rules.json` when present, and writes every target's files. It prints one line per file: `wrote`, `unchanged` or `removed`, then any `not supported by <target>` notes.

- Output is deterministic: the same sources and org rules give byte-identical files on every machine and OS (LF line endings, keys sorted where the format allows, a trailing newline).
- `.cedr/sync.lock` records each generated path, its target and the SHA-256 of what was written (of the owned keys only, for merged JSON and TOML). A path in the old lock that the new run no longer generates is removed (a merged file only loses its owned keys).
- `cedr sync` refuses to overwrite a Markdown file it did not generate (no header line and not in the lock) unless `--force`: run `cedr sync init` first so its content is imported, not lost. It refuses to overwrite a generated file someone edited by hand (its hash differs from the lock) unless `--force`, and names the file.
- `--dry-run` writes nothing and prints what would change as a unified diff.
- Exit 0 written (or nothing to change), 1 refused (with the files named), 2 usage or I/O failure (including invalid sources).

## Import: `cedr sync init`

`cedr sync init [PATH] [--targets <list>]` creates `.cedr/` sources from the agent files a repo already has, so adopting Sync loses nothing:

- Instructions: `AGENTS.md` becomes `.cedr/rules/10-agents.md`. `CLAUDE.md`, `GEMINI.md` and `.github/copilot-instructions.md` become rules scoped to their agent, unless their content equals `AGENTS.md` or is a lone `@AGENTS.md` import. Cursor `.mdc` and Copilot `.instructions.md` files become rules with their `paths` (`globs`, `applyTo`) and description.
- Skills from `.agents/skills/`, `.claude/skills/` and the other targets' skill folders, once per name (the first found, in that order; a different copy under the same name is reported).
- Rules in `.grok/rules/*.md` become rules scoped to `grok`.
- MCP servers from every target's MCP file, once per name. The same name with a different launch in two files is reported, and the first (in target order) is kept. Literal secret values in `env` or `headers` are replaced by `${NAME}` references named after the key, and listed so the person can set them.
- Permissions and hooks from `.claude/settings.json` (and other targets where they translate, including `.grok/config.toml` `[permission]` and `.grok/hooks/*.json`); entries with no translation, such as Grok Build's `ask` rules, are listed.
- `targets` is `--targets`, else `agents` plus every target whose files were found.

It refuses to run when `.cedr/sync.toml` exists. It writes sources only; run `cedr sync` next to regenerate the native files from them (and review the diff).

## Check: `cedr sync check`

`cedr sync check [PATH] [--format text|json|github]` never touches the network and never writes. It fails (exit 1) on any finding; findings use Guard's lint JSON shape and `--format github` annotations ([design.md](design.md), Lint rules):

| ID | Severity | Finds |
|---|---|---|
| CS001 | error | A generated file differs from what `cedr sync` would write now (the sources changed and `cedr sync` was not run) |
| CS002 | error | A generated file was edited by hand: it differs from the lock's hash |
| CS003 | error | A generated file is missing |
| CS004 | error | An invalid source (the message names the file and key) |
| CS101 | error | An MCP server matches an organization's banned server pattern |
| CS102 | error | Unrestricted shell (`shell:*`) is allowed while the organization forbids it |
| CS103 | error | An unpinned MCP server (Guard's Source resolution) while the organization requires pinned servers |
| CS104 | error | A required organization skill is missing or was changed |
| CS105 | warning | A target the organization requires is not in `targets` |

## Organization rules

Organizations on a plan with org rules (Team and Business) set them in the app. `cedr sync pull [PATH] [--org ID]` fetches them with `GET /sync/rules?org=<id>` and writes `.cedr/org-rules.json`; the next `cedr sync` applies them. Vendoring keeps compiles offline and deterministic, and makes the repo's history show which rule set it ran and when.

```json
{"org": "org-1a2b3c4d5e6f", "version": 7, "hash": "sha256-...", "updatedAt": "2026-10-30T18:02:11Z",
 "instructions": "Never commit secrets.\n",
 "deny": ["shell:rm -rf", "read:.env"],
 "banned": ["npm:@evil/*", "*filesystem*"],
 "targets": ["agents", "claude"],
 "checks": {"unrestrictedShell": false, "pinnedServers": true},
 "skills": [{"name": "security-review", "files": [{"path": "SKILL.md", "content": "---\nname: security-review\n..."}]}]}
```

- `instructions` (Markdown, at most 32 KiB) is written at the end of the shared `AGENTS.md` (and every target instruction file) under `## Organization rules`.
- `deny` is added to every target's denied permissions; a repo's `allow` cannot override it.
- `banned` patterns match a server's name or its source (Guard's Source resolution) with `*` as a wildcard, case-insensitively; a match is CS101 and the server is not written.
- `targets` the organization requires (CS105 when missing).
- `checks.unrestrictedShell = false` makes `shell:*` CS102; `checks.pinnedServers = true` makes an unpinned server CS103.
- `skills` are written like the repo's own skills (CS104 when a generated copy differs). A repo skill with the same name is replaced by the organization's.
- `hash` is the SHA-256 of the canonical JSON of every field but `org`, `version`, `hash` and `updatedAt`; `cedr sync` refuses a file whose hash does not match (edited by hand).
- Responses: 200 the rules; 404 `no_rules` when the org has none (the vendored file is removed); 403 `plan_required` when the plan lacks org rules (the file is kept, and the command says so); other errors fail with exit 2.
- `cedr sync pull` prints `org rules v<old> -> v<new>` or `org rules v<n> unchanged`. Exit 0 pulled or none, 2 on failure.
- Credentials and URL come as for Guard's push ([design.md](design.md), Push): `--org` without `--config` reads no routes config from the checkout.

## Push

`cedr sync push [PATH] [--repo NAME] [--org ID] [--config FILE]` runs the check in process and sends `PUT /sync/repo`, so the app shows every repo's state and the rule set it last applied:

```json
{"org": "org-1a2b3c4d5e6f", "repo": "acme/web", "at": "2026-10-30T18:02:11Z", "cli": "0.4.0", "commit": "3f2269d0c1...",
 "targets": ["agents", "claude", "codex"],
 "rules": {"version": 7, "hash": "sha256-..."},
 "counts": {"rules": 6, "skills": 2, "servers": 3, "allow": 4, "deny": 5, "hooks": 1},
 "files": [{"path": ".mcp.json", "target": "claude", "state": "ok"}],
 "findings": [<check findings>]}
```

- `repo`, `commit` and credentials follow Guard's push. `rules` is null when no org rules are vendored. `files` lists every generated path with `state` `ok`, `drift` (CS001), `edited` (CS002) or `missing` (CS003).
- Limits: 512 files, 1000 findings, strings cut as for Guard; the app refuses a body over 512 KiB (413) or any value over the limits (400).
- The reply is `{"ok": true, "rules": {"version": 8}}`, where `rules` is the organization's current rule version (null when it has none or its plan lacks org rules); when it is newer than the vendored one the command prints `org rules v8 are available: run cedr sync pull`.
- It sends no file contents, env values, rule text or absolute paths. Exit 0 sent, 2 on failure; findings never change the exit status.

## GitHub Action

The action (`uses: Sheernox/cedr@v0.4.0`) gains two inputs:

| Input | Default | What |
|---|---|---|
| `sync` | `auto` | `auto` runs `cedr sync check --format github` when `.cedr/sync.toml` exists; `true` requires it; `false` skips |
| `sync-fix` | `false` | `true` runs `cedr sync` and, when files change, pushes the branch `cedr/sync-fix` and opens (or updates) a pull request with `gh`. Runs only on `push`, `schedule` and `workflow_dispatch` events, never on `pull_request`, and needs `contents: write` and `pull-requests: write` |

With `push-org`, the action also runs `cedr sync push` when `.cedr/sync.toml` exists.

## MCP tools

`cedr mcp` adds:

| Tool | Arguments | Returns |
|---|---|---|
| `sync_check` | `path?` | `{"findings": [...], "files": [...]}`, the push's shapes |
| `sync_plan` | `path?` | The files `cedr sync` would write, each with `state` and a unified diff, without writing |

## Wording

Sync never calls a configuration safe or compliant; it reports which rules a repo breaks and what differs.
