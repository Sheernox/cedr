# Cedr Bench: design contract

The contract for `cedr bench` and its MCP tools. A change to any shape below lands here first, then in the code.

## Scope

Bench measures whether an agent configuration, skill or model change improves outcomes. It runs a suite of tasks against a baseline and a candidate, lets tests decide each trial, and reports the counts. It reports facts: it never says a change is better, safe or approved.

Unlike Route and Council, Bench starts agents: a repeatable suite in CI has no orchestrator to do it. It runs the vendors' unmodified CLIs with the user's own credentials, and it holds, pools and proxies nothing.

Out of scope: a sandbox of its own and running trials in parallel.

## Trust

**A suite is code.** `setup` and `test` are shell commands and a `command` harness is any program, all run with the user's permissions. Run suites you trust, as you would a Makefile. `cedr bench plan` shows what a suite would do without starting anything.

Each trial works in a fresh clone in a temporary directory, under the permissions below. That protects the checkout, not the machine: a `workspace` agent is limited only by its vendor's own sandbox, and `setup`, `test` and `full` agents by nothing. Isolate with CI or a container when it matters.

The trial environment drops every variable named `CEDR_*` or `GIT_*`. `setup` and `test` also lose every variable whose name contains `API_KEY`, `ACCESS_KEY`, `TOKEN`, `SECRET`, `PASSWORD` or `CREDENTIAL`, since they run code the agent wrote.

## Suite

`cedr bench plan` and `run` take a suite path, or find the only `*.toml` in `.cedr/bench/`. Paths in a suite are relative to the repo root (the git top level of the suite file) and may not be absolute or contain `..`. Unknown keys are errors that name the file and key.

```toml
version = 1
trials = 3                  # per task and variant; default 3, 1 to 100
timeout_seconds = 600       # for each of setup, agent and test; default 600

[baseline]
harness = "claude"          # claude | codex | command
model = "claude-sonnet-5-5"
effort = "medium"           # optional: passed to the harness as is
ref = "main"                # optional: git ref the trial starts from; default HEAD
permissions = "workspace"   # optional: workspace (default) | full

[candidate]
harness = "claude"
model = "claude-sonnet-5-5"
ref = "HEAD"

[[tasks]]
id = "fix-parser"           # letters, digits, `.`, `_` and `-`; unique
prompt = "Fix the failing parser test."   # or prompt_file = ".cedr/bench/fix-parser.md"
setup = "npm ci"            # optional: runs before the agent; failing leaves the trial unscored
test = "npm test -- parser" # exit 0 is a pass, any other exit a failure
test_files = ".cedr/bench/fix-parser/tests"   # optional
```

The two variants must differ in at least one of `harness`, `model`, `effort`, `permissions`, `command` or the commit their `ref` resolves to.

