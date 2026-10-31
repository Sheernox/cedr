# AGENTS.md - cedr

The open-source `cedr` CLI from [cedr.dev](https://cedr.dev): Route picks the AI coding agent, model and effort for each kind of work, Council merges multi-vendor reviews into one gate, Sync compiles one agent setup into every agent's files, Monitor reports every agent's usage, spend and plan limits, and Guard lints, pins and verifies the skills and MCP servers agents load. Human contributors: see [CONTRIBUTING.md](CONTRIBUTING.md).

## How this repo works
- It is published from the cedr.dev source tree. Accepted pull requests are applied there and return with the next release, so keep each change small and self-contained.
- Frozen contracts: [docs/route.md](docs/route.md) for `cedr route` and `cedr mcp`, [docs/council.md](docs/council.md) for `cedr council`, [docs/design.md](docs/design.md) for Guard, [docs/sync.md](docs/sync.md) for `cedr sync`, [docs/monitor.md](docs/monitor.md) for `cedr monitor` and [docs/doctor.md](docs/doctor.md) for `cedr doctor`. Change the contract first, then the code.
- Product docs live at [cedr.dev/docs](https://cedr.dev/docs); `cedr <command> --help` is the CLI reference.

## Layout
- `crates/cedr/`: the binary, including the MCP server (`cedr mcp`).
- `crates/cedr-route/`: Route and Council (config, readings and built-in readers, pool, pick, history, sync, council).
- `crates/cedr-core/`: Guard (discovery, lint, lock, surface extraction, diff, outdated).
- `crates/cedr-sync/`: Sync (sources, per-agent emitters, merge, lock, check, import, org rules).
- `crates/cedr-monitor/`: Monitor (collectors, local store, pricing, reports, limits, push, schedule).
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
