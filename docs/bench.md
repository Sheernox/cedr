# Cedr Bench: design contract

The stable contract for the standard benchmark and its read-only MCP tools. Compare measures changes on a user suite; Bench measures one configuration on a frozen public edition.

## What a run is

One run measures one **configuration**: a harness (the agent CLI), a model and a reasoning effort. The same model through two agents is two configurations. A run works through every task of one suite **edition**, in the edition's order, once.

```
cedr bench run --harness claude --model claude-sonnet-5-5 --effort high
```

## The suite

The suite lives in its own public repository, `Sheernox/cedr-bench`, so a task's workspace holds the task and nothing else. An edition is frozen and numbered. A later edition replaces some tasks and keeps some as anchors; scores are only compared within an edition.

```
edition.toml              number, goal_usd, image digest, task order, probe references
tasks/<id>/task.toml      id, kind (code | visual), language, timeout_seconds, artifacts
tasks/<id>/prompt.md      the prompt, verbatim
tasks/<id>/repo/          the starting state, copied into a fresh workspace
graders/<id>/grade        the hidden grader; never copied into the workspace
```

- **Code tasks** (ten in edition 1): short multi-file repository work. Each grader runs four named check groups and prints `{"checks":[{"id":"...","passed":true}]}`. A group passes only when every assertion in it passes. Task score is groups passed over four; a task is solved at four of four.
- **Visual tasks** (two in edition 1): the agent writes one static file (a product page as `index.html`, a chart as `chart.svg`). The grader runs fixed gates (file rules, no script, required text, data drawn to scale) and renders screenshots with a pinned browser. Visual tasks do not count toward the score: the screenshots are published for people to judge. Font choices and annotation limits belong to the edition's task prompts and graders, not the CLI.
- Graders run after the agent has exited, on a copy of the workspace, with no network. The agent never sees a grader.

## Score

`score` is the mean task score over the code tasks, times 100. A task the run never reached counts as zero. `solved` is the number of code tasks at four of four. There is no judge model anywhere.

The code score is a correctness gate. The headline comparisons are cost per solved code task (the sum of code-task list costs divided by code tasks solved) and time per task, derived from the existing task records. Visual costs and probe costs remain part of run spend but not the cost-per-solved numerator.

## Cost: a goal, not a limit

- The edition sets `goal_usd` (10.00 in edition 1), at API list prices from the Monitor price table, including for runs on a subscription.
- A run that spends more is recorded with `over_goal: true`. It is never failed, truncated or dropped for cost.
- There is no spending stop by default. `--stop-at-usd N` (or the `bench_stop_usd` setting) adds one: it is checked before each task, and a run it ends is incomplete with `stop: "user_stop"`. With no stop set, `run` prints one warning before it starts saying so and naming the flag.
- A trial with unknown cost follows Compare's rule: the run stops as `unknown_cost` unless `--allow-unpriced`.

## A clean environment

A result must depend on the configuration, not on whose machine ran it. Three layers:

1. **Clean agent home.** Every trial runs with an agent home that holds the sign-in and nothing else: no user rules, skills, hooks, memory, MCP servers or account connectors. Each harness has its own recipe (for example a fresh `CODEX_HOME` for Codex; `--safe-mode --strict-mcp-config` for Claude). Credentials are linked or mounted, never copied or printed, and the user's sign-in must still work after a run. See #52 for what leaks today.
2. **The pinned image.** By default the agent and the graders run in the container image the edition names by digest: fixed toolchains, fonts and browser. The agent's network is limited to its model provider's hosts; graders get none. `--local` runs on the host instead, with the clean home only.
3. **The probe.** Before the tasks, the runner sends a fixed fingerprint prompt in an empty workspace and records its input tokens (fresh, cache read and cache write together), followed by a separate tool-readiness prompt whose files are verified. Extra rules or tools show up as extra tokens. The separately published `probes/edition-<n>.toml` carries a reference per harness version, model and effort; the result records `match`, `mismatch` or `no_reference`.

The result's `environment.mode` is `container` or `local`. The derived `eligible` field requires a complete run, a pinned container, a clean home, allowlist networking, a matching (or explicitly set) fingerprint, passing tool readiness, and a verified vendor adapter.

## Harnesses

