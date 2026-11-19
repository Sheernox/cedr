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
- `spent_usd` is the known subtotal, including probes and visual tasks. If `unpriced_trials > 0`, the run cost is incomplete and text says "at least $X, N trials unpriced". Cost-per-run and cost-per-solved comparisons are unknown in that case. `over_goal` is unknown-safe: it is true if the known subtotal exceeds the goal or any trial is unpriced, so false always means fully priced and within goal. Cost never fails, truncates or drops a run.
- There is no spending stop by default. `--stop-at-usd N` (or the `bench_stop_usd` setting) adds one: it is checked before each task, and a run it ends is incomplete with `stop: "user_stop"`. With no stop set, `run` prints one warning before it starts saying so and naming the flag.
- A trial with unknown cost follows Compare's rule: the run stops as `unknown_cost` unless `--allow-unpriced`.

## A clean environment

A result must depend on the configuration, not on whose machine ran it. Three layers:

1. **Clean agent home.** Every trial runs with an agent home that holds the sign-in and nothing else: no user rules, skills, hooks, memory, MCP servers or account connectors. Each harness has its own recipe (for example a fresh `CODEX_HOME` for Codex; `--safe-mode --strict-mcp-config` for Claude). Sign-ins are mounted read-only; cedr never writes a sign-in file or saves a renewal back. Credentials are never printed. See #52 for what leaks today.
2. **The pinned image.** The agent and the graders run in the container image the edition names by digest: fixed toolchains, fonts and browser. The agent's network is limited to its model provider's hosts; graders get none.
3. **The probe.** Before the tasks, the runner sends a fixed fingerprint prompt in an empty workspace and records its input tokens (fresh, cache read and cache write together), followed by a separate tool-readiness prompt whose files are verified. Extra rules or tools show up as extra tokens. The separately published `probes/edition-<n>.toml` carries a reference per harness version, model and effort; the result records `match`, `mismatch` or `no_reference`.

The result's `environment.mode` is always `container`. The derived `eligible` field requires a complete run, a pinned container, a clean home, allowlist networking, a fingerprint matching a published reference, passing tool readiness, a verified vendor adapter, and no unresolved tamper signals.

## Harnesses

| Harness | Runs |
|---|---|
| `claude` | Claude Code CLI, headless |
| `codex` | Codex CLI, headless |
| `muse` | Muse Code, `muse exec --json` |
| `grok` | Grok Build, `grok -p --output-format json` |
| `gemini` | Gemini CLI, headless |
| `antigravity` | T3 Code's managed Antigravity ACP server. Requires `--effort`. |
| `command` | Refused by Bench; use Compare for custom local commands |

Each adapter reports tokens split into fresh input, cache read, cache write, output and reasoning (where the agent reports them), the agent's version and the models it resolved. An adapter that cannot report tokens leaves the trial unpriced. An effort level a model does not offer is an error, never substituted.

## Result

Saved to `$XDG_STATE_HOME/cedr/bench/<id>.json`, with artifacts beside it in `<id>/artifacts/<task>/`.

```json
{
  "version": 1, "kind": "bench", "id": "b-20261113T050000Z-1a2b3c",
  "edition": {"number": 1, "suite_sha256": "...", "commit": "...", "image": "sha256:..."},
  "config": {"harness": "claude", "harness_version": "...", "model": "...", "effort": "high", "resolved_models": ["..."]},
  "environment": {"mode": "container", "clean_home": true, "network": "allowlist", "pinned": true, "agent_image": "sha256:...", "verified": ["engine>=28", "isolated-internal-bridge", "task-network-gateway-absent", "external-ip-unreachable", "external-dns-blocked"], "limits": "memory=2g,pids=256,tmp=1g,swap=0", "os": "linux", "arch": "x86_64", "cli": "..."},
  "probe": {"input_tokens": 31926, "tools": 27, "reference": 31926, "tolerance": 16, "status": "match", "tools_ok": true},
  "billing": "plan",
  "started_at": "...", "finished_at": "...",
  "goal_usd": 10.0, "stop_usd": null, "spent_usd": 4.12, "over_goal": false, "unpriced_trials": 0,
  "complete": true, "stop": null, "eligible": true,
  "score": 72.5, "solved": 6, "code_tasks": 10,
  "tamper_review": null,
  "tasks": [
    {"id": "py-ledger-rounding", "kind": "code", "status": "graded",
     "checks": [{"id": "reported-case", "passed": true}], "score": 0.75, "solved": false,
     "duration_ms": 61200, "turns": 9,
     "input_tokens": 0, "cache_read_tokens": 0, "cache_write_tokens": 0, "output_tokens": 0, "reasoning_tokens": null,
     "cost_usd": 0.31, "patch_sha256": "...", "tamper_signals": [], "gates": [], "artifacts": []}
  ]
}
```

