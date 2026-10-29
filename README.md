<div align="center">

<a href="https://cedr.dev"><img src="assets/banner.png" alt="cedr.dev: developer tools for humans and agents. Route the work to the right agent, review it across vendors, and pin what your agents load. One open-source CLI." width="100%"></a>

<a href="https://github.com/Sheernox/cedr/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/Sheernox/cedr/ci.yml?branch=main&style=for-the-badge&label=CI&labelColor=555555" alt="CI status"></a>
<a href="https://github.com/Sheernox/cedr/releases/latest"><img src="https://img.shields.io/github/v/release/Sheernox/cedr?style=for-the-badge&label=Release&color=2F7A57&labelColor=555555" alt="Latest release"></a>
<a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-2F7A57?style=for-the-badge&labelColor=555555" alt="License: Apache-2.0"></a>
<a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Built_with-Rust-7A6A55?style=for-the-badge&labelColor=555555" alt="Built with Rust"></a>
<a href="https://cedr.dev/docs"><img src="https://img.shields.io/badge/Docs-cedr.dev-2F7A57?style=for-the-badge&labelColor=555555" alt="Docs: cedr.dev"></a>

**[Website](https://cedr.dev)** · **[Docs](https://cedr.dev/docs)** · **[Install](#-install)** · **[Report a bug](https://github.com/Sheernox/cedr/issues)**

</div>

<br>

`cedr` is the command line tool behind [cedr.dev](https://cedr.dev). It picks which AI coding agent, model and effort should take each kind of work, merges reviews from several vendors into one pass or block decision, and pins the skills and MCP servers your agents load. It works with Claude Code, Codex, Cursor and Gemini, and any agent that speaks MCP can call it directly.

> [!NOTE]
> cedr is in preview. Commands and file formats may still change before 1.0.

## ✨ What it does

<table>
<tr>
<th width="33%">🧭 Route</th>
<th width="33%">⚖️ Council</th>
<th width="33%">🔒 Guard <sub>preview</sub></th>
</tr>
<tr valign="top">
<td>Picks the agent, model and effort for each kind of work from your <code>routes.toml</code>. Equally suited work goes to the plan with the most room left, and plans near their limits are skipped.</td>
<td>Starts one reviewer per vendor, merges their reports, groups findings that point at the same place and decides the gate. Any P0 or P1 blocks; too few reports is never a silent pass.</td>
<td>Lints your agent setup, pins every skill and MCP server in <code>cedr.lock</code>, and fails CI when a server's tools, descriptions or network hosts change after you pinned it.</td>
</tr>
</table>

## 📦 Install

Download the archive for your platform from the [latest release](https://github.com/Sheernox/cedr/releases/latest), unpack it and put `cedr` on your `PATH`.

| Platform | Archive |
| --- | --- |
| Linux x86_64 | `cedr-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `cedr-aarch64-unknown-linux-gnu.tar.gz` |
| macOS, Apple silicon | `cedr-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `cedr-x86_64-apple-darwin.tar.gz` |
| Windows x86_64 | `cedr-x86_64-pc-windows-msvc.zip` |

For example, on Linux x86_64:

```sh
curl -fsSL https://github.com/Sheernox/cedr/releases/latest/download/cedr-x86_64-unknown-linux-gnu.tar.gz | tar -xz
install -m 755 cedr-x86_64-unknown-linux-gnu/cedr ~/.local/bin/cedr
cedr --version
```

Linux builds need glibc 2.34 or newer (Ubuntu 22.04, Debian 12, RHEL 9 or later). Every release lists SHA-256 checksums in `SHA256SUMS` and carries signed build provenance you can check with `gh attestation verify <archive> --repo Sheernox/cedr`.

To build from source instead, use a current stable [Rust toolchain](https://rustup.rs):

```sh
cargo install --locked --git https://github.com/Sheernox/cedr cedr
```

## 🚀 Quick start

**1. Describe your plans and routes** in `~/.config/cedr/routes.toml`. The [Route page](https://cedr.dev/docs/route/) in the cedr.dev app can generate this file for you.

```toml
version = 1

[orchestrator]                  # the agent you work in; it keeps work no one else should take
harness = "claude"
model = "opus"

[harnesses.claude]
expect = ["five_hour", "seven_day"]
effort_option = "effort"

[harnesses.codex]
expect = ["primary"]
effort_option = "reasoningEffort"

[[readings]]                    # where plan usage comes from
kind = "claude-statusline"
harness = "claude"

[[readings]]
kind = "codex"
harness = "codex"

[routes."review.code"]
work = "Independent review"
role = "review"
candidates = [
  { target = "codex/sol", effort = "high", tier = 1 },
  { target = "claude/sonnet", effort = "high", tier = 1 },
  { target = "self" },
]
```

**2. Let cedr see your Claude Code plan usage** by making it your status line in `~/.claude/settings.json`. Add `-- <your command>` to keep an existing status line.

```json
{ "statusLine": { "type": "command", "command": "cedr route statusline" } }
```

**3. Pick, then record how it went.**

```console
$ cedr route pick review.code
[route review.code -> codex/gpt-6.1-sol high | balanced:31%<claude=64% | codex s22% w31%; claude s64% w48% | rules 770517e | pick p-979e77351990]
  codex/gpt-6.1-sol role=review

$ cedr route verdict p-979e77351990 accepted
```

`cedr route stats` then shows which model does each kind of work best.

## 🧰 Commands

| Command | What it does |
| --- | --- |
| `cedr route pick <route>` | Choose the agent, model and effort for a kind of work |
| `cedr route pools` | Show how much room each plan has left, and which ones cedr will use |
| `cedr route list` | List your routes and the models each one tries |
| `cedr route verdict <pick> <verdict>` | Record whether the work was accepted, reworked, redone or rejected |
| `cedr route stats` | Picks and verdicts per route and model |
| `cedr route sync` | Share history and routes with your cedr.dev organization |
| `cedr council plan <route>` | Name the reviewers to start, one per vendor, and the quorum |
| `cedr council merge <reports>...` | Merge their reports and decide the gate |
| `cedr mcp` | Serve the Route and Council tools over MCP on stdio |
| `cedr lint` | Find problems in a repo's agent setup |
| `cedr lock` | Pin every skill and MCP server in `cedr.lock` |
| `cedr verify` | Fail when the setup no longer matches `cedr.lock` (offline) |

Run `cedr <command> --help` for every option. Every config field, output shape and exit code is specified in the design docs for [Route](docs/route.md), [Council](docs/council.md) and [Guard](docs/design.md).

## 🔌 Use it from your agent

`cedr mcp` offers the same tools to any agent that speaks MCP:

```sh
claude mcp add cedr -- cedr mcp
codex mcp add cedr -- cedr mcp
```

## 🔐 Your keys stay yours

- cedr never reads your agents' credentials and never calls a vendor API for you. Plan usage comes from your agents' own status line and CLI, or from a feed you point it at.
- Picks, verdicts and stats stay on your machine. `cedr route sync` sends them to cedr.dev only when you add a `[sync]` section and a cedr.dev API key.
- `cedr lock` runs an MCP server only when you pass `--introspect`, and warns before it does.

## 🤝 Contributing

Bug reports and pull requests are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first, and report security issues privately as described in [SECURITY.md](SECURITY.md).

## 📄 License

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE) for included third-party work.

<div align="center">
<sub>Free and open source. Paid plans in the <a href="https://app.cedr.dev">cedr.dev app</a> add features such as routes shared across your machines and team (<a href="https://cedr.dev/docs/billing/plans/">plans</a>).<br>Made by <a href="https://sheernox.com">Sheernox</a>.</sub>
</div>
