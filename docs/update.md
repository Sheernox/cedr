# cedr update: design contract

`cedr update` replaces the running `cedr` with the latest release, and cedr does the same by itself once a day unless turned off. Releases are the GitHub releases of [Sheernox/cedr](https://github.com/Sheernox/cedr/releases): one archive per platform (`cedr-<os>-<arch>.tar.gz`, `.zip` on Windows, holding `cedr-<os>-<arch>/cedr[.exe]`) and a `SHA256SUMS` file.

## Command

```
cedr update [--check] [--version X.Y.Z] [--format text|json]
cedr update --auto on|off
```

- With no flags: look up the latest release; when it is newer than the running version, download this platform's archive, check its SHA-256 against `SHA256SUMS`, unpack the binary next to the running one, run it with `--version` to confirm it starts and reports the expected version, then swap it in. Prints `cedr: updated 0.4.2 -> 0.5.0` or `cedr: 0.5.0 is the latest`.
- `--check`: report only; nothing is downloaded.
- `--version X.Y.Z`: install that release, newer or older (the only way to go back).
- `--auto on|off`: set `auto_update` in `cli.toml` ([telemetry.md](telemetry.md), Settings) and print the new state; no lookup.
- JSON: `{"current": "0.4.2", "latest": "0.5.0", "updated": bool, "auto": bool, "install": "archive" | "cargo" | "read-only", "fix": string | null}`.
- Exit 0 when up to date or updated (or `--check` ran), 2 on any failure. On failure the running binary is untouched.

Only stable releases count: a tag with a pre-release part (`v0.6.0-rc.1`) is never picked as latest.

## Install kinds

- `archive`: the binary's directory is writable and it is not a cargo install. Updated in place.
- `cargo`: the binary sits in `$CARGO_HOME/bin` (default `~/.cargo/bin`, `%USERPROFILE%\.cargo\bin` on Windows). Never replaced; `fix` and the text output give `cargo install --locked --git https://github.com/Sheernox/cedr cedr`.
- `read-only`: the directory is not writable (a system install). Never replaced; `fix` says to rerun with the needed permissions or reinstall to a writable directory.

## Swap

The new binary is written to a temporary file in the same directory, made executable, and checked as above. On Unix it is renamed over the running binary (atomic). On Windows the running `cedr.exe` is renamed to `.cedr-old-<random>.exe` (allowed while it runs) and the new one moved into place; the next run deletes leftover `.cedr-old-*` files. If the final move fails, the old binary is moved back.

## Automatic updates

On by default. A command (any but `update`, `completions` and the hidden background commands) starts `cedr update --background` (hidden) when the last check began more than 24 hours ago (`$XDG_STATE_HOME/cedr/update-check`). It never runs when:

- `auto_update = false`, or `CEDR_NO_AUTO_UPDATE` is set to anything but empty, `0`, `false` or `off`;
- in CI (as [telemetry.md](telemetry.md) detects it);
- the install kind is `cargo` or `read-only`.

Every install takes the same lock (`cedr update` fails with a message while another holds it). The background run updates as `cedr update` does, but only to a stable release newer than the binary on disk (which a manual update may have replaced since it started), prints nothing, and records the result in `$XDG_STATE_HOME/cedr/update.json`: `{"from": "0.4.2", "to": "0.5.0", "at": "..."}` after an update, `{"error": "...", "at": "..."}` after a failure. The next command prints `cedr: updated to 0.5.0 (from 0.4.2); see https://github.com/Sheernox/cedr/releases` once and clears it. Failures are shown by `cedr doctor` (`cli.version`), never on every command.

## Network

Release lookups use `https://api.github.com/repos/Sheernox/cedr/releases/latest` (or `/releases/tags/vX.Y.Z`), and downloads `https://github.com/Sheernox/cedr/releases/download/vX.Y.Z/<asset>`, with a 15-second timeout for lookups, 120 seconds for downloads, archives capped at 64 MiB, every redirect https. `CEDR_RELEASE_URL` replaces `https://github.com/Sheernox/cedr` for tests (then lookups go to `<url>/releases/latest` and downloads to `<url>/releases/download/...`); it must be https or loopback http.