A visual task has `kind: "visual"`, `gates: [{"id", "passed", "detail"}]` and `artifacts: [{"path", "sha256", "bytes"}]` (the file the agent wrote and each screenshot), and no `checks`. Task `status` is one of `graded`, `timeout`, `agent_error`, `setup_failed`, `interrupted`, `not_reached`. Result JSON holds patch and artifact hashes and relative artifact paths; event streams are parsed for usage and discarded.

## Commands

- `cedr bench run --harness H --model M [--effort E] [--edition N | --suite PATH] [--stop-at-usd N] [--task ID]... [--allow-unpriced] [--format text|json]`
- `cedr bench probe --harness H --model M [--effort E]`: the probe alone.
- `cedr bench history`, `cedr bench show <ID>`: as in Compare.
- `cedr bench export <ID>... --out DIR`: writes `results.json` and screenshots under `shots/<id>/<task>/`, for publishing.
- `cedr bench push [ID]... [--org ORG] [--config PATH] [--dry-run] [--format text|json]`: explicitly sends saved measurements to an organization, as described below.

Exit codes follow Compare: 0 complete, 3 incomplete, 2 usage or I/O. There is no `--fail-on`: Bench reports, it does not gate.

`cedr bench review ID --clean --note "Read each saved patch; ordinary application code"` records a person's audit of a flagged run. Use `--tampered` instead to keep it ineligible. Exactly one verdict and a single-line, nonblank note of 1-500 characters are required. Runs without signals cannot be reviewed. A later review replaces the earlier review atomically. `show` prints signals, the review, saved patch paths and review instructions when needed.

MCP gains `bench_history` and `bench_show`. There is no `bench_run`, for the reason Compare has none.

## Not in the first version

Community submissions, a spending stop by default, a parallel mode, and any ranking of the visual tasks.


## Suite schema and integrity

All TOML objects deny unknown fields. Task ids use ASCII letters, digits, `_`, `-` and `.` (never `.` or `..`). Paths are relative, contain no parent components, and must not traverse symlinks. Suite files and task repositories contain regular files only; `.git` is excluded. Task metadata additionally accepts descriptive `summary` and `difficulty` strings.

`probes/edition-<n>.toml` contains `[[references]]` entries: `harness`, `harness_version`, `model`, optional `effort`, `input_tokens`, optional `tools`, optional `tolerance` (default 16, integer 0-64). Identity includes effort. Duplicate identities are errors. Reference comparison uses total fresh plus cache-read plus cache-write input, with the reference's inclusive tolerance. Use the smallest allowance supported by repeated measurements; never exceed 64. Missing reported fields remain null. A reference does not prove byte-identical context: small injections below the tolerance can pass.

