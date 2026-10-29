# AGENTS.md - cedr

The open-source `cedr` CLI from [cedr.dev](https://cedr.dev): Route picks the AI coding agent, model and effort for each kind of work, Council merges multi-vendor reviews into one gate, and Guard (preview) lints, pins and verifies the skills and MCP servers agents load. Human contributors: see [CONTRIBUTING.md](CONTRIBUTING.md).

## How this repo works
- It is published from the cedr.dev source tree. Accepted pull requests are applied there and return with the next release, so keep each change small and self-contained.
- Product docs live at [cedr.dev/docs](https://cedr.dev/docs); `cedr <command> --help` is the CLI reference.

## Layout
- `crates/cedr/`: the binary, including the MCP server (`cedr mcp`).
- `crates/cedr-route/`: Route and Council (config, readings and built-in readers, pool, pick, history, sync, council).
- `crates/cedr-core/`: Guard (discovery, lint, lock, surface extraction, diff).
- `fixtures/`: sample repos, npm tarballs and configs for tests.

## Commands

```sh
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Guardrails
- Tests never need the network; online tests are `#[ignore]`.
- Never execute a package or MCP server except under `cedr lock --introspect`, which warns first.
- Never call a skill or server safe, trusted or approved in output; report what changed since it was pinned.
- Never print or log an API key, token or URL query string; error messages name a URL by scheme and host only.
- Code ported from another project keeps its licence notice in `NOTICE` and a header naming the source file and commit.
