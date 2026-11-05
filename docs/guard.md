# Guard: status and known gaps

[design.md](design.md) is the contract; this page records what works today and what does not yet.

## Status

Guard is in beta, like every cedr product. Its files live in `.cedr/`, the GitHub Action checks pull requests, and `cedr guard push` sends each repo's pins and findings to the cedr.dev app.

`cedr guard lint`, `lock`, `verify`, `remote`, `surface`, `latest` and `diff` work as design.md specifies, for Claude, Codex, Cursor, Gemini and Grok configs. Static extraction finds every tool in the official MCP servers tried: `@modelcontextprotocol/server-everything@2.1.0` (24), `server-filesystem` 2.0.0 and 2025.8.21 (14 each) and `server-memory@2.0.1` (9).

## Known gaps

- Skill hashes do not include executable permission bits. A chmod alone does not change verification, and file modes differ across platforms.
- Pre-upgrade symlink pins lack a type tag. Re-lock skills containing symlinks; until then, replacing a link with a regular file containing exactly its target text can still match the old pin.
- URL paths remain in source identities. Put credentials in headers or environment variables, not URL paths. Re-lock and rotate any credentials previously written into lockfiles or their Git history.

- `cedr guard latest` follows the registry's `latest` dist-tag. For `server-filesystem` that is 2.1.0, even though later calendar versions (2026.x) exist under other tags.
- PyPI (`uvx`), OCI digests and git-sourced skills have no upstream version check yet.
- `description_changed` reports only the new text. Showing the old text alongside needs a contract change to the diff shape.
- Skills outside the standard paths are not discovered.
- `cedr guard remote` reads only public servers over Streamable HTTP. Servers that need a login or use the legacy SSE transport are reported, not read, and Cedr installs no timer for it.
- Guard scans the repo root, so it shows less than Grok loads. Grok also reads user-level files (`~/.grok/`, and by default `~/.claude/` and `~/.cursor/` rules, skills, servers and hooks), a `.grok/config.toml` in each directory below the repo root, and `.grok/rules/*.md`. It loads `.mcp.json` and `.cursor/mcp.json` too; Guard lists those under `claude` and `cursor`, so a server named in both is two entries. `grok inspect --json` can name the wrong config file as a server's source, so Guard parses the files instead.
- Dropped SkillSpector patterns (lookarounds, possessive quantifiers, non-regex analyzers) are listed in the header of `crates/cedr-core/src/patterns.rs`.