The probe has two independent prompts in separate empty workspaces and fresh clean homes. Part 1, the fingerprint, is exactly `Reply with the single word OK.` It needs one model request and no tools. Compare its total fresh plus cache-read plus cache-write input tokens with the reference within the reference's tolerance for every adapter, including those that only report run totals. Part 2, readiness, is exactly: `Run exactly one shell command: printf 'cedr-bench-command\n' > command.txt. Then use apply_patch or your native file-write tool (not the shell) to create probe.txt containing exactly cedr-bench-probe followed by a newline. Do not run any other shell commands.` The runner verifies both files after trimming trailing whitespace (leading whitespace and other extra text fail), recording `tools_ok`. Each marker must be a regular UTF-8 file of at most 4 KiB. The shell command and separate file-write instruction exercise both capabilities. Readiness tokens are never compared. The explicit native-tool instruction avoids interpreting the shell-command limit as a ban on creating the second file. There is no readiness retry. A passing probe requires `match` (or `reference`) and `tools_ok`. `no_reference` is usable for diagnosis but is not a passing probe. `--set-reference` records status `reference` and prints an entry for that probe file only when no reference exists for the identity. An existing reference always wins, including a mismatch; the runner never edits the reference file.

Probe records hold fingerprint `input_tokens`, optional readiness `tools`, `reference`, `tolerance`, `status`, `tools_ok`, `fingerprint_cost_usd`, `tools_cost_usd`, and combined `cost_usd` (null if either cost is unknown). Run spend includes each known part even if the other is unknown; unpriced counts include each unknown part. Optional per-request usage must not replace the uniform total-token fingerprint comparison.

`--suite PATH` reads a local tree. Otherwise edition defaults to 1. `--edition N` clones `https://github.com/Sheernox/cedr-bench.git` at `edition-N` into `$XDG_CACHE_HOME/cedr/bench/edition-N`; the first fetch records the commit and SHA-256 alongside it. Subsequent loads recompute and verify both, refusing drift. This is trust on first fetch, not a publisher signature. Hashing sorts relative UTF-8 names and hashes length-prefixed names, executable bits and file bytes, excluding `.git` and `probes/`. A local suite also records that digest and its commit when available. With `--suite`, references come from that local tree. Otherwise the runner fetches only the probe file from the suite repository default branch and caches it separately. Offline runs reuse that copy; without a copy they record `no_reference`. Every loaded suite is snapshotted before execution so later source edits cannot change prompts or graders in flight. Grader support files participate in the digest.

`--task ID` is diagnostic: all edition tasks remain in the result, unselected tasks are `not_reached`, the run is incomplete and ineligible. Each reached task starts from a new one-commit repository. Patch digests cover the final binary git diff, including untracked files, against that initial commit.

## Execution and credentials

Container mode uses Docker CLI subprocesses only. The edition's `image` must include `@sha256:<64 hex>`. `--build-image` builds `image/Dockerfile`; an unpinned edition is allowed for this option, records `pinned: false`, and cannot qualify for listing. The actual image id and agent layer id are recorded. Agent layers are local, keyed by base digest, harness and exact version, and never pushed. Bench copies supported local binaries, including the Antigravity ACP release, into the agent layer. For other harnesses, it runs the vendor installer at the requested version. The executable version is checked before use, and Claude automatic updates are disabled for the invocation. In containers, its terminal result ends the invocation and stops remaining helpers before grading; a background shell tool cannot keep a completed response alive until the task timeout.

On Unix, interrupting a probe stops its active container work. If a Bench process is killed, the next run or probe using the same state directory and Docker daemon reclaims its labelled containers and network. Concurrent runs keep their own resources. Resources created before Bench tracked ownership need manual cleanup.

