# Contributing to cedr

Thanks for helping improve cedr. Bug reports, fixes and documentation improvements are all welcome.

## Reporting a bug

Open an [issue](https://github.com/Sheernox/cedr/issues) with:

- what you ran (the full `cedr` command) and what you expected,
- what happened instead, including any error output,
- `cedr --version` and your operating system.

Remove API keys, tokens and private paths from anything you paste. Security problems go through [SECURITY.md](SECURITY.md), never a public issue.

## Proposing a change

For anything larger than a small fix, open an issue first so we can agree on the approach before you write code.

1. Fork the repository and create a branch from `main`.
2. Make your change, with a focused test for any behaviour you add or fix.
3. Run the checks below; all of them must pass.
4. Open a pull request that says what changed and why.

This repository is published from the cedr.dev source tree. When we accept a pull request, we apply it there and it comes back here with the next release, credited to you.

## Development

cedr is a Rust workspace:

| Path | What |
| --- | --- |
| `crates/cedr` | The `cedr` binary and its MCP server |
| `crates/cedr-route` | Route and Council: config, plan readings, picks, history, sync and review merging |
| `crates/cedr-core` | Guard: discovery, lint rules, the lock file and MCP server surfaces |
| `fixtures/` | Sample repos, packages and configs used by the tests |

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The tests never use the network; tests that need it are marked `#[ignore]`.

## Design contracts

[docs/route.md](docs/route.md), [docs/council.md](docs/council.md) and [docs/design.md](docs/design.md) define every config field, output shape and exit code. A change to any of them belongs in the same pull request as the code, with the contract updated first.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org): `fix(route): keep the pick when every plan is out`.

## Licence

By contributing, you agree that your contribution is licensed under the [Apache License 2.0](LICENSE).
