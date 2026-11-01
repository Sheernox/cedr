# Contributing to cedr

Thanks for helping improve cedr. Bug reports, fixes and documentation improvements are all welcome.

cedr is developed in public and is coded and maintained by AI agents, directed by its maintainers. We read every report, but we have limited review capacity, and opening a pull request does not oblige us to review or merge it.

## Reporting a bug

Open an [issue](https://github.com/Sheernox/cedr/issues) with:

- what you ran (the full `cedr` command) and what you expected,
- what happened instead, including any error output,
- `cedr --version` and your operating system.

Remove API keys, tokens and private paths from anything you paste. Security problems go through [SECURITY.md](SECURITY.md), never a public issue.

## Proposing a change

Issues come first. Bug reports and ideas are the most useful things you can send.

- **Focused fixes** can go straight to a pull request. Explain the defect and why the fix is right.
- **Features and behaviour changes** need an issue where a maintainer has agreed the direction and scope. Link it in your pull request.

For a pull request:

1. Fork the repository and create a branch from `main`, and leave "Allow edits by maintainers" on.
2. Make your change, with a focused test for any behaviour you add or fix.
3. Run the checks below; all of them must pass.
4. Open a pull request that says what changed and why.

Maintainers' agents review each pull request as untrusted input, and two independent reviews from different vendors must pass before it lands. We rebase it onto `main`, keeping you as the author. We may also reimplement your idea instead, with credit to you. A release that includes outside code waits for a maintainer's approval.

## Development

cedr is a Rust workspace:

| Path | What |
| --- | --- |
| `crates/cedr` | The `cedr` binary and its MCP server |
| `crates/cedr-route` | Route and Council: config, plan readings, picks, history, sync and review merging |
| `crates/cedr-core` | Guard: discovery, lint rules, the lock file and MCP server surfaces |
| `crates/cedr-sync` | Sync: sources, per-agent emitters, merge, check and import |
| `crates/cedr-monitor` | Monitor: sources, local store, pricing, reports, limits and push |
| `fixtures/` | Sample repos, packages and configs used by the tests |

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The tests never use the network; tests that need it are marked `#[ignore]`.

## Design contracts

[docs/route.md](docs/route.md), [docs/council.md](docs/council.md), [docs/design.md](docs/design.md), [docs/sync.md](docs/sync.md), [docs/monitor.md](docs/monitor.md) and [docs/doctor.md](docs/doctor.md) define every config field, output shape and exit code. A change to any of them belongs in the same pull request as the code, with the contract updated first.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org): `fix(route): keep the pick when every plan is out`.

## Licence

By contributing, you agree that your contribution is licensed under the [Apache License 2.0](LICENSE).