The agent runs non-root at `/workspace` with home `/home/bench`, a read-only root, dropped capabilities, no-new-privileges and writable temporary mounts. Only provider credentials, proxy settings and the fixed tool environment are allowed. Provider hostnames are exact allowlist entries, with no wildcards. A CONNECT-only proxy on an internal Docker network with an isolated bridge gateway is the sole egress route (Docker Engine 28 or newer is required); the proxy alone also joins an external network. Only port 443 is accepted; loopback, private, link-local and multicast destinations are refused after DNS resolution. The agent has neither an external network nor the Docker socket. Before inference, the runner checks the engine version and network inspection, then, after attaching the egress proxy, records `task-network-gateway-absent` when inspection reports no gateway. If the task network reports a gateway, the runner tests that address and a live host listener and records `host-gateway-unreachable` and `live-host-port-unreachable` only after those tests pass. It also actively tests that a task-network container cannot connect to an outside IP or resolve an outside host. Failure is fatal; `environment.verified` records only the checks actually performed. Task and grader containers run as the invoking UID/GID with a writable home/workspace, a 2 GiB memory limit, no swap, 256 processes and a 1 GiB `/tmp`. `environment.limits` records those defaults. Rootless or remapped Docker bind ownership is refused with its cause. The proxy and the Claude and Antigravity launchers use the base image's Python 3 and standard library. Proxy policy failures abort preparation. A launcher failure fails the invocation. There is no host-Python fallback or retry without the egress limit. If the proxy stops later, the isolated task network still has no other egress route. Graders run in the base image on a separate workspace copy, with `--network=none`, and no credential mounts.

Sign-in files are mounted read-only for containers. They are never copied into images, logged or included in results. Supported credential environment variables take precedence. Before each mounted-sign-in invocation, a tool-free host check runs in a fresh empty workspace using the person's real agent home: only vendor code writes credentials and coordinates renewals. Cedr preserves configured locations and environment, including unresolved credential symlinks, introduces no location override and disables self-updates. Claude uses `--safe-mode --strict-mcp-config --tools "" --setting-sources "" --no-session-persistence`; Codex uses `login status`, Muse `usage --json` and Grok `models`. These checks expose no user tools. Each Claude host check is one short reply, capped at $0.25 separately from task/probe cost totals; it is not included in the run spending stop. Sign-in paths are resolved again after checks, immediately before reading guard values and mounting, so vendor file or symlink replacements take effect. Other harnesses keep renewal hosts outside the task allowlist. Antigravity opens an ACP session on the host without prompting. Its task allowlist is limited to `oauth2.googleapis.com`, `cloudcode-pa.googleapis.com` and `daily-cloudcode-pa.googleapis.com` on port 443. A failed task stops the run without a retry; check the vendor sign-in before running again. Cedr never writes a sign-in file.

Bench runs only in containers. Claude, Codex, Muse, Grok and Antigravity have verified adapters. Gemini remains documentation-derived and unverified. Use Compare for local comparisons with a harness without container support. Efforts must be offered by the CLI for the selected model; unavailable metadata is a refusal, with no silent model or effort fallback.

For Antigravity, install the managed ACP server in T3 Code. Bench uses the active release under `~/.t3/tools/antigravity-acp/linux-x64` with `agy_acp_server.par` and `localharness_external`. Set `CEDR_ANTIGRAVITY_DIR` to select another installed release directory. If supplied, `--agent-version` must match that release. Point `GEMINI_HOME` to your signed-in Antigravity profile, such as `~/.t3/userdata/providers/antigravity/<profile>`. Without it, Bench looks under `~/.gemini`. Bench mounts only `antigravity-acp/acp_token.json` and configures OAuth sign-in in each fresh task home. Like T3 Code, Bench serves Antigravity's file reads itself: UTF-8 text files of at most 8 MiB inside the task workspace, optionally by line range. Antigravity can record a failed model request without ending its turn. When its newest step is such an error and it has been quiet for a minute, Bench ends the task as an agent error that names the provider's HTTP status, and the run stops.

## Usage and accounting

