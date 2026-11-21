# Guard: status and known gaps

[design.md](design.md) is the contract; this page records what works today and what does not yet.

## Status

Guard is in beta. It discovers repo-level instructions, skills and MCP server settings for the [supported clients](design.md#discovery). Its lock and server surfaces live in `.cedr/`. The GitHub Action checks pull requests, and `cedr guard push` sends a summary of repo pins and findings to the cedr.dev app.

Lint checks the repo setup, and verify detects local drift from the lock offline. Outdated compares locked npm servers with newer versions from the registry. Opt in to remote snapshots with `cedr guard lock --remote`, then use `cedr guard remote` to compare their current tool lists. The local text comparisons show both versions of a changed tool description.

## Known gaps

- Skill hashes do not include executable permission bits. A chmod alone does not change verification, and file modes differ across platforms.
- Verify reports legacy untyped skill pins as `CV006` warnings. An untyped hash can match a regular file and a link with the same target text. From the repo root, run `cedr guard lock` and review `git diff -- .cedr/guard.lock .cedr/surfaces/` before committing the new pins. Use `cedr guard verify --require-typed-pins` or the Action input `require-typed-pins: true` to require typed pins in CI.
- URL paths remain in source identities. Put credentials in headers or environment variables, not URL paths. Re-lock and rotate any credentials previously written into lockfiles or their Git history.
- `cedr guard latest` and `cedr guard outdated` follow the registry's `latest` dist-tag. Newer versions under other tags do not count.
- PyPI (`uvx`), OCI digests and git-sourced skills have no upstream version check yet.
- Add nonstandard skill roots to `skills` in `.cedr/guard.toml` for discovery. Guard skips skill directories that link outside the repo.
- `cedr guard remote` reads only public servers over Streamable HTTP. Servers that need a login or use the legacy SSE transport are reported, not read, and Cedr installs no timer for it.
- Guard scans the repo root, so it shows less than Grok loads. Grok also reads user-level files (`~/.grok/`, and by default `~/.claude/` and `~/.cursor/` rules, skills, servers and hooks), a `.grok/config.toml` in each directory below the repo root, and `.grok/rules/*.md`. It loads `.mcp.json` and `.cursor/mcp.json` too; Guard lists those under `claude` and `cursor`, so a server named in both is two entries. `grok inspect --json` can name the wrong config file as a server's source, so Guard parses the files instead.
- Dropped SkillSpector patterns (lookarounds, possessive quantifiers, non-regex analyzers) are listed in the header of [patterns.rs](../crates/cedr-core/src/patterns.rs).