| Harness | Runs |
|---|---|
| `claude` | Claude Code CLI, headless |
| `codex` | Codex CLI, headless |
| `muse` | Muse Code, `muse exec --json` |
| `grok` | Grok Build, `grok -p --output-format json` |
| `gemini` | Gemini CLI, headless |
| `antigravity` | T3 Code's bundled Antigravity harness, over the Agent Client Protocol |
| `command` | Any program, as in Compare; never eligible for listing |

Each adapter reports tokens split into fresh input, cache read, cache write, output and reasoning (where the agent reports them), the agent's version and the models it resolved. An adapter that cannot report tokens leaves the trial unpriced. An effort level a model does not offer is an error, never substituted.

## Result

Saved to `$XDG_STATE_HOME/cedr/bench/<id>.json`, with artifacts beside it in `<id>/artifacts/<task>/`.

```json
{
  "version": 1, "kind": "bench", "id": "b-20261113T050000Z-1a2b3c",
  "edition": {"number": 1, "suite_sha256": "...", "commit": "...", "image": "sha256:..."},
  "config": {"harness": "claude", "harness_version": "...", "model": "...", "effort": "high", "resolved_models": ["..."]},
  "environment": {"mode": "container", "clean_home": true, "network": "allowlist", "pinned": true, "agent_image": "sha256:...", "verified": ["engine>=28", "isolated-internal-bridge", "host-gateway-unreachable", "live-host-port-unreachable", "external-ip-unreachable", "external-dns-blocked"], "limits": "memory=2g,pids=256,tmp=1g,swap=0", "os": "linux", "arch": "x86_64", "cli": "..."},
  "probe": {"input_tokens": 31926, "tools": 27, "reference": 31926, "status": "match", "tools_ok": true},
  "billing": "plan",
  "started_at": "...", "finished_at": "...",
  "goal_usd": 10.0, "stop_usd": null, "spent_usd": 4.12, "over_goal": false, "unpriced_trials": 0,
  "complete": true, "stop": null, "eligible": true,
  "score": 72.5, "solved": 6, "code_tasks": 10,
  "tasks": [
    {"id": "py-ledger-rounding", "kind": "code", "status": "graded",
     "checks": [{"id": "reported-case", "passed": true}], "score": 0.75, "solved": false,
     "duration_ms": 61200, "turns": 9,
     "input_tokens": 0, "cache_read_tokens": 0, "cache_write_tokens": 0, "output_tokens": 0, "reasoning_tokens": null,
     "cost_usd": 0.31, "patch_sha256": "...", "gates": [], "artifacts": []}
  ]
}
```

A visual task has `kind: "visual"`, `gates: [{"id", "passed", "detail"}]` and `artifacts: [{"path", "sha256", "bytes"}]` (the file the agent wrote and each screenshot), and no `checks`. Task `status` is one of `graded`, `timeout`, `agent_error`, `setup_failed`, `interrupted`, `not_reached`. Results hold no prompt text, agent output or paths.

## Commands

- `cedr bench run --harness H --model M [--effort E] [--edition N | --suite PATH] [--local] [--stop-at-usd N] [--task ID]... [--allow-unpriced] [--format text|json]`
- `cedr bench probe --harness H --model M [--effort E] [--local]`: the probe alone.
- `cedr bench history`, `cedr bench show <ID>`: as in Compare.
- `cedr bench export <ID>... --out DIR`: writes `results.json` (the results, unchanged) and copies each screenshot to `DIR/shots/<id>/<task>/<name>.png`, for publishing.

Exit codes follow Compare: 0 complete, 3 incomplete, 2 usage or I/O. There is no `--fail-on`: Bench reports, it does not gate.

MCP gains `bench_history` and `bench_show`. There is no `bench_run`, for the reason Compare has none.

## Not in the first version

Community submissions, a spending stop by default, a parallel mode, and any ranking of the visual tasks.


## Suite schema and integrity

All TOML objects deny unknown fields. Task ids use ASCII letters, digits, `_`, `-` and `.` (never `.` or `..`). Paths are relative, contain no parent components, and must not traverse symlinks. Suite files and task repositories contain regular files only; `.git` is excluded. Task metadata additionally accepts descriptive `summary` and `difficulty` strings.

`probes/edition-<n>.toml` contains `[[references]]` entries: `harness`, `harness_version`, `model`, optional `effort`, `input_tokens`, optional `tools`. Identity includes effort. Duplicate identities are errors. Reference comparison uses total fresh plus cache-read plus cache-write input, with an inclusive tolerance of 16. Missing reported fields remain null. A reference does not prove byte-identical context: small injections below the tolerance can pass.