Five nullable token fields are fresh `input_tokens`, `cache_read_tokens`, `cache_write_tokens`, `output_tokens`, `reasoning_tokens`. Codex input includes cache and its output includes reasoning: subtract cache from input, do not add reasoning twice. Claude modelUsage covers the whole run, including side queries, and reports `thinkingTokens` as reasoning. Claude is invoked with verbose stream JSON so request usage can select the Monitor catalog's long-prompt tier; session totals alone cannot select it. Cache writes use their reported one-hour lifetime, with unclassified writes priced at five minutes. Repeated content blocks for one message are counted once for tier pricing. If unobserved usage could itself exceed the tier threshold, cost remains null rather than guessing. Codex, Muse, Gemini and Antigravity declare unreported cache writes already included in ordinary input and billed at the plain input rate, with no separate write premium. Their unreported write class is zero in the normalized split to avoid double counting. Antigravity reads one usage record per model request. Its output count already includes reasoning. A missing fresh input or output count on any request leaves the trial unpriced. Other missing cache classes also make cost unknown. Missing reported fields remain null. List cost uses Monitor prices for each resolved model; unreported usage or an unknown price yields null cost. A reported subset never implies zero spend for the missing part. Duration and turns are nullable when unavailable. Both probe parts are recorded and included in run spend and unpriced counts. Host account maintenance is separate from task/probe usage; catalog/account calls are preferred, with fixed no-tool inference checks where necessary.

`--stop-at-usd` overrides `bench_stop_usd` in `$XDG_CONFIG_HOME/cedr/cli.toml`. Both must be finite and positive. Check the accumulated known spend before every task; a stop produces `user_stop`. With no stop, print exactly one warning naming `--stop-at-usd`. The goal never stops work. A timed-out task scores zero and can still belong to a complete, eligible run; `agent_error`, `setup_failed`, `interrupted` and `not_reached` cannot. A timed-out or interrupted invocation has unknown cost even if partial usage was emitted. Unknown timeout usage makes the run cost unknown (the recorded spend is only the known subtotal). Unknown cost stops before another task unless `--allow-unpriced`.

The runner chooses and records each graded task's `seed` and passes it as `BENCH_SEED` for reproduction. Submission symlinks are skipped when copying for grading and never followed. A code grader emits exactly four uniquely named boolean checks. A visual grader emits gates (`id`, `passed`, optional `detail`) and artifacts (relative paths); declared task artifacts plus emitted artifacts are collected after validating regular files within the grader copy. Malformed grader output is `setup_failed`, never a pass. A failed visual `file` gate may omit an uncreated declared submission; it remains a graded failure. Every artifact explicitly emitted by a grader must exist, even when a gate fails. Screenshots are PNG files. Artifact metadata is `{path, sha256, bytes}` with paths relative to the saved result artifact directory; no absolute paths enter results.

## Persistence and consistency

Saved result version is 1 and kind is `bench`. The issue's result example defines required fields. `edition.image` records the image actually used, `environment.pinned` records whether it matched the edition, and `environment.agent_image` records the local layer. `eligible` additionally requires a complete run, a pinned container, clean home, allowlist networking, a fingerprint matching a published reference and a successful readiness probe, and a verified vendor adapter, no credential match or `evidence_mismatch`, and no signals or a clean human review. `billing` is `api` or `plan`, supplied with `--billing` (default `plan`).

`show` validates ids, unique task ids, finite nonnegative costs, check counts, score, solved, task totals, spent, unpriced count, over_goal, completion, stop and listing eligibility against the underlying records. It refuses inconsistent results. History returns summary rows newest first. Both MCP tools use the same loader. Raw prompts and agent event streams are not saved. Each code or visual task's diff against its starting state includes ignored files and nested repository contents as ordinary files, using the same copy policy as grading. Git content transformations are disabled. Before grading, the runner applies the saved patch to a pristine starting tree and compares every resulting file byte for byte with the grader copy. A difference or failed replay records `evidence_mismatch`, which can never receive a clean review and always prevents eligibility. The added lines in the same patch feed the scan. It is saved as `<id>/artifacts/<task>/submission.patch`, with `patch_sha256` in the result.

