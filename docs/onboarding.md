# First-run setup and daily status

## First-run setup

`cedr init [--yes] [--dry-run] [--format text|json]` plans setup for the local machine. It uses the ordinary Route config search and creates [starter routes](route.md#starter-routes) only when that search finds no file. It offers to register the `cedr` MCP server with installed Claude, Codex and Grok CLIs, using the same commands shown by [doctor](doctor.md). An unreadable registration is a failed step so it cannot replace an existing entry. Other harnesses have no registration step. It also offers the [Monitor push timer](monitor.md), where supported. The timer is useful once you connect an organization.

The command shows the plan before acting. In a terminal, confirm each proposed change with a yes or no. `--yes` confirms every step. `--dry-run` previews the plan without applying it. Without a terminal or `--yes`, the command prints a notice and previews the plan. Existing setup is kept. After applying or declining the proposed steps, the command prints a summary from doctor and where to go next with Monitor, Guard lint, Sync and Route.

JSON stdout contains `{"steps":[{"id","description","command","status","error"}]}`. Each command is an array of arguments or null for a step handled directly by cedr. Status is `planned`, `applied`, `skipped`, `failed` or `already`. Error is a message for a failed step, otherwise null. Plan and prompt text, the doctor summary and next steps go to stderr in JSON mode.

One failed step leaves the remaining steps available. Exit 0 means setup or preview completed, 1 means a step failed during setup, and 2 means usage or a setup command error. Doctor findings do not change the exit code. The setup command runs in the CLI, where the person setting up the machine can confirm changes.

## Daily status

`cedr status [--format text|json]` shows local usage and repository state. It updates the local Monitor store from agent logs and cached plan readings. It makes no network requests, runs no vendor probes and writes no configuration.

- **Limits.** The window with the highest known usage for each plan in Monitor, excluding windows whose known reset has passed. These are last recorded values.
- **Spend.** API-equivalent cost in USD from local midnight and over the last seven days, plus any models Monitor could not price.
- **Pending picks.** Up to five latest recorded Route picks without a verdict. Run `cedr route verdict <id> <verdict>` to record one.
- **Sync and Guard.** The Sync check and Guard verify summary for the containing Git checkout or the current directory outside Git. Sync requires `.cedr/sync.toml`. Guard accepts `.cedr/guard.lock` or a legacy `cedr.lock`.

JSON stdout is an object with these fields. Each field is null when its source is absent. Existing Route history with no pending picks yields an empty array.

| Field | Value when available |
| --- | --- |
| `limits` | Array of [Monitor limit rows](monitor.md), one per plan |
| `spend` | `{todayUsd,last7DaysUsd,unpricedToday,unpricedLast7Days}` with numeric costs and arrays of model ids |
| `pendingPicks` | Array of `{id,route,target,at}` with the recorded timestamp |
| `sync` | `{state,findings}` with `clean` or `drift` and a finding count |
| `guard` | `{state,findings,error}` with `clean`, `drift` or `error`, a finding count when verification completes, and an error message on failure |

Text omits unavailable sections. Exit 0 means the report was produced, including when it finds repo drift or a Guard error. Usage or report collection errors exit 2. The MCP `status` tool accepts an optional repository `path` and returns the same JSON.
