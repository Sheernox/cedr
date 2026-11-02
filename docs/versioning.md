# Versioning

cedr follows [Semantic Versioning 2.0](https://semver.org/spec/v2.0.0.html). One version covers the whole CLI and all its products. This page defines what that version promises, which SemVer leaves to each project.

## Public API

A version number makes promises about:

- commands, flags, exit codes and JSON output (`--format json` and other machine formats),
- config, lock and surface file formats,
- the shapes `cedr monitor push` and other push commands send,
- GitHub Action inputs,
- MCP tools.

The [contracts](.) define each of these. Human-readable text output is not API and may change in any release, so script against JSON. The Rust crates are internal (`publish = false`).

Adding a field to JSON output is not a breaking change, so ignore fields you don't recognise.

## Choosing the bump

Before 1.0:

| Bump | What it carries |
| --- | --- |
| Minor (`0.X.0`) | Any new command, flag, config key, contract field, supported agent or outbound request, and any breaking change |
| Patch (`0.0.X`) | Fixes only: behaviour moves toward what the contract already says, with nothing new to learn or allow |

From 1.0, a breaking change takes a major, new capability a minor, and a fix a patch.

When in doubt, it's a minor. Every minor and major needs a maintainer's approval before release, so the doubt reaches a person.

## Breaking changes

A change is breaking when a script, CI job or config that follows the contract stops working or changes meaning. Examples: removing or renaming a command, flag, JSON field or Action input; changing a field's type or a flag's meaning; returning a different exit code for the same condition.

- **Deprecate first when possible.** The old form keeps working and warns on stderr for at least one later minor. There is no minimum time, so that minor can arrive soon after the deprecation.
- **Otherwise,** ship it in a minor with a **Breaking** changelog section and an upgrade step. From 1.0, a major.
- **Security fixes** may skip deprecation. They are still labelled by what they change, and the changelog names the break.

## File formats

Files carry a `version`. A new CLI reads every older file, so upgrading never strands a file.

The reverse isn't promised: cedr rejects keys it doesn't know, so an older CLI may refuse a file written for a newer one. Additive changes are therefore minors with a changelog note. A change that invalidates old files bumps the file's `version` and counts as breaking.

## Releases

- Every release is a plain `vX.Y.Z` tag. cedr publishes no pre-releases, so nothing is staged: a release is live for everyone as soon as it is tagged.
- A pushed tag never moves and is never deleted, so there are no floating tags such as `v0`.
- A bad release is fixed forward with a new patch, reverting if needed, and the bad version's changelog entry gets a note.
- Security fixes go into the latest release only ([SECURITY.md](../SECURITY.md)).
- Which release is 1.0 is the maintainers' call.

## Staying on a version

Self-update installs the newest stable release, including across major versions, within a day of its release ([update.md](update.md)). A breaking change can therefore reach an unattended machine that soon.

- On a machine, `cedr update --auto off` stops this, and `cedr update --version X.Y.Z` installs a chosen release, which is also the way back.
- In CI, cedr never updates itself. Pin the Action to an exact tag, `Sheernox/cedr@vX.Y.Z`. With a commit SHA in `uses:`, also set the `version` input, or the Action downloads the latest release.