The credential guard registers sign-in strings of at least eight characters under a key or ancestor containing `token`, `secret`, `password`, `credential`, `apikey` or `api_key`, or exactly `key` (ASCII case-insensitive), opaque non-URL strings of at least 40 characters, and every nonempty credential environment value. It matches byte substrings in plain form and the forms listed by the [GitHub Actions runner](https://github.com/actions/runner/tree/8cfa1049f86781d141056082d73802d56cd2b433): UTF-8 base64 at offsets 0, 1 and 2; command argument, expression single-quote, JSON, URI and XML escaping; surrounding-double-quote removal; and PowerShell ampersand fragments. Trimmed line fragments require eight characters. It does not catch a value transformed in any other way. Patches and changed paths are checked before the tamper scan; reported models, grader strings, artifact names and contents, and saved or printed task metadata are checked before retention. A match discards the task's patch and agent-controlled metadata, records only a fixed `descriptor_access` signal at `submission.patch:1`, and makes the task `setup_failed`; clean review refuses it and a new run is required. Nothing agent-chosen is exported: tamper paths and line numbers stay local, while export carries each signal's `kind` only.

Each task's `tamper_signals` is an array of `{"path":"src/example.py","line":12,"kind":"process_introspection"}` objects, empty when clean. Paths are relative to the task workspace; line numbers are one-based positions in the submitted file. Only lines added or changed against the task's starting commit are scanned, including every line of a new file. Categories are `process_introspection`, `descriptor_access`, `runner_modification`, `assertion_reassignment`, `evidence_mismatch` and `scan_limit` (a changed file above 16 MiB, reported at line 1). Ordinary entry points, process exits, `importlib`, `process.stdin`, `fileno()` and `os.close()` do not alone prompt review; the completion nonce already makes early exits score zero.

Signals are review prompts and never change correctness scores. A run with any signal remains ineligible until a person reads its saved patches and records a clean review, except that `evidence_mismatch` and credential matches always remain ineligible and require the run to be run again. The top-level `tamper_review` is null when unreviewed, otherwise `{"verdict":"clean","note":"Read each saved patch; ordinary application code","time":"2026-11-14T00:00:00+00:00"}`. Verdict is `clean` or `tampered`; notes and offset-bearing ISO-8601 times are validated when loading a result. A clean review verifies each flagged task's saved patch against its recorded hash. Missing or altered files refuse the review with the task and filename. The same verification runs before a new result becomes eligible and when an eligible result is loaded. It clears only the tamper restriction, so other eligibility requirements still apply. MCP `bench_show` carries the review and original signals; export carries the review and signal kinds.

`run` and `probe` accept `--suite`, `--edition`, `--build-image`, `--agent-version`, `--set-reference`, and `--format text|json`. The publisher sets references first with `probe --set-reference` and appends the printed entry to `probes/edition-<n>.toml`. A self-set `Reference` is not eligible; listing requires `Match` against a published reference with all input token classes present. Probe results are printed but not saved as benchmark runs. `history --limit N` and `show ID` accept text and JSON. `export` copies validated screenshots and writes validated results as a JSON array, with tamper paths and line numbers omitted. It refuses screenshot hash or length mismatches and overwrites. Patches remain local. The loader accepts retired capture fields from older version-1 results and derives eligibility without those requirements; an obsolete hold alone does not prevent listing. Runs never upload automatically; only an explicit `push` sends measurements.

## Push

`cedr bench push` sends unsent saved runs on this machine. Explicit IDs resend those runs, including ones sent before. `--org` overrides `[sync] org`; `--config` selects the local routes file. The API key comes from `CEDR_API_KEY` or the configured secret helper, as for Compare. Running Bench still needs no account. Neither `run`, `review`, `show`, `export` nor the read-only MCP tools upload anything. Push validates recorded measurements without requiring local artifact files; it does not verify or claim public eligibility.

Review flagged runs before their first push. The accepted copy includes the review verdict and cannot be updated by resending. If you review a run after pushing it, remove its saved result from the organization in the app, then push that ID explicitly to send the revised measurements.

Each request is `POST <sync url>/bench/runs`, with a JSON body of at most 256 KiB:

```json
{"org":"org-000000000001","cli":"X.Y.Z","machine":{"id":"m-000000000001","name":"workstation"},"result":{}}
```

`result` is a version-1 `kind: "bench"` measurement with these allowlisted fields:

- Header: `version`, `kind`, `id`, `edition`, `config`, `billing`, `started_at`, `finished_at`, `goal_usd`, `stop_usd`, `spent_usd`, `over_goal`, `unpriced_trials`, `complete`, `stop`, `score`, `solved`, `code_tasks`.
- `edition`: number, suite digest, commit and pinned image. `config`: harness, harness version, model, effort and resolved models. Local model paths are replaced with `[local model]`, including Windows relative forms. Names such as `provider/model` and `a:mini` stay unchanged.
- `environment`: `mode`, `clean_home`, `network`, `pinned`, `os`, `arch`, `cli`. These and the probe are the CLI's reported facts, not server attestations.
- `probe`: input tokens, tool count, reference, tolerance, status, tool readiness and the three cost fields.
- `tasks`: id, kind, status, checks (`id`, `passed`), score, solved, duration, turns, the five token fields, cost, gates (`id`, `passed`), and tamper signals (`kind` only).
- `tamper_review`: null, or the verdict and offset-bearing time. The note is omitted.

Prompts, event output, errors, gate details, patches, artifact paths, screenshots, seeds, review notes and eligibility claims are omitted. Check and gate IDs must be plain suite identifiers. Unknown signal categories are sent as `other`. Local files are unchanged. The endpoint accepts complete and incomplete runs, strips unknown fields, validates derived measurements and checks organization membership. It never executes uploaded content or submits a result to the public Benchmarks page.

An accepted reply is `{"ok":true,"id":"b-20261113T000000Z-abcdef","duplicate":false,"retained":true}`. The organization and run ID identify an immutable first write across members; a retry returns `duplicate: true`. `retained` reports whether the result remains after the organization's current history allowance is applied. An expired or over-cap result returns `retained: false`; the CLI reports it separately as `not_retained`, preserves its local file and records the acknowledgement so default batches do not repeatedly resend it. Explicit IDs recheck it if the allowance changes. Results kept now can still expire or be removed by later retention.

Only matching accepted replies with both boolean fields mark a run sent, separately for each organization. A 503 is retried at most twice using the bounded `Retry-After` interval. Other failures leave the run available for a later push. A batch still sends its other runs and exits 2 if any failed; normal JSON output is `{pushed: [id], duplicates: [id], not_retained: [id], failed: [{id, error}]}`. To bypass an oversized or unsupported saved result, name the other IDs explicitly. `--dry-run` needs no API key, prints the request bodies as a JSON array and neither sends nor marks them. Exit 0 means every result was acknowledged (including any reported as not retained), or there were no unsent runs.

Exit 0 means complete, 3 incomplete or failed probe, 2 usage or I/O failure. No score threshold changes an exit code. There is no Bench Action input in this version: CI may invoke the CLI directly. There is no MCP run tool because it spends money. Fetching a suite, pulling images and vendor installers make ordinary operational network requests, documented in telemetry.md, without sending results.

## Adapter verification limits

Container effort validation queries the installed agent layer: Claude SDK capabilities, Codex `model/list` or the model options in an Antigravity ACP session. Other adapters refuse explicit effort until their metadata can be verified. Host binaries never validate a container effort. The container path is untested on macOS and Windows. No adapter installs or updates an agent on the host as part of a run.

Edition 1 Python and Node graders run submission code in the test process, alongside hidden checks. Ordinary equality and plain file hashing check the task contracts; performance checks use wall-clock deadlines. The defences are clean-tree replay and advisory tamper flags read by a person before listing. The completion nonce detects early exits and printed fakes. The scan flags explicit comparison hooks, imported-module writes, builtin prototype writes, process introspection and runner modification where practical, but deliberate or obfuscated in-process tampering can evade detection. A clean review is a person's statement about the evidence. A process boundary between grader and submission, and published evidence, are planned for edition 2.