The probe has two independent prompts in separate empty workspaces and fresh clean homes. Part 1, the fingerprint, is exactly `Reply with the single word OK.` It needs one model request and no tools. Compare its total fresh plus cache-read plus cache-write input tokens with the reference within 16 tokens for every adapter, including those that only report run totals. Part 2, readiness, is exactly: `Run exactly one shell command: printf 'cedr-bench-command\n' > command.txt. Then write probe.txt containing exactly cedr-bench-probe followed by a newline. Do not run any other shell commands.` The runner verifies both files after trimming trailing whitespace (leading whitespace and other extra text fail), recording `tools_ok`. Each marker must be a regular UTF-8 file of at most 4 KiB. The shell command and separate file-write instruction exercise both capabilities. Readiness tokens are never compared. A passing probe requires `match` (or `reference`) and `tools_ok`. `no_reference` is usable for diagnosis but is not a passing probe. `--set-reference` records status `reference` and prints an entry for that probe file only when no reference exists for the identity. An existing reference always wins, including a mismatch; the runner never edits the reference file.

Probe records hold fingerprint `input_tokens`, optional readiness `tools`, `reference`, `status`, `tools_ok`, `fingerprint_cost_usd`, `tools_cost_usd`, and combined `cost_usd` (null if either cost is unknown). Run spend includes each known part even if the other is unknown; unpriced counts include each unknown part. Optional per-request usage must not replace the uniform total-token fingerprint comparison.

`--suite PATH` reads a local tree. Otherwise edition defaults to 1. `--edition N` clones `https://github.com/Sheernox/cedr-bench.git` at `edition-N` into `$XDG_CACHE_HOME/cedr/bench/edition-N`; the first fetch records the commit and SHA-256 alongside it. Subsequent loads recompute and verify both, refusing drift. This is trust on first fetch, not a publisher signature. Hashing sorts relative UTF-8 names and hashes length-prefixed names, executable bits and file bytes, excluding `.git` and `probes/`. A local suite also records that digest and its commit when available. With `--suite`, references come from that local tree. Otherwise the runner fetches only the probe file from the suite repository default branch and caches it separately. Offline runs reuse that copy; without a copy they record `no_reference`. Every loaded suite is snapshotted before execution so later source edits cannot change prompts or graders in flight. Grader support files participate in the digest.

`--task ID` is diagnostic: all edition tasks remain in the result, unselected tasks are `not_reached`, the run is incomplete and ineligible. Each reached task starts from a new one-commit repository. Patch digests cover the final binary git diff, including untracked files, against that initial commit.

## Execution and credentials

Container mode uses Docker CLI subprocesses only. The edition's `image` must include `@sha256:<64 hex>`. `--build-image` builds `image/Dockerfile`; an unpinned edition is allowed for this option, records `pinned: false`, and cannot qualify for listing. The actual image id and agent layer id are recorded. Agent layers are local, keyed by base digest, harness and exact version, and never pushed. Static self-contained vendor binaries may be copied into a layer; other agents use their vendor installer at the requested version. The executable version is checked before use, and Claude automatic updates are disabled for the invocation. In containers, its terminal result ends the invocation and stops remaining helpers before grading; a background shell tool cannot keep a completed response alive until the task timeout.

The agent runs non-root at `/workspace` with home `/home/bench`, a read-only root, dropped capabilities, no-new-privileges and writable temporary mounts. Only provider credentials, proxy settings and the fixed tool environment are allowed. Provider hostnames are exact allowlist entries, with no wildcards. A CONNECT-only proxy on an internal Docker network with an isolated bridge gateway is the sole egress route (Docker Engine 28 or newer is required); the proxy alone also joins an external network. Only port 443 is accepted; loopback, private, link-local and multicast destinations are refused after DNS resolution. The agent has neither an external network nor the Docker socket. Before inference, the runner checks the engine version and network inspection, then actively tests that a task-network container cannot connect to the host bridge gateway, a listening host port or an outside IP, or resolve an outside host. Failure is fatal; `environment.verified` records these checks. Task and grader containers run as the invoking UID/GID with a writable home/workspace, a 2 GiB memory limit, no swap, 256 processes and a 1 GiB `/tmp`. `environment.limits` records those defaults. Rootless or remapped Docker bind ownership is refused for both task and grader containers with its cause; host-only local grading needs Docker to be unavailable. The embedded `proxy.py` runs with `python3` inside the suite base image; `claude_process.py` runs with that image's Python in the derived Claude agent layer. During a benchmark, neither helper runs on the host, including with `--local`. The base image must provide Python 3 and its standard library. Missing Python or a missing or failing proxy aborts preparation when the proxy policy check fails; a missing or failing Claude launcher fails the invocation. There is no host-Python fallback or retry without the egress limit. If the proxy stops later, the isolated task network still has no other egress route. Graders run in the base image on a separate workspace copy, with `--network=none`, and no credential mounts.

