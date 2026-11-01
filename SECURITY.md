# Security policy

## Reporting a vulnerability

Please report security problems privately through GitHub's [private vulnerability reporting](https://github.com/Sheernox/cedr/security/advisories/new). Do not open a public issue.

Include the cedr version, the command or configuration involved, and steps to reproduce. We will acknowledge your report, keep you updated while we investigate, and credit you in the advisory unless you prefer otherwise.

## Scope

cedr runs on developer machines and in CI next to AI coding agents, so we especially want to hear about:

- a way for a shared `routes.toml`, a readings feed or a merged review report to run a command, read a file or reach a host it should not,
- an API key or other secret appearing in output, logs, history or error messages,
- `cedr guard verify` passing when a pinned skill or MCP server has changed,
- `cedr guard lock` executing a package or server without `--introspect`.

## Supported versions

cedr is in beta; fixes go into the latest release only.
