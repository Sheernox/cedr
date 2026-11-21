# Cedr Compare: design contract

The contract for `cedr compare` and its MCP tools. A change to any shape below lands here first, then in the code.

## Scope

Compare measures whether an agent configuration, skill or model change improves outcomes. It runs a suite of tasks against a baseline and a candidate, lets tests decide each trial, and reports the counts. It reports facts: it never says a change is better, safe or approved.

Unlike Route and Council, Compare starts agents: a repeatable suite in CI has no orchestrator to do it. It runs the vendors' unmodified CLIs with the user's own credentials, and it holds, pools and proxies nothing.

Out of scope: a sandbox of its own and running trials in parallel.

## Trust

**A suite is code.** `setup` and `test` are shell commands and a `command` harness is any program, all run with the user's permissions. Run suites you trust, as you would a Makefile. `cedr compare plan` shows what a suite would do without starting anything.

Each trial works in a fresh clone in a temporary directory, under the permissions below. That protects the checkout, not the machine: a `workspace` agent is limited only by its vendor's own sandbox, and `setup`, `test` and `full` agents by nothing. Isolate with CI or a container when it matters.

The trial environment drops every variable named `CEDR_*` or `GIT_*`. `setup` and `test` also lose every variable whose name contains `API_KEY`, `ACCESS_KEY`, `TOKEN`, `SECRET`, `PASSWORD` or `CREDENTIAL`, since they run code the agent wrote.

## Suite