Sign-in files are mounted read-only for containers and linked for local runs. They are never copied into images, logged or included in results. Supported credential environment variables take precedence. Before each mounted-sign-in invocation, a host check uses a fresh empty workspace and a credential-only home. Claude has `--tools ""`, safe mode and no settings; Codex uses the non-generative `login status` command; Muse and Grok use account/catalog calls; Gemini refuses file-based sign-in because a tool-free host check is not verified; use `GEMINI_API_KEY` or `GOOGLE_API_KEY` instead. No check uses bypass flags. Output is discarded. Container renewal is blocked: Claude renewal uses `platform.claude.com/v1/oauth/token`, Codex `auth.openai.com`, Muse `auth.meta.com`, Grok `auth.x.ai` (and `accounts.x.ai` for sign-in), and Gemini `oauth2.googleapis.com`. These hosts are excluded. Inference allowlists are Claude `api.anthropic.com`; Codex `chatgpt.com`, `api.openai.com`; Muse `api.meta.ai`; Grok `api.x.ai`, `cli-chat-proxy.grok.com`; Gemini `generativelanguage.googleapis.com`, `cloudcode-pa.googleapis.com`. The first four renewal endpoints were checked against the installed vendor binaries; Gemini follows Google OAuth. Each task mounts the current host sign-in read-only. Agent failures stop the run as incomplete without an in-run retry; an expired sign-in requires checking the vendor sign-in and running again. Local Bench and Compare atomically preserve a renewed credential that replaces its temporary symlink, retaining the original mode, before deleting that home. Local mode reuses Compare's shared filtered shadow home, without user rules, skills, hooks, memory or MCP. Native local runs use private XDG data, state and cache directories as well; custom command runs inherit the caller environment except `CEDR_*` and `GIT_*`, record `clean_home: false`, and supply their own isolation. It is configuration isolation, not a sandbox. On every platform, local mode grades in the Docker base image when Docker is available. Without Docker it grades code on the host, so local scores depend on host Python/Node toolchains and provide no grader network isolation. Visual tasks cannot be graded there and report a clear error.

Other coding agents, including Cursor, Copilot and opencode, can use the local command adapter and cannot qualify for listing.

