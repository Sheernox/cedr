# Guard: status and known gaps

[design.md](design.md) is the contract; this page records what works today and what does not yet.

## Status

Guard is in beta, like every cedr product. Its files live in `.cedr/`, the GitHub Action checks pull requests, and `cedr guard push` sends each repo's pins and findings to the cedr.dev app.

`cedr guard lint`, `lock`, `verify`, `surface`, `latest` and `diff` work as design.md specifies, for Claude Code, Codex, Cursor and Gemini configs. Static extraction finds every tool in the official MCP servers tried: `@modelcontextprotocol/server-everything@2.1.0` (24), `server-filesystem` 2.0.0 and 2025.8.21 (14 each) and `server-memory@2.0.1` (9).

## Known gaps

- `cedr guard latest` follows the registry's `latest` dist-tag. For `server-filesystem` that is 2.1.0, even though later calendar versions (2026.x) exist under other tags.
- PyPI (`uvx`), OCI digests and git-sourced skills have no upstream version check yet.
- A skill symlink that escapes the root stops the whole run with exit 2 instead of reporting a finding.
- `description_changed` reports only the new text. Showing the old text alongside needs a contract change to the diff shape.
- Skills outside the standard paths are not discovered.
- Dropped SkillSpector patterns (lookarounds, possessive quantifiers, non-regex analyzers) are listed in the header of `crates/cedr-core/src/patterns.rs`.