`cedr compare plan` and `run` take a suite path, or find the only `*.toml` in `.cedr/compare/`. Paths in a suite are relative to the repo root (the git top level of the suite file) and may not be absolute or contain `..`. Unknown keys are errors that name the file and key.

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
prompt = "Fix the failing parser test."   # or prompt_file = ".cedr/compare/fix-parser.md"
setup = "npm ci"            # optional: runs before the agent; failing leaves the trial unscored
test = "npm test -- parser" # exit 0 is a pass, any other exit a failure
test_files = ".cedr/compare/fix-parser/tests"   # optional
```

The two variants must differ in at least one of `harness`, `model`, `effort`, `permissions`, `command` or the commit their `ref` resolves to.

**Refs.** A configuration or skill change is a difference between two refs (`main` and a pull request's head, say); a model change is a difference in fields. Only committed work is measured. The suite, prompts and `test_files` always come from the working tree, so both variants face the same task whichever ref they start from.

**`test_files`.** An agent can edit the tests it is judged by. The contents of the `test_files` directory are copied onto the root of the trial's clone after the agent stops and before `test` runs (`<test_files>/tests/a.js` lands at `tests/a.js`), replacing whatever is there without following links, so tests kept there decide the trial as written. This narrows the problem without closing it: the agent can still change what those tests call.

### Harnesses

| Harness | Runs | `workspace` | `full` | Cost |
|---|---|---|---|---|
| `claude` | `claude -p --output-format json --no-session-persistence --setting-sources project,local --strict-mcp-config [--mcp-config <clone>/.mcp.json] --model M [--effort E] --max-budget-usd <left>` | `--permission-mode acceptEdits` plus the repo's own allow rules | `--permission-mode bypassPermissions` | `total_cost_usd` from its output |
| `codex` | `codex exec --json --ephemeral --skip-git-repo-check -m M [-c model_reasoning_effort=E] -` | `--sandbox workspace-write` | `--dangerously-bypass-approvals-and-sandbox` | Tokens from its output, priced by Monitor's table |
| `command` | The variant's `command`, an array with no shell; `{prompt_file}`, `{model}` and `{effort}` are replaced in each element | No effect | No effect | The JSON object on the last line of stdout, if any: `{"cost_usd", "input_tokens", "output_tokens", "cached_tokens", "models"}`, each optional |

```toml
[candidate]
harness = "command"
model = "my-model"
command = ["opencode", "run", "--model", "{model}", "--file", "{prompt_file}"]
```

- On Linux and macOS, each `claude` and `codex` trial gets a temporary shadow home: top-level entries of the user's home are linked, except user-level agent setup. Toolchains, git configuration, caches and other tools stay available through links. The shared hidden-root list is `.agents`, `.codex`, `.claude`, `.claude.json`, `.gemini`, `.cursor`, `.grok`, `.muse`, `.opencode`, `.copilot`, `Antigravity`, `AGENTS.md`, `AGENTS.override.md`, `CLAUDE.md`, `CLAUDE.local.md` and `GEMINI.md`. Dotless spellings are hidden too. `.config` and an `XDG_CONFIG_HOME` under the user's home are real directories whose children are linked with the same exclusions, creating filtered views of their ancestors as needed. Building these views never follows a symlink: a linked config directory or ancestor gets an empty view. An external or invalid `XDG_CONFIG_HOME` uses the filtered `.config` instead. Other XDG locations stay inherited, or use their normal defaults in the linked home, unless an in-home location contains a listed agent root (for example Muse skills in `.local/share/muse` or Copilot managed settings in `.cache/copilot`); that location then gets the same filtered view.
- Before a trial, the same tool-free host sign-in check as Bench runs in an empty working directory using the person's real agent home, preserving configured locations. Claude disables tools, customization, persistence and self-update, with a separate $0.25 maximum per check (one short reply; the cap only bounds a runaway); Codex uses `login status`. Host check costs are outside trial totals and the run budget. Only vendor code renews sign-ins in place before the trial links them. If a trial replaces its sign-in link with a regular file, Compare prints a one-line notice that the renewal was not saved and the person may need to sign in again. Cedr never writes a sign-in file or copies a replacement back. A legacy `bench_budget_usd` setting blocks Compare until it is renamed to `compare_budget_usd`, even when a new cap is also present.
- The agent config directory (`CODEX_HOME` or `CLAUDE_CONFIG_DIR`) is fresh and holds only a link to the user's sign-in file, when present (`.codex/auth.json` or `.claude/.credentials.json`). In-place vendor writes reach the real file through that link; replacements are never copied back. Inherited `CLAUDE_*` and `CODEX_*` overrides (except the `CLAUDE_CODE_OAUTH_TOKEN` and `CODEX_API_KEY` credentials), `CLAUDECODE` and shell startup overrides are removed. API-key credentials remain available. On macOS, Claude can still use the system sign-in keychain. On Windows, Compare keeps the real home and isolates only the agent config directory, recording `clean_home: false` for Codex because home-level user skills remain reachable; linking the sign-in file still requires symbolic-link permission, and a failure leaves the trial unscored with an actionable error.
- Compare measures the repo's own agent setup at the variant's commit: project instructions, skills, hooks, permissions and MCP configuration still load. Claude uses `--strict-mcp-config` to exclude account connectors, explicitly passing the clone's `.mcp.json` when present; it never uses `--safe-mode` or `--bare`. Codex explicitly trusts the trial clone so its `.codex/` configuration loads with the fresh home. It omits `--ignore-user-config`, which also suppresses project configuration in supported builds; the shadow home already excludes user configuration. This is configuration isolation, not a security sandbox.
- `command` is not isolated: it inherits the user's home, agent configuration and environment, apart from the trial-wide credential rules above. Its adapter must supply any isolation it needs.
- Trials leave no session history and do not appear in `cedr monitor`.
- Other agents (Cursor, Gemini, Copilot, opencode, Grok) run through `command`. They have no built-in adapter yet.
- `command` requires `command`, and the others refuse it. An unpriced variant (a `command` harness, or a `codex` model missing from the price table) needs `--allow-unpriced`, because the budget cannot see what it spends.

## Budget

Every run has a cap in USD at API prices, including on a plan where nothing is billed per call. The first of these sets it:

1. `--budget-usd N` on `compare run`.
2. `compare_budget_usd` in the CLI's settings (`$XDG_CONFIG_HOME/cedr/cli.toml`, set with `cedr compare budget`). The cedr.dev app's cap reaches this file through `cedr compare budget --pull`.
3. The default, `2.00`.

A suite can never set its own budget. The cap must be a finite number above 0. A settings file that cannot be read or does not parse, or a broken value in it, is an error rather than the default.

Before each trial, a run stops when what it has spent plus the most expensive trial so far would pass the cap. A run stopped this way is incomplete, with `stop` set to `"budget"`. The first trial always runs, since nothing is known about its cost yet.

What the cap guarantees depends on the variant. `compare plan` and `compare run` (on stderr, before the first trial) state it for each variant, and its `budget_enforcement` records it:

| `budget_enforcement` | Variants | Guarantee |
|---|---|---|
| `enforced` | `claude` | The harness also receives what is left of the cap as its own limit (`--max-budget-usd`) and stops itself there; how closely it holds that limit is the vendor's |
| `between_trials` | priced `codex` | Checked only between trials, so a trial can pass the cap, by at most what it spends before its timeout |
| `unpriced` | `command`, and `codex` with a model missing from the price table | The budget cannot see the spend at all; such a variant runs only with `--allow-unpriced` |

When a run's known spend ends above the cap, the result's `overrun` records by how much and which trial's cost took it over, and the text report says so.

A trial whose agent was killed at its timeout or by an interrupt, or an agent that reports no cost, has an unknown cost: it is left out of `spent_usd` and counted in `unpriced_trials`. The budget cannot see that spend, so the run stops there, incomplete, with `stop` set to `"unknown_cost"`, unless `--allow-unpriced` was passed.

Trials run in order: for each task, for each trial number, baseline then candidate. A run stopped early has therefore spent evenly on both variants.

`cedr compare budget [USD] [--unset] [--pull] [--org ORG] [--config PATH] [--format text|json]` shows or changes the cap. `--pull` reads `GET /v1/compare/settings?org=<org>` with `CEDR_API_KEY` (org and API settings as `cedr sync pull`) and stores the app's cap in the settings file; an org with no cap set removes the local one. JSON: `{"budget_usd": 2.0, "source": "default" | "settings"}`.

## Trials

A trial clones the repo at the variant's commit into a temporary directory (sharing its objects, with no remote), runs `setup`, runs the agent, copies `test_files`, runs `test`, and removes the directory. The prompt goes to a vendor CLI on stdin and to a `command` through `{prompt_file}`, a file outside the clone. Output from each step is capped at 16 MiB and is not kept.

The repo needs its full history, since a clone of a shallow one cannot reach every ref: `compare plan` and `compare run` refuse a shallow clone. In GitHub Actions, set `fetch-depth: 0` on `actions/checkout`.

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

### `cedr compare init [--name NAME]`

Writes a commented starter suite to `.cedr/compare/<name>.toml` (default `suite`) at the git top level of the working directory, to edit before use: its variants compare `main` with `HEAD` on the same model. Refuses to overwrite a file (exit 1). Prints the path, and on stderr the next steps: write a real task's prompt and test, make the candidate differ from the baseline (commit the change to measure, or give the candidate another model), then `cedr compare plan`.

### `cedr compare plan [SUITE] [--task ID]... [--trials N] [--format text|json]`

Validates the suite, resolves the refs and prints what a run would do, starting nothing. JSON: `{"suite", "sha256", "trials_each", "total_trials", "budget_usd", "budget_source", "variants": {"baseline": <variant>, "candidate": <variant>}, "tasks": [{"id", "setup", "test", "test_files"}], "warnings": []}`, where each variant has the Result's variant fields, including `budget_enforcement` ([Budget](#budget)), where a warning covers a harness that is not installed, `full` permissions, an unpriced variant and a `codex` model priced by its family's older version ([monitor.md](monitor.md), Pricing). Exit 0, or 2 on any error.

### `cedr compare run [SUITE] [--budget-usd N] [--task ID]... [--trials N] [--fail-on never|lower|separated] [--allow-unpriced] [--no-record] [--no-push] [--repo NAME] [--format text|json]`

Runs the suite. Progress goes to stderr, the report to stdout. Exit codes:

- `0`: complete, and the `--fail-on` condition (default `never`) did not hold.
- `1`: complete, and it held. `lower`: the candidate passed a smaller share of its scored trials, compared by count and not the rounded rates. `separated`: lower, and the two intervals do not overlap.
- `3`: incomplete, whatever the counts say. A partial result never passes.
- `2`: usage, config or I/O failure, including a refs problem found before any trial.

Unless `--no-record`, the result is saved to `$XDG_STATE_HOME/cedr/compare/<id>.json`, including when incomplete. `--repo` sets the recorded repo name; an invalid explicit name is a usage error before any trial. `--no-push` skips automatic sending for that run; a later `cedr compare push` can still send it. `--no-record` keeps a run from being saved or sent.

### `cedr compare history [--limit N] [--format text|json]`

Recorded runs, newest first, 20 by default. JSON: an array of `{"id", "suite", "started_at", "complete", "stop", "spent_usd", "baseline": {"passed", "scored"}, "candidate": {"passed", "scored"}}`.

### `cedr compare show <ID|PATH> [--fail-on ...] [--format text|json]`

Prints a recorded result, with the same report and exit codes as the run. Results are read as untrusted input: capped at 16 MiB and refused when `complete`, `summary` or `separated` do not follow from the trials.

Remove a result by deleting its file. Results moved from the former state directory remain readable, including their original `b-` ids; all new ids use `c-`.

## Result

```json
{
  "version": 1,
  "id": "c-20261101T193000Z-1a2b3c",
  "source": {"repo": "acme/web", "root_commit": "<root commit>", "ci": {"provider": "github", "pr": 12, "run_url": "https://github.com/acme/web/actions/runs/123"}},
  "suite": {"name": "agent-config", "sha256": "..."},
  "started_at": "2026-11-01T19:30:00Z",
  "finished_at": "2026-11-01T19:42:00Z",
  "budget_usd": 2.0,
  "spent_usd": 1.27,
  "unpriced_trials": 0,
  "overrun": null,
  "complete": true,
  "stop": null,
  "variants": {
    "baseline": {
      "harness": "claude", "clean_home": true, "harness_version": "2.5.0", "model": "claude-sonnet-5-5", "effort": "medium",
      "permissions": "workspace", "ref": "main", "commit": "<sha>", "config_sha256": "<sha256>",
      "resolved_models": ["claude-sonnet-5-5"], "budget_enforcement": "enforced"
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
- `clean_home` records whether the variant uses a clean home: true for `claude` and Unix `codex`, false for Windows `codex` and `command`. It defaults to false when reading older results. Plans show the same per-variant policy.
- `harness_version` is the CLI's own `--version` line, and null for `command`.
- `resolved_models` is what the harness reports having used; `codex` does not report one, so it repeats the requested model.
- `cost_usd` and `pass_rate` are null when nothing can fill them. `spent_usd` is the sum of known costs and `unpriced_trials` counts the rest.
- `budget_enforcement` is `enforced`, `between_trials` or `unpriced`, as in [Budget](#budget). It is null in results saved before it was recorded.
- `overrun` is null unless `spent_usd` ends above `budget_usd`. Then it is `{"usd": 0.31, "task": "fix-parser", "variant": "candidate", "trial": 2}`: `usd` is `spent_usd` minus `budget_usd`, and the rest names the first trial after which the known costs, summed in trial order, passed the cap. Spend in unpriced trials is not in it. It is null in results saved before it was recorded.
- `interval` is a 95% Wilson score interval over scored trials, null with none scored. `separated` is true when the variants' intervals do not overlap. Trials of one task are not independent samples of every task, so treat it as a floor: overlapping intervals mean this run does not tell the variants apart, not that they are equal.
- Results are saved locally and can be sent to an organization as described below. They hold prompts by reference only: the suite's digest and task ids, never the prompt text, agent output or paths.
- `source` is optional, defaults to null for older files, and is not part of the consistency checks. Its repo name is the first valid name from `--repo`, `GITHUB_REPOSITORY` on GitHub Actions, `CI_PROJECT_PATH` on GitLab CI (when `GITLAB_CI=true`), the last two path segments of a network `origin` remote (without `.git`), or the git top level's directory name. Only 1-128 characters of `A-Z a-z 0-9 . _ / -` are allowed. Only a non-file URL with a host or scp-style `host:path` can supply an origin name; local paths, `file://` and relative remotes fall through to the directory name. Derived names containing `.` or `..` segments are refused. Remote hosts, users and credentials are never kept. Invalid inferred names are ignored; with no valid name, `repo` is null.
- `source.root_commit` is the lexically first line of `git rev-list --max-parents=0 HEAD`, read at run time from the suite's git root, or null if unreadable. Older files without the field read it as null. Like the rest of `source`, it is never sent.
- `source.ci` is null except on GitHub Actions and GitLab CI. On GitHub Actions it needs `GITHUB_ACTIONS=true` and `GITHUB_SERVER_URL`, `GITHUB_REPOSITORY` and `GITHUB_RUN_ID` forming an HTTPS run URL: `$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID`. Its provider is `github`; `pr` is the number in `GITHUB_REF=refs/pull/<n>/merge`, or null for other refs. On GitLab CI it needs `GITLAB_CI=true` and `CI_SERVER_URL`, `CI_PROJECT_PATH` and `CI_JOB_URL`, with `CI_JOB_URL` an HTTPS URL under `$CI_SERVER_URL/$CI_PROJECT_PATH/`; that URL becomes `run_url`, the provider is `gitlab`, and `pr` is `CI_MERGE_REQUEST_IID` when it is a plain number, else null. No other environment values are read for provenance, and a URL with credentials, a query or a fragment (or any non-HTTPS URL) drops `source.ci` to null instead of being stored.

## Push

`cedr compare push [ID]... [--all] [--org ORG] [--config PATH] [--repo NAME] [--dry-run] [--format text|json]` sends saved results to an organization. Org and API settings follow `cedr sync push`, using `CEDR_API_KEY` or the saved API key.

With no ids and no `--all`, it sends only unsent results whose saved `source.repo` equals the current repo's name ignoring ASCII case, and whose saved `source.root_commit` equals the current repo's root commit, oldest first. Both name and root must match: a fork with the same root but a different name is left alone. The name is resolved exactly as for `compare run`: `--repo`, `GITHUB_REPOSITORY` on Actions, `CI_PROJECT_PATH` on GitLab CI, a network `origin`, then the git top level's directory name. Without a valid name and a readable root commit, for example outside a git repo, it errors saying to run it inside the repo, name ids or pass `--all`. Results with no saved repo or root commit, from another repo, or in a file that cannot be read or parsed are skipped, not failures; this also leaves older result files alone. `--repo` selects matching names in this mode and still requires the current repo's root commit, since a name alone can collide across organizations. `--repo` never supplies a missing saved name in bare mode. Ids and `--all` can reach results without a saved root commit, and report unreadable or unparseable files as failures.

`--all` explicitly sends every unsent result on this machine. `--all --repo` is a usage error: one name must never be stamped onto many results. Explicit ids send those results again, even if previously sent; the server treats repeats as duplicates. With ids, `--repo` overrides each result's `source.repo`, including supplying a missing name. In explicit-id or `--all` mode, a result with no repo name fails with a message to pass ids with `--repo`, while the others continue.

Each body goes to `POST <sync url>/compare/runs` with bearer authentication:

```json
{"org":"org-...","cli":"<CLI version>","repo":"acme/web","machine":{"id":"...","name":"..."},"ci":{"provider":"github","pr":12,"run_url":"https://github.com/acme/web/actions/runs/123"},"result": <the saved result without source>}
```

`ci` can be null. It carries GitHub Actions provenance only: a result saved on GitLab CI keeps its `source.ci` locally and is sent with `ci` null. The machine id and name are the same as Route uses, with control characters stripped from the name before clipping it to 64 characters. Sending includes the result, repo name, machine id and name, PR number and run URL. Prompts, agent output and paths are never sent. Model names that look like filesystem paths (starting with `/`, `~/`, `./`, `../`, `.\`, `..\`, `file:`, a UNC prefix `\\` or a drive letter such as `C:\`) send only their final segment after trimming trailing separators, including entries in `resolved_models` and trial `models`. The outgoing copy strips control characters from `suite.name`, `model`, `effort`, `ref`, `harness_version` and model arrays. Empty model-array entries are dropped; empty `effort` and `harness_version` become null. Required `suite.name`, `model` and `ref` fields that become empty send `unknown`. These strings are clipped by characters: `suite.name` 128, model names 256, `effort` 128, `ref` 256 and `harness_version` 512. Saved files and reports are unchanged. This is an organization upload, with no new telemetry fields. Bodies over 256 KiB are refused locally, naming the result and the limit; the larger result stays saved locally. A 200 reply is `{"ok":true,"id":"<result id>","duplicate":false}` (or `duplicate:true` on a repeat). Errors follow Sync's rules, including 400 `bad_request`, 403 `not_a_member`, 413 `too_large` and 503 when the organization lock is busy. Compare alone retries a 503 up to twice more, waiting the `Retry-After` delay in seconds capped at 5, or 1 second when the header is missing or invalid, then reports the failure as usual.

Successful ids are appended to `$XDG_STATE_HOME/cedr/compare-pushed-<org>.txt`, one per line, after each success. Losing this file only causes safe repeat sends. A failure on one result does not stop the others, except when no key or org is available. Exit 0 when all sent or none were due, 2 otherwise. `--dry-run` prints the bodies it would send and sends nothing, without updating the sent file.

Text output has one line per failure, followed by `pushed N to <org>` and the duplicate count when any. When results were skipped, it also prints `N saved results from other repos left alone; name them or pass --all`. JSON output is `{"org":"org-...","pushed":["ids"],"duplicates":["ids"],"failed":[{"id":"...","error":"..."}],"skipped":0}`. Duplicates also count as pushed.

### Automatic sending

After saving a run, Compare sends that one result (including incomplete runs) when the config resolved from the suite's git top level (rather than the working directory) has `[sync]` and `auto` is not false. It never sends older runs automatically. A machine with `[sync]` already configured starts sending after upgrading. Use `--no-push` to skip automatic sending for that run; a later `cedr compare push` can still send it. `[sync] auto = false` stops automatic sending, including Route's automatic sync. `--no-record` keeps a run from being saved or sent.

With no config or no `[sync]`, nothing is printed or sent. Sending runs in process and is waited for, bounded by the client's 15-second timeout per attempt, plus at most two retry waits of 5 seconds each for a 503. Success prints one line to stderr. Broken config, a missing key, network or server errors print one line saying the result was not sent and that `cedr compare push` can send it later. Sending never changes the run's exit code or its stdout report, including JSON.

## Text output

```text
compare agent-config: complete, $1.27 of $2.00 (c-20261101T193000Z-1a2b3c)
              passed  rate   95%          median  cost
  baseline    2/3     67%    21%-94%      41s     $0.12
  candidate   3/3     100%   44%-100%     38s     $0.09
  fix-parser  baseline 2/3, candidate 3/3
pass rate +33 points; the intervals overlap, so this run does not separate them
```

An incomplete run says `incomplete (budget)`, `incomplete (unknown cost)`, `incomplete (interrupted)` or `incomplete (N unscored)` on its first line. A run that passed its cap adds a line after it: `spent $0.31 over the $2.00 budget; fix-parser candidate trial 2 took it over`.

## MCP tools

| Tool | Arguments | Returns |
|---|---|---|
| `compare_plan` | `suite?`, `task?`, `trials?` | The plan JSON |
| `compare_history` | `limit?` | History rows |
| `compare_show` | `id` | The result JSON |

There is no `compare_run`: it spends money and can take hours, so an agent runs it through the CLI, where the budget and its flags are in plain view.

## GitHub Action

`action.yml` gains `compare` (a suite path; empty, the default, skips Compare), `compare-budget-usd` (else the app's cap when `push-org` and `CEDR_API_KEY` are set, else the default), `compare-fail-on` (default `never`) and `compare-allow-unpriced` (default `false`). The workflow checks out the full history (`fetch-depth: 0`) and installs the vendor CLIs and sets their API keys in the environment; in CI use API keys, not a signed-in plan. A failure or an incomplete run fails the action once the other checks have run. The report goes to the job summary. The action passes `--no-push` to the run, then, when `push-org` and `CEDR_API_KEY` are set, runs `cedr compare push --org "$INPUT_ORG"` even after a failed or incomplete run. This selects only the current repo's results, so other repos' results on a shared runner are left alone. A failed send produces a warning and never changes the Compare gate or job status. Do not point it at a suite from an untrusted pull request: a suite is code.