**Refs.** A configuration or skill change is a difference between two refs (`main` and a pull request's head, say); a model change is a difference in fields. Only committed work is measured. The suite, prompts and `test_files` always come from the working tree, so both variants face the same task whichever ref they start from.

**`test_files`.** An agent can edit the tests it is judged by. The contents of the `test_files` directory are copied onto the root of the trial's clone after the agent stops and before `test` runs (`<test_files>/tests/a.js` lands at `tests/a.js`), replacing whatever is there without following links, so tests kept there decide the trial as written. This narrows the problem without closing it: the agent can still change what those tests call.

### Harnesses

| Harness | Runs | `workspace` | `full` | Cost |
|---|---|---|---|---|
| `claude` | `claude -p --output-format json --no-session-persistence --setting-sources project,local --model M [--effort E] --max-budget-usd <left>` | `--permission-mode acceptEdits` plus the repo's own allow rules | `--permission-mode bypassPermissions` | `total_cost_usd` from its output |
| `codex` | `codex exec --json --ephemeral --ignore-user-config --skip-git-repo-check -m M [-c model_reasoning_effort=E] -` | `--sandbox workspace-write` | `--dangerously-bypass-approvals-and-sandbox` | Tokens from its output, priced by Monitor's table |
| `command` | The variant's `command`, an array with no shell; `{prompt_file}`, `{model}` and `{effort}` are replaced in each element | No effect | No effect | The JSON object on the last line of stdout, if any: `{"cost_usd", "input_tokens", "output_tokens", "cached_tokens", "models"}`, each optional |

```toml
[candidate]
harness = "command"
model = "my-model"
command = ["opencode", "run", "--model", "{model}", "--file", "{prompt_file}"]
```

- The vendor CLIs ignore the user's own settings, so a trial depends on the repo and the variant rather than on whose machine it ran on. Credentials still come from the user's sign-in or the environment.
- Trials leave no session history and do not appear in `cedr monitor`.
- Other agents (Cursor, Gemini, Copilot, opencode, Grok) run through `command`. They have no built-in adapter yet.
- `command` requires `command`, and the others refuse it. An unpriced variant (a `command` harness, or a `codex` model missing from the price table) needs `--allow-unpriced`, because the budget cannot see what it spends.

## Budget

Every run has a cap in USD at API prices, including on a plan where nothing is billed per call. The first of these sets it:

1. `--budget-usd N` on `bench run`.
2. `bench_budget_usd` in the CLI's settings (`$XDG_CONFIG_HOME/cedr/cli.toml`, set with `cedr bench budget`). The cedr.dev app's cap reaches this file through `cedr bench budget --pull`.
3. The default, `2.00`.

A suite can never set its own budget. The cap must be a finite number above 0. A settings file that cannot be read or does not parse, or a broken value in it, is an error rather than the default.

Before each trial, a run stops when what it has spent plus the most expensive trial so far would pass the cap. `claude` also receives what is left as its own limit. Other harnesses are only checked between trials, so a trial can overrun the cap, by at most what it spends before its timeout. A run stopped this way is incomplete, with `stop` set to `"budget"`.

A trial whose agent was killed at its timeout or by an interrupt, or an agent that reports no cost, has an unknown cost: it is left out of `spent_usd` and counted in `unpriced_trials`. The budget cannot see that spend, so the run stops there, incomplete, with `stop` set to `"unknown_cost"`, unless `--allow-unpriced` was passed.

Trials run in order: for each task, for each trial number, baseline then candidate. A run stopped early has therefore spent evenly on both variants.

`cedr bench budget [USD] [--unset] [--pull] [--org ORG] [--config PATH] [--format text|json]` shows or changes the cap. `--pull` reads `GET /v1/bench/settings?org=<org>` with `CEDR_API_KEY` (org and API settings as `cedr sync pull`) and stores the app's cap in the settings file; an org with no cap set removes the local one. JSON: `{"budget_usd": 2.0, "source": "default" | "settings"}`.

## Trials

A trial clones the repo at the variant's commit into a temporary directory (sharing its objects, with no remote), runs `setup`, runs the agent, copies `test_files`, runs `test`, and removes the directory. The prompt goes to a vendor CLI on stdin and to a `command` through `{prompt_file}`, a file outside the clone. Output from each step is capped at 16 MiB and is not kept.

The repo needs its full history, since a clone of a shallow one cannot reach every ref: `bench plan` and `bench run` refuse a shallow clone. In GitHub Actions, set `fetch-depth: 0` on `actions/checkout`.

Each step runs on its own, and what it leaves running is killed when it ends on Linux and macOS, so a server started in `setup` is gone by `test`: start it in the `test` command. On Windows, a process a step leaves behind keeps running.

| Status | Meaning | Scored |
|---|---|---|
| `passed` | `test` exited 0 | Yes |
| `failed` | `test` exited non-zero | Yes |
| `timeout` | The agent or `test` ran past the timeout | Yes, as not passed |
| `setup_failed` | `setup` failed, timed out or was interrupted, or the agent could not start | No |
| `agent_error` | The agent exited with an error or reported one, or left its clone unable to run `test` | No |
| `budget` | The vendor CLI stopped at its limit | No |
| `interrupted` | The run was interrupted during the agent or `test` | No |

An unscored trial, or a run stopped by budget, makes the run incomplete.

On Linux and macOS, Ctrl-C (or SIGTERM or SIGHUP) kills the running step, removes its clone and ends the run with what it measured: saved as usual, incomplete, with `stop` set to `"interrupted"`. A signal ignored when cedr started stays ignored, so `nohup` keeps a run going. On Windows, Ctrl-C ends cedr and the step together, and that run is not saved.

## Commands

### `cedr bench init [--name NAME]`

Writes a commented starter suite to `.cedr/bench/<name>.toml` (default `suite`) at the git top level of the working directory, to edit before use: its variants compare `main` with `HEAD` on the same model. Refuses to overwrite a file (exit 1). Prints the path, and on stderr the next steps: write a real task's prompt and test, make the candidate differ from the baseline (commit the change to measure, or give the candidate another model), then `cedr bench plan`.

### `cedr bench plan [SUITE] [--task ID]... [--trials N] [--format text|json]`

Validates the suite, resolves the refs and prints what a run would do, starting nothing. JSON: `{"suite", "sha256", "trials_each", "total_trials", "budget_usd", "budget_source", "variants": {"baseline": <variant>, "candidate": <variant>}, "tasks": [{"id", "setup", "test", "test_files"}], "warnings": []}`, where a warning covers a harness that is not installed, `full` permissions and an unpriced variant. Exit 0, or 2 on any error.

### `cedr bench run [SUITE] [--budget-usd N] [--task ID]... [--trials N] [--fail-on never|lower|separated] [--allow-unpriced] [--no-record] [--no-push] [--repo NAME] [--format text|json]`

Runs the suite. Progress goes to stderr, the report to stdout. Exit codes:

- `0`: complete, and the `--fail-on` condition (default `never`) did not hold.
- `1`: complete, and it held. `lower`: the candidate passed a smaller share of its scored trials, compared by count and not the rounded rates. `separated`: lower, and the two intervals do not overlap.
- `3`: incomplete, whatever the counts say. A partial result never passes.
- `2`: usage, config or I/O failure, including a refs problem found before any trial.

Unless `--no-record`, the result is saved to `$XDG_STATE_HOME/cedr/bench/<id>.json`, including when incomplete. `--repo` sets the recorded repo name; an invalid explicit name is a usage error before any trial. `--no-push` skips automatic sending for that run; a later `cedr bench push` can still send it. `--no-record` keeps a run from being saved or sent.

### `cedr bench history [--limit N] [--format text|json]`

Recorded runs, newest first, 20 by default. JSON: an array of `{"id", "suite", "started_at", "complete", "stop", "spent_usd", "baseline": {"passed", "scored"}, "candidate": {"passed", "scored"}}`.

### `cedr bench show <ID|PATH> [--fail-on ...] [--format text|json]`

Prints a recorded result, with the same report and exit codes as the run. Results are read as untrusted input: capped at 16 MiB and refused when `complete`, `summary` or `separated` do not follow from the trials.

Remove a result by deleting its file.

## Result

```json
{
  "version": 1,
  "id": "b-20261101T193000Z-1a2b3c",
  "source": {"repo": "acme/web", "root_commit": "<root commit>", "ci": {"provider": "github", "pr": 12, "run_url": "https://github.com/acme/web/actions/runs/123"}},
  "suite": {"name": "agent-config", "sha256": "..."},
  "started_at": "2026-11-01T19:30:00Z",
  "finished_at": "2026-11-01T19:42:00Z",
  "budget_usd": 2.0,
  "spent_usd": 1.27,
  "unpriced_trials": 0,
  "complete": true,
  "stop": null,
  "variants": {
    "baseline": {
      "harness": "claude", "harness_version": "2.5.0", "model": "claude-sonnet-5-5", "effort": "medium",
      "permissions": "workspace", "ref": "main", "commit": "<sha>", "config_sha256": "<sha256>",
      "resolved_models": ["claude-sonnet-5-5"]
    },
    "candidate": {}
  },
  "trials": [
    {"task": "fix-parser", "variant": "baseline", "trial": 1, "status": "passed", "test_exit": 0, "duration_ms": 41200,
     "input_tokens": 1200, "output_tokens": 300, "cached_tokens": 4000, "cost_usd": 0.042, "models": ["claude-sonnet-5-5"]}
  ],
  "summary": {
    "baseline": {"scored": 3, "passed": 2, "pass_rate": 0.667, "interval": [0.208, 0.939], "unscored": 0,
                 "duration_ms_median": 41200, "cost_usd": 0.12, "tokens": 5500},
    "candidate": {}
  },
  "tasks": [{"id": "fix-parser", "baseline": {"scored": 3, "passed": 2}, "candidate": {"scored": 3, "passed": 3}}],
  "pass_rate_delta": 0.333,
  "separated": false
}
```

- `config_sha256` digests the agent files at the variant's commit: every path Guard and Sync treat as agent setup (`AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `.mcp.json`, `opencode.json`, `.agents/`, `.claude/`, `.codex/`, `.cursor/`, `.gemini/`, `.opencode/`, `.grok/`, `.vscode/mcp.json`, `.github/copilot-instructions.md`, `.github/mcp.json`, `.github/skills/`). It shows whether two runs measured the same setup.
- `harness_version` is the CLI's own `--version` line, and null for `command`.
- `resolved_models` is what the harness reports having used; `codex` does not report one, so it repeats the requested model.
- `cost_usd` and `pass_rate` are null when nothing can fill them. `spent_usd` is the sum of known costs and `unpriced_trials` counts the rest.
- `interval` is a 95% Wilson score interval over scored trials, null with none scored. `separated` is true when the variants' intervals do not overlap. Trials of one task are not independent samples of every task, so treat it as a floor: overlapping intervals mean this run does not tell the variants apart, not that they are equal.
- Results are saved locally and can be sent to an organization as described below. They hold prompts by reference only: the suite's digest and task ids, never the prompt text, agent output or paths.
- `source` is optional, defaults to null for older files, and is not part of the consistency checks. Its repo name is the first valid name from `--repo`, `GITHUB_REPOSITORY` on GitHub Actions, the last two path segments of a network `origin` remote (without `.git`), or the git top level's directory name. Only 1-128 characters of `A-Z a-z 0-9 . _ / -` are allowed. Only a non-file URL with a host or scp-style `host:path` can supply an origin name; local paths, `file://` and relative remotes fall through to the directory name. Derived names containing `.` or `..` segments are refused. Remote hosts, users and credentials are never kept. Invalid inferred names are ignored; with no valid name, `repo` is null.
- `source.root_commit` is the lexically first line of `git rev-list --max-parents=0 HEAD`, read at run time from the suite's git root, or null if unreadable. Older files without the field read it as null. Like the rest of `source`, it is never sent.
- `source.ci` is null unless `GITHUB_ACTIONS=true` and `GITHUB_SERVER_URL`, `GITHUB_REPOSITORY` and `GITHUB_RUN_ID` are set and form an HTTPS run URL: `$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID`. Its provider is `github`; `pr` is the number in `GITHUB_REF=refs/pull/<n>/merge`, or null for other refs.

## Push

`cedr bench push [ID]... [--all] [--org ORG] [--config PATH] [--repo NAME] [--dry-run] [--format text|json]` sends saved results to an organization. Org and API settings follow `cedr sync push`, using `CEDR_API_KEY` or the saved API key.

With no ids and no `--all`, it sends only unsent results whose saved `source.repo` equals the current repo's name ignoring ASCII case, and whose saved `source.root_commit` equals the current repo's root commit, oldest first. Both name and root must match: a fork with the same root but a different name is left alone. The name is resolved exactly as for `bench run`: `--repo`, `GITHUB_REPOSITORY` on Actions, a network `origin`, then the git top level's directory name. Without a valid name and a readable root commit, for example outside a git repo, it errors saying to run it inside the repo, name ids or pass `--all`. Results with no saved repo or root commit, from another repo, or in a file that cannot be read or parsed are skipped, not failures; this also leaves older result files alone. `--repo` selects matching names in this mode and still requires the current repo's root commit, since a name alone can collide across organizations. `--repo` never supplies a missing saved name in bare mode. Ids and `--all` can reach results without a saved root commit, and report unreadable or unparseable files as failures.

`--all` explicitly sends every unsent result on this machine. `--all --repo` is a usage error: one name must never be stamped onto many results. Explicit ids send those results again, even if previously sent; the server treats repeats as duplicates. With ids, `--repo` overrides each result's `source.repo`, including supplying a missing name. In explicit-id or `--all` mode, a result with no repo name fails with a message to pass ids with `--repo`, while the others continue.

Each body goes to `POST <sync url>/bench/runs` with bearer authentication:

```json
{"org":"org-...","cli":"<CLI version>","repo":"acme/web","machine":{"id":"...","name":"..."},"ci":{"provider":"github","pr":12,"run_url":"https://github.com/acme/web/actions/runs/123"},"result": <the saved result without source>}
```

`ci` can be null. The machine id and name are the same as Route uses, with control characters stripped from the name before clipping it to 64 characters. Sending includes the result, repo name, machine id and name, PR number and run URL. Prompts, agent output and paths are never sent. Model names that look like filesystem paths (starting with `/`, `~/`, `./`, `../`, `.\`, `..\`, `file:`, a UNC prefix `\\` or a drive letter such as `C:\`) send only their final segment after trimming trailing separators, including entries in `resolved_models` and trial `models`. The outgoing copy strips control characters from `suite.name`, `model`, `effort`, `ref`, `harness_version` and model arrays. Empty model-array entries are dropped; empty `effort` and `harness_version` become null. Required `suite.name`, `model` and `ref` fields that become empty send `unknown`. These strings are clipped by characters: `suite.name` 128, model names 256, `effort` 128, `ref` 256 and `harness_version` 512. Saved files and reports are unchanged. This is an organization upload, with no new telemetry fields. Bodies over 256 KiB are refused locally, naming the result and the limit; the larger result stays saved locally. A 200 reply is `{"ok":true,"id":"<result id>","duplicate":false}` (or `duplicate:true` on a repeat). Errors follow Sync's rules, including 400 `bad_request`, 403 `not_a_member`, 413 `too_large` and 503 when the organization lock is busy. Bench alone retries a 503 up to twice more, waiting the `Retry-After` delay in seconds capped at 5, or 1 second when the header is missing or invalid, then reports the failure as usual.

Successful ids are appended to `$XDG_STATE_HOME/cedr/bench-pushed-<org>.txt`, one per line, after each success. Losing this file only causes safe repeat sends. A failure on one result does not stop the others, except when no key or org is available. Exit 0 when all sent or none were due, 2 otherwise. `--dry-run` prints the bodies it would send and sends nothing, without updating the sent file.

Text output has one line per failure, followed by `pushed N to <org>` and the duplicate count when any. When results were skipped, it also prints `N saved results from other repos left alone; name them or pass --all`. JSON output is `{"org":"org-...","pushed":["ids"],"duplicates":["ids"],"failed":[{"id":"...","error":"..."}],"skipped":0}`. Duplicates also count as pushed.

### Automatic sending

After saving a run, Bench sends that one result (including incomplete runs) when the config resolved from the suite's git top level (rather than the working directory) has `[sync]` and `auto` is not false. It never sends older runs automatically. A machine with `[sync]` already configured starts sending after upgrading. Use `--no-push` to skip automatic sending for that run; a later `cedr bench push` can still send it. `[sync] auto = false` stops automatic sending, including Route's automatic sync. `--no-record` keeps a run from being saved or sent.

With no config or no `[sync]`, nothing is printed or sent. Sending runs in process and is waited for, bounded by the client's 15-second timeout per attempt, plus at most two retry waits of 5 seconds each for a 503. Success prints one line to stderr. Broken config, a missing key, network or server errors print one line saying the result was not sent and that `cedr bench push` can send it later. Sending never changes the run's exit code or its stdout report, including JSON.

## Text output

```text
bench agent-config: complete, $1.27 of $2.00 (b-20261101T193000Z-1a2b3c)
              passed  rate   95%          median  cost
  baseline    2/3     67%    21%-94%      41s     $0.12
  candidate   3/3     100%   44%-100%     38s     $0.09
  fix-parser  baseline 2/3, candidate 3/3
pass rate +33 points; the intervals overlap, so this run does not separate them
```

An incomplete run says `incomplete (budget)`, `incomplete (unknown cost)`, `incomplete (interrupted)` or `incomplete (N unscored)` on its first line.

## MCP tools

| Tool | Arguments | Returns |
|---|---|---|
| `bench_plan` | `suite?`, `task?`, `trials?` | The plan JSON |
| `bench_history` | `limit?` | History rows |
| `bench_show` | `id` | The result JSON |

There is no `bench_run`: it spends money and can take hours, so an agent runs it through the CLI, where the budget and its flags are in plain view.

## GitHub Action

`action.yml` gains `bench` (a suite path; empty, the default, skips Bench), `bench-budget-usd` (else the app's cap when `push-org` and `CEDR_API_KEY` are set, else the default), `bench-fail-on` (default `never`) and `bench-allow-unpriced` (default `false`). The workflow checks out the full history (`fetch-depth: 0`) and installs the vendor CLIs and sets their API keys in the environment; in CI use API keys, not a signed-in plan. A failure or an incomplete run fails the action once the other checks have run. The report goes to the job summary. The action passes `--no-push` to the run, then, when `push-org` and `CEDR_API_KEY` are set, runs `cedr bench push --org "$INPUT_ORG"` even after a failed or incomplete run. This selects only the current repo's results, so other repos' results on a shared runner are left alone. A failed send produces a warning and never changes the Bench gate or job status. Do not point it at a suite from an untrusted pull request: a suite is code.