The adapters are Claude, Codex, Muse, Grok, Gemini ([documentation-derived](https://geminicli.com/docs/cli/headless/) and unverified), Antigravity (ACP, local only), and command (custom argv, local only, never eligible). Efforts must be offered by the CLI for the selected model; unknown availability is a refusal rather than a guessed substitution. No silent model or effort fallback is permitted.

## Usage and accounting

Five nullable token fields are fresh `input_tokens`, `cache_read_tokens`, `cache_write_tokens`, `output_tokens`, `reasoning_tokens`. Codex input includes cache and its output includes reasoning: subtract cache from input, do not add reasoning twice. Claude modelUsage covers the whole run, including side queries, and reports `thinkingTokens` as reasoning. Claude is invoked with verbose stream JSON so request usage can select the Monitor catalog's long-prompt tier; session totals alone cannot select it. Cache writes use their reported one-hour lifetime, with unclassified writes priced at five minutes. Repeated content blocks for one message are counted once for tier pricing. If unobserved usage could itself exceed the tier threshold, cost remains null rather than guessing. Codex, Muse and Gemini declare unreported cache writes already included in ordinary input and billed at the plain input rate, with no separate write premium. Their unreported write class is zero in the normalized split to avoid double counting. All other missing cache classes make cost unknown, including Antigravity. Missing reported fields remain null. List cost uses Monitor prices for each resolved model; unreported usage or an unknown price yields null cost. A reported subset never implies zero spend for the missing part. Duration and turns are nullable when unavailable. Both probe parts are recorded and included in run spend and unpriced counts. Host account maintenance is separate from task/probe usage; catalog/account calls are preferred, with fixed no-tool inference checks where necessary.

`--stop-at-usd` overrides `bench_stop_usd` in `$XDG_CONFIG_HOME/cedr/cli.toml`. Both must be finite and positive. Check the accumulated known spend before every task; a stop produces `user_stop`. With no stop, print exactly one warning naming `--stop-at-usd`. The goal never stops work. A timed-out task scores zero and can still belong to a complete, eligible run; `agent_error`, `setup_failed`, `interrupted` and `not_reached` cannot. A timed-out or interrupted invocation has unknown cost even if partial usage was emitted. Unknown timeout usage makes the run cost unknown (the recorded spend is only the known subtotal). Unknown cost stops before another task unless `--allow-unpriced`.

The runner chooses and records each graded task's `seed` and passes it as `BENCH_SEED` for reproduction. Submission symlinks are skipped when copying for grading and never followed. A code grader emits exactly four uniquely named boolean checks. A visual grader emits gates (`id`, `passed`, optional `detail`) and artifacts (relative paths); declared task artifacts plus emitted artifacts are collected after validating regular files within the grader copy. Malformed grader output is `setup_failed`, never a pass. A failed visual `file` gate may omit an uncreated declared submission; it remains a graded failure. Every artifact explicitly emitted by a grader must exist, even when a gate fails. Screenshots are PNG files. Artifact metadata is `{path, sha256, bytes}` with paths relative to the saved result artifact directory; no absolute paths enter results.

## Persistence and consistency

Saved result version is 1 and kind is `bench`. The issue's result example defines required fields. `edition.image` records the image actually used, `environment.pinned` records whether it matched the edition, and `environment.agent_image` records the local layer. `eligible` additionally requires a complete run, a pinned container, clean home, allowlist networking, a successful matching or setting fingerprint and readiness probe, and a verified vendor adapter. `billing` is `api` or `plan`, supplied with `--billing` (default `plan`).

`show` validates ids, unique task ids, finite nonnegative costs, check counts, score, solved, task totals, spent, unpriced count, over_goal, completion, stop and listing eligibility against the underlying records. It refuses inconsistent results. History returns summary rows newest first. Both MCP tools use the same loader. Results contain neither prompts nor captured agent transcripts. Each code task's diff against its starting state is saved locally as `<id>/artifacts/<task>/submission.patch`, with `patch_sha256` in the result; these audit patches are never exported by `bench export` or pushed. `tamper_signals` records matched path, line and signal category (an empty list when clean), without source snippets; a changed file above the 16 MiB scan limit also emits a signal. Signals do not change scores, but any signal makes a run ineligible pending human review. There is no automatic review override: a person audits the local patch before considering it for publication.

`run` and `probe` accept `--suite`, `--edition`, `--local`, `--build-image`, `--agent-version`, `--set-reference`, `--command ARG` (repeatable for command only), and `--format text|json`. Probe results are printed but not saved as benchmark runs. `history --limit N` and `show ID` accept text and JSON. `export` copies validated screenshot artifacts and writes the original result objects, unchanged, as a JSON array. Export verifies artifact hashes and refuses overwrites. No command uploads results.

Exit 0 means complete, 3 incomplete or failed probe, 2 usage or I/O failure. No score threshold changes an exit code. There is no Bench Action input in this version: CI may invoke the CLI directly. There is no MCP run tool because it spends money. Fetching a suite, pulling images and vendor installers make ordinary operational network requests, documented in telemetry.md, without sending results.

## Adapter verification limits

Local effort validation uses Claude's SDK initialize capabilities (no model turn), and generated vendor catalogs for Codex, Muse and Grok. Container validation queries the installed agent layer: Claude SDK capabilities or Codex `model/list`; other container adapters refuse explicit effort until their metadata can be verified. Host binaries never validate a container effort. If Gemini or Antigravity cannot expose verifiable effort metadata, explicit `--effort` is refused; omitting it preserves the vendor default, and Antigravity model ids may already encode effort. This is a refusal, never a guessed substitution. Gemini remains documentation-derived and has no signed-in live verification. Antigravity supports `--local` only. The container path is untested on macOS and Windows. Windows local Codex trials record `clean_home: false`, because home-level user skills remain reachable. No adapter installs or updates an agent on the host as part of a run.

The hidden test runs in the same process as the submission: the nonce stops early exits and printed fakes. Deliberate introspection of that process is flagged conservatively by the tamper scan and checked by audit, rather than prevented; obfuscated attacks may evade the scan. A process boundary between grader and submission is planned for a later edition.
