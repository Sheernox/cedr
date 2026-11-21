# First-run setup and daily status

## First-run setup

`cedr init [--yes] [--dry-run] [--format text|json]` plans setup for the local machine. It uses the ordinary Route config search and creates [starter routes](route.md#starter-routes) only when that search finds no file. It offers to register the `cedr` MCP server with installed Claude, Codex and Grok CLIs, using the same commands shown by [doctor](doctor.md). An unreadable registration is a failed step so it cannot replace an existing entry. Other harnesses have no registration step. It also offers the [Monitor push timer](monitor.md), where supported. The timer is useful once you connect an organization.

The command shows the plan before acting. In a terminal, confirm each proposed change with a yes or no. `--yes` confirms every step. `--dry-run` previews the plan without applying it. Without a terminal or `--yes`, the command prints a notice and previews the plan. Existing setup is kept. After applying or declining the proposed steps, the command prints a summary from doctor and where to go next with Monitor, Guard lint, Sync and Route.

JSON stdout contains `{"steps":[{"id","description","command","status","error"}]}`. Each command is an array of arguments or null for a step handled directly by cedr. Status is `planned`, `applied`, `skipped`, `failed` or `already`. Error is a message for a failed step, otherwise null. Plan and prompt text, the doctor summary and next steps go to stderr in JSON mode.

One failed step leaves the remaining steps available. Exit 0 means setup or preview completed, 1 means a step failed during setup, and 2 means usage or a setup command error. Doctor findings do not change the exit code. The setup command runs in the CLI, where the person setting up the machine can confirm changes.
