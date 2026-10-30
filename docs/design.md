# Cedr Guard: design contract

The frozen contract for Guard, in preview: its commands, JSON shapes, lint rules and lock fields. A change to any shape below lands here first, then in the code.

## Scope

Guard covers the loop end to end on one machine:

1. `cedr lint` finds problems in a repo's agent setup.
2. `cedr lock` pins every skill and MCP server and snapshots each server's tool surface.
3. `cedr verify` fails CI when the setup drifts from the lock; the GitHub Action runs `lint` and `verify` on pull requests.
4. `cedr push` sends the pinned inventory and the latest findings to the cedr.dev app, which shows every repo of an organization in one place.
5. A watcher imports a lock, watches the pinned npm packages through the watcher commands, and raises an alert when a newer version changes the surface.

Out of scope: PyPI, OCI and git sources for surfaces (recorded in the lock, never fetched), remote MCP polling and sandboxed execution.

## Layout

| Path | What |
|---|---|
| `crates/cedr-core/` | Library: discovery, lint, lock, surface extraction, diff |
| `crates/cedr/` | The `cedr` binary (thin clap layer over `cedr-core`) |
| `fixtures/` | Sample repos and generated npm tarballs for tests |

## Reuse, do not rebuild

- MCP client: the official Rust SDK `rmcp` (features `client`, `transport-child-process`) for `--introspect`.
- JavaScript and TypeScript parsing: `tree-sitter` with `tree-sitter-javascript` and `tree-sitter-typescript`.
- File walking: `ignore` (ripgrep's walker, honours `.gitignore`).
- npm tarballs: `reqwest` (rustls, blocking, redirects only to https), `flate2`, `tar`; verify `dist.integrity` (SRI sha512) before reading, and cap the total decompressed bytes of every entry, scanned or not.
- Skill risk patterns: port the static regex families for prompt injection, data exfiltration and supply chain from NVIDIA SkillSpector (Apache-2.0, `src/skillspector/nodes/analyzers/static_patterns_*.py`) into a Rust data file, with attribution in `NOTICE` and a header comment naming the source file and commit.

## Repo files

Everything Guard keeps in a repo lives in `.cedr/`, next to Route's optional `.cedr/routes.toml`:

| Path | What |
|---|---|
| `.cedr/guard.toml` | Optional config (below) |
| `.cedr/guard.lock` | The lock (see Lock file) |
| `.cedr/surfaces/` | One surface per locked server |

A lock at the legacy path `cedr.lock` (repo root, 0.2.0 and earlier) is read when `.cedr/guard.lock` is absent. `cedr lock` writes `.cedr/guard.lock`, removes a legacy `cedr.lock` and prints `moved cedr.lock to .cedr/guard.lock`.

### Config: `.cedr/guard.toml`

```toml
version = 1
skills = ["agents/skills"]   # extra skill roots, relative to the repo root; each holds <name>/SKILL.md

[lint]
ignore = ["CG002"]           # rule ids never reported

[[lint.allow]]               # a rule not reported under some paths
rule = "CG009"
paths = [".agents/skills/security-audit/**"]   # globs over the finding's path, `/` separators
```

- Every key is optional; a missing file is the empty config. An unknown `version`, an unknown key, a rule id that is not `CG001`-`CG010`, a skill root that is absolute, is the root itself, contains `..` or does not exist, and an invalid glob all fail with exit 2 and a message naming the file and key.
- Extra skill roots are discovered, linted, locked and verified like the standard ones, under the same containment rule. A skill name found in two roots is two entries; the lock keys skills by `path`, so they never collide.
- `ignore` and `allow` apply to `cedr lint` only. `cedr verify` findings are never suppressed: drift from the lock is what the lock is for.

## Discovery

`cedr` scans a repo root (default `.`) for:

| Agent | Instructions | Skills | MCP servers | Permissions |
|---|---|---|---|---|
| Any | `AGENTS.md` (any depth) | `.agents/skills/<name>/SKILL.md` | | |
| Claude Code | `CLAUDE.md` | `.claude/skills/<name>/SKILL.md` | `.mcp.json` (`mcpServers`) | `.claude/settings.json` `permissions.allow` |
| Codex | | | `.codex/config.toml` (`[mcp_servers.<name>]`) | `.codex/config.toml` `sandbox_mode`, `approval_policy` |
| Cursor | | `.cursor/skills/<name>/SKILL.md` | `.cursor/mcp.json` (`mcpServers`) | |
| Gemini | `GEMINI.md` | `.gemini/skills/<name>/SKILL.md` | `.gemini/settings.json` (`mcpServers`; `httpUrl` is remote) | |
| Copilot | `.github/copilot-instructions.md` | `.github/skills/<name>/SKILL.md` | `.vscode/mcp.json` (`servers`), `.github/mcp.json` (`mcpServers`) | |
| opencode | | `.opencode/skills/<name>/SKILL.md` | `opencode.json` (`mcp`: `type` `local` with a `command` array, or `remote` with `url`) | |
| Antigravity | | | `.agents/mcp_config.json` (`mcpServers`; `serverUrl` is remote) | |

A server entry is stdio (`command`, `args`, `env`) or remote (`url`, `httpUrl`, `serverUrl`, or `type`/`transport` of `http`/`sse`). Clients are `claude`, `codex`, `cursor`, `gemini`, `copilot`, `opencode` and `antigravity`. The same server name in two clients is two lock entries keyed by `client` and `name`.

Skill directories and the files in them are read only when their canonical path stays inside the canonical root (a `.claude/skills` link into `.agents/skills` is fine). A file symlink inside a skill is never followed: the hash and the lint rules both see its target text. A skill directory whose canonical path escapes the root is skipped, never read: `cedr lint` reports it as CG010, and `cedr lock` and `cedr verify` print `skipped <path>: links outside the repo` on stderr and carry on with the rest.

**Source resolution** for stdio servers:
- `npx [-y] <pkg>[@<ver>]`, `pnpm dlx`, `bunx` give `npm:<pkg>@<ver>`; no version or a dist-tag (`latest`, `next`) is unpinned.
- `uvx <pkg>[==<ver>]` gives `pypi:<pkg>==<ver>`.
- `docker run ... <image>[@sha256:<digest>]` gives `oci:<image>@<digest>`; a tag alone is unpinned.
- Anything else gives `command:<command>`.
- Remote servers give `url:<url>`.
- `command:` and `url:` sources count as pinned: there is no version to pin, so CG006 stays quiet and the surface diff is the only change signal.

## Lint rules

Findings never show a URL's user info, query or fragment, and show a `command:` source by its program name, without its directory.

| ID | Severity | Finds |
|---|---|---|
| CG001 | error | A command in an instruction file (`npm run X`, `npm --prefix D run X`, `pnpm X`, `yarn X`) whose script is missing from the nearest `package.json` (or `D/package.json`) |
| CG002 | warning | A relative markdown link in an instruction file or SKILL.md whose target does not exist |
| CG003 | warning | `CLAUDE.md` or `GEMINI.md` next to `AGENTS.md` whose content differs and is not a symlink to it or a lone `@AGENTS.md` import |
| CG004 | error | SKILL.md frontmatter missing `name` or `description`, `name` not matching its directory or longer than 64 characters, `description` longer than 1024 characters |
| CG005 | error | Hidden Unicode in instruction or skill files: tag characters U+E0000-U+E007F, bidi controls U+202A-U+202E and U+2066-U+2069, zero-width U+200B-U+200D and U+2060, U+FEFF after the first byte |
| CG006 | warning | An unpinned MCP server (see Source resolution) |
| CG007 | warning | Broad permissions: Claude `Bash` or `Bash(*)` in `permissions.allow`; Codex `sandbox_mode = "danger-full-access"` or `approval_policy = "never"` |
| CG008 | warning | A remote MCP server over plain `http://` that is not localhost |
| CG009 | per pattern | A ported SkillSpector pattern matched in a skill directory; SkillSpector high maps to error, medium and low to warning, and the message keeps the original severity |
| CG010 | error | A skill directory that links outside the repo root (it is skipped, not read) |

`cedr lint [PATH] [--format text|json|github] [--strict]`. Exit 0 when clean, 1 when any error is found (or any warning with `--strict`), 2 on usage or I/O failure.

`--format github` (lint and verify) prints one GitHub Actions workflow command per finding, `::error file=<path>,line=<line>,title=<rule>::<message>` (`::warning` for warnings; `line=` omitted when `line` is null), so findings show as annotations on the pull request. Values are escaped as the runner expects: `%` `\r` `\n` as `%25` `%0D` `%0A` in the message, and additionally `:` and `,` as `%3A` and `%2C` in properties. Diagnostics on stderr escape control characters in repo paths, so no repo content starts a workflow command. A last line `cedr: <n> errors, <m> warnings` follows on stdout.

```json
{ "version": 1, "findings": [
  { "rule": "CG001", "severity": "error", "path": "AGENTS.md", "line": 12, "message": "npm script \"deploy\" not found in package.json" }
] }
```

Paths are relative to the scanned root with `/` separators; `line` is 1-based or `null`.

## Surface

A surface is what a server exposes to a model, as one JSON document. It is stored in `.cedr/surfaces/<client>.<name>.json` next to the lock, with bytes outside `[A-Za-z0-9._-]` in the name percent-encoded so two names never share a file.

```json
{
  "version": 1,
  "source": "npm:@acme/mcp-github@1.4.2",
  "method": "static",
  "integrity": "sha512-...",
  "tools": [
    { "name": "create_issue", "description": "Create an issue", "schema": "{ title: z.string() }" }
  ],
  "egress": ["api.github.com"],
  "capabilities": ["child_process", "env", "fs_write", "network"],
  "hash": "sha256-<hex>"
}
```

- `method`: `static` (parsed from the package), `introspect` (`tools/list` over stdio) or `none` (not extracted; `tools` empty).
- `tools` sorted by `name`. `schema` is the raw source text of the schema argument (static) or the canonical JSON of `inputSchema` (introspect), or `null`.
- Static extraction recognises two registration styles:
  - High-level: calls whose callee ends in `.tool`, `.registerTool` or `.addTool`. The name is the first argument; the description is a string second argument or the `description` property of an object argument.
  - Low-level: object literals with a string `name` property and an `inputSchema` property (the tool list returned by a `ListToolsRequestSchema` handler). The description is its `description` property.
  - An identifier argument or property value resolves to a same-file `const` initialised with a string literal, a template literal without substitutions or an object literal; a schema identifier resolves to the raw text of its initializer. Anything else is left unresolved rather than guessed.
- `egress`: sorted, unique hostnames from `http(s)://` string literals (the host after any `user:pass@`, without port or IPv6 brackets), excluding `localhost`, the loopback addresses `127.0.0.1` and `::1`, `example.com`/`example.org` and schema hosts (`json-schema.org`, `www.w3.org`).
- `capabilities`: sorted subset of `child_process` (`child_process`, `execa`, `node:child_process`), `network` (`fetch(`, `http`, `https`, `net`, `undici`, `axios`), `fs_write` (a call to `writeFile`, `appendFile`, `createWriteStream`, `rm` or `unlink`, with or without `Sync`, on a binding imported or required from `fs`, `fs/promises` or their `node:` forms, or a named import of one of them; a property merely named `rm` does not count), `env` (`process.env`).
- Scanned files: `.js`, `.mjs`, `.cjs`, `.ts`, `.mts`, `.cts` except `.d.ts` and anything under `node_modules/`.
- `hash`: `sha256-` plus the hex digest of the canonical JSON (sorted keys, no whitespace) of `{tools, egress, capabilities}`.

## Diff

`cedr diff OLD.json NEW.json [--format text|json]` compares two surfaces. Exit 0 when the hashes match, 1 when changes are found.

```json
{ "version": 1, "from": "npm:x@1.0.0", "to": "npm:x@1.1.0", "changes": [
  { "kind": "egress_added", "tool": null, "detail": "evil.example.net", "before": null, "severity": "high" }
] }
```

| kind | severity |
|---|---|
| `egress_added`, `capability_added` | high |
| `description_changed` | high when the new text matches a CG005 or CG009 pattern, else medium |
| `tool_added`, `schema_changed` | medium |
| `tool_removed`, `egress_removed`, `capability_removed` | low |

`detail` is the host or capability for egress and capability changes, the new description or schema for `description_changed` and `schema_changed`, and the tool's description for `tool_added` and `tool_removed` (empty when there is none). `before` is the old description or schema for `description_changed` and `schema_changed` (empty string when there was none) and `null` for every other kind; text output prints it on a `was:` line under the change.

Changes are ordered by severity (high first), then kind name alphabetically, then tool.

## Lock file

`.cedr/guard.lock` (see Repo files), TOML, written deterministically through a temporary file and a rename (so a failed write never leaves a partial lock, and a symlink at the path is replaced, never written through; `.cedr/` and `.cedr/surfaces/` must resolve inside the repo) (all `[[skill]]` entries by name, then all `[[mcp]]` entries by client and name).

```toml
version = 1

[[skill]]
name = "deploy"
path = ".agents/skills/deploy"
content = "sha256-<hex>"            # over `<path>\0<len>\0<bytes>` per file in path order; a symlink contributes its target text
allowed_tools = ["Bash(npm run *)"] # from SKILL.md frontmatter `allowed-tools`, empty when absent

[[mcp]]
client = "claude"
name = "github"
transport = "stdio"                 # or "http"
source = "npm:@acme/mcp-github@1.4.2"
pinned = true
integrity = "sha512-..."            # npm dist.integrity; omitted when not resolved
launch = "sha256-<hex>"             # canonical JSON of {command, args, env keys sorted, url}: catches changed arguments and env
surface = "sha256-<hex>"            # hash of .cedr/surfaces/claude.github.json; omitted with method "none"
method = "static"
tools = ["create_issue", "search_code"]
egress = ["api.github.com"]
```

`cedr lock [PATH] [--offline] [--introspect]`:
- Resolves unpinned npm sources to the dist-tag the config names (`latest` when none) and records the resolved version with `pinned = false`. `cedr verify` accepts an unpinned entry while the config still names the same package unpinned.
- Extracts npm surfaces statically. `--offline` skips the network and records `method = "none"`.
- `--introspect` runs each stdio server with its configured command and a cleared environment holding only `PATH`, `HOME` and the configured env, calls `initialize` and `tools/list` with a 20-second timeout, and uses that surface (`method = "introspect"`). Egress and capabilities still come from static extraction when the package could be fetched. It prints a warning first: this executes the configured servers.

`cedr verify [PATH] [--format text|json|github]` never touches the network. It fails (exit 1) when a skill's content hash differs, a skill or server was added or removed, or a server's source changed. Findings use the lint JSON shape with rules `CV001` (content changed), `CV002` (added), `CV003` (removed), `CV004` (source changed) and `CV005` (launch changed: command, arguments, env keys or URL). Env values are never hashed or stored, only their keys.

## Updates

`cedr outdated [PATH] [--format text|json|github] [--fail-on high|medium|low|never]` checks each locked npm server for a newer version and shows what it would change, without installing or running anything:

1. For each `[[mcp]]` entry with an `npm:` source, read the registry's `latest` dist-tag (as `cedr latest`).
2. When it is newer than the locked version (semver; an unpinned entry compares the version it resolved to), extract the newer version's surface statically (as `cedr surface`) and diff it against the locked surface (as `cedr diff`; an entry locked with `method = "none"` reports the version alone).
3. Report one update per server: `{"client", "name", "source", "latest", "severity", "changes": [<diff changes>]}`, where `severity` is the highest change's (`none` when the surfaces match).

Other sources (`pypi:`, `oci:`, `command:`, `url:`) are listed as `not checked` in text output and left out of JSON. A registry or network failure for one server is a warning naming the server, never a failure of the run. JSON output is `{"version": 1, "updates": [...]}`; `--format github` prints one `::warning` per update (`::error` for high). Exit 0, or 1 when an update reaches `--fail-on` (default `never`), 2 on usage or I/O failure.

## Push

`cedr push [PATH] [--repo NAME] [--org SLUG] [--config FILE]` sends the repo's pinned inventory and its current findings to the cedr.dev app, so an organization sees every repo's agent setup in one place. It uses Route's credentials ([route.md](route.md), Sync), the key from `$CEDR_API_KEY` or `$XDG_CONFIG_HOME/cedr/api-key`. With `--org` and no `--config` it sends to the default API URL and reads no routes config at all: a CI checkout is untrusted, and a `.cedr/routes.toml` in it must never redirect the key. Otherwise `[sync] url` and `org` come from the routes config found as Route finds it (`--config` first), with `--org` overriding `[sync] org`.

It reads the lock (exit 2 with `no lock: run cedr lock first` when there is none), runs lint and verify in process, and sends `PUT /guard/repo`:

```json
{"org": "acme", "repo": "web", "at": "2026-10-29T18:02:11Z", "cli": "0.3.0", "commit": "3f2269d0c1...",
 "skills": [{"name": "deploy", "path": ".agents/skills/deploy", "content": "sha256-...", "allowed_tools": ["Bash(npm run *)"]}],
 "mcp": [{"client": "claude", "name": "github", "transport": "stdio", "source": "npm:@acme/mcp-github@1.4.2", "pinned": true,
          "method": "static", "surface": "sha256-...", "tools": ["create_issue"], "egress": ["api.github.com"]}],
 "lint": [<lint findings>], "verify": [<verify findings>]}
```

- `repo` is `--repo` or the name of the canonical root directory; it must match `[A-Za-z0-9._/-]{1,128}` after that, or the push fails naming the flag. `commit` is the 40-hex `HEAD` of the repo when it is a git checkout (read from `.git`, no `git` process), else `null`.
- `skills` and `mcp` are the lock's entries in lock order; `surface` is `null` with method `none`. Findings use the lint JSON shape (`lint` after `guard.toml` suppression).
- Limits: strings cut to 1024 characters (names, paths and sources to 256), lists to 512 entries (`tools` and `egress` 256 per server, each finding list 1000), so the app never refuses a push for one long value; each list cut short prints `sent N of M <what>` as a warning. The app refuses a body over 2 MiB (413) and any value over these limits (400); a push that would exceed 2 MiB fails before sending, naming the size. It never sends file contents, env values, launch commands or absolute paths, and a `url:` source goes without its user info, query or fragment.
- The app shows when it received each push (not the client's `at`), keeps at most 200 repos and 32 MiB of stored pushes per organization (the oldest pushes are dropped first), and keeps a repo's push when its sender leaves the organization. Owners and admins can remove a repo's record.
- `--updates` adds `"updates": [{"client", "name", "source", "latest", "severity", "changes"}]` from `cedr outdated`, where `changes` is the number of surface changes (at most 512 updates); without it the field is absent. The GitHub Action passes it.
- The reply is `{"ok": true}`; the app keeps only the latest push per organization and repo. Exit 0 sent, 2 on any failure (the message names the HTTP status and the API's error text, never the URL's query or the key). It does not change exit status on findings: `verify` gates, `push` reports.

## GitHub Action

The public repo's root `action.yml` is a composite action: `uses: Sheernox/cedr@v0.3.0`.

| Input | Default | What |
|---|---|---|
| `version` | the action's own tag, else the latest release | cedr release to install |
| `path` | `.` | Repo root to check |
| `strict` | `false` | Pass `--strict` to lint |
| `verify` | `auto` | `auto` runs `cedr verify` when a lock exists; `true` requires one; `false` skips it |
| `push-org` | empty | When set with the `CEDR_API_KEY` env, run `cedr push --org <it> --updates` after the checks, also when they fail |
| `outdated` | `true` | Run `cedr outdated --format github` after verify, so newer versions of pinned servers show as annotations; it never fails the job |

`version` must be a release tag (`vX.Y.Z`, optionally with a `-suffix`). It downloads `cedr-<os>-<arch>.tar.gz` for the runner (`linux`, `macos` or `windows`; `x86_64` or `arm64`; `.zip` on Windows) and `SHA256SUMS` from the GitHub release over https, refuses an archive whose checksum does not match, then runs `cedr lint --format github` and `cedr verify --format github`. The job fails when either fails. It runs no package or server and never uses `--introspect`.

## Watcher commands

A watcher shells out to these. All print JSON on stdout; errors go to stderr with exit 2.

- `cedr latest <SOURCE>`: `{"source": "npm:x@1.0.0", "latest": "npm:x@1.1.0"}` from the registry's `latest` dist-tag.
- `cedr surface <SOURCE>`: a surface document. `SOURCE` is `npm:<pkg>@<ver>` or `file:<path-to.tgz>` (tests and fixtures).

## Wording

Never call a skill or server safe, trusted or approved. Report what changed since it was pinned.
