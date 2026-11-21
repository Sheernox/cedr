#!/usr/bin/env bash
# Build and post the action's one sticky pull request comment (docs/design.md, GitHub Action).
# The comment is found again by its hidden marker, so every push updates the same comment
# instead of adding a new one. Runs only from action.yml, which passes every value below as
# an environment variable; a failure to comment warns and never changes the job's result,
# because the lint and verify steps already decided it.
#
# Inputs (environment): INPUT_COMMENT, INPUT_PATH, INPUT_STRICT, INPUT_VERIFY, INPUT_REQUIRE_TYPED_PINS, INPUT_SYNC, INPUT_OUTDATED,
# COMMENT_EVENT, COMMENT_PR, COMMENT_HEAD_REPO, COMMENT_BASE_REPO, COMPARE_REPORT,
# GITHUB_REPOSITORY, GH_TOKEN (read by gh), RUNNER_TEMP, CEDR_BIN (default cedr).
set -uo pipefail

marker='<!-- cedr-action -->'
cedr_bin="${CEDR_BIN:-cedr}"
work="${RUNNER_TEMP:-/tmp}/cedr-comment"
mkdir -p "$work"

notice() { echo "::notice title=cedr::$1"; }
warn() { echo "::warning title=cedr::$1"; }

case "${INPUT_COMMENT:-false}" in
  true) ;;
  false) exit 0 ;;
  *) echo "::error title=cedr::comment must be true or false"; exit 1 ;;
esac

if [ "${COMMENT_EVENT:-}" != "pull_request" ]; then
  notice "comment skipped: not a pull_request event"
  exit 0
fi
if [ -z "${COMMENT_PR:-}" ]; then
  notice "comment skipped: no pull request number"
  exit 0
fi
if [ "${COMMENT_HEAD_REPO:-}" != "${COMMENT_BASE_REPO:-}" ]; then
  notice "comment skipped: the pull request comes from a fork, whose token cannot comment"
  exit 0
fi
if ! command -v jq >/dev/null 2>&1; then
  warn "comment skipped: jq is not installed on this runner"
  exit 0
fi
if ! command -v gh >/dev/null 2>&1; then
  warn "comment skipped: gh is not installed on this runner"
  exit 0
fi

# Re-run each check in JSON so the comment can count and quote findings; the steps above
# already ran them as annotations and decided the job, so exit codes here are ignored.
# Guard keeps its old top-level names, like the steps in action.yml.
lint_args=("$cedr_bin" lint "$INPUT_PATH" --format json)
if [ "${INPUT_STRICT:-false}" = "true" ]; then lint_args+=(--strict); fi
"${lint_args[@]}" >"$work/lint.json" 2>/dev/null || true

verify_json=""
case "${INPUT_VERIFY:-auto}" in
  false) ;;
  auto) if [ -f "$INPUT_PATH/.cedr/guard.lock" ] || [ -f "$INPUT_PATH/cedr.lock" ]; then verify_json="$work/verify.json"; fi ;;
  true) verify_json="$work/verify.json" ;;
esac
if [ -n "$verify_json" ]; then
  verify_args=("$cedr_bin" verify "$INPUT_PATH" --format json)
  if [ "${INPUT_REQUIRE_TYPED_PINS:-false}" = "true" ]; then verify_args+=(--require-typed-pins); fi
  "${verify_args[@]}" >"$verify_json" 2>/dev/null || true
fi

outdated_json=""
if [ "${INPUT_OUTDATED:-true}" = "true" ] && { [ -f "$INPUT_PATH/.cedr/guard.lock" ] || [ -f "$INPUT_PATH/cedr.lock" ]; }; then
  outdated_json="$work/outdated.json"
  "$cedr_bin" outdated "$INPUT_PATH" --format json >"$outdated_json" 2>/dev/null || true
fi

sync_json=""
case "${INPUT_SYNC:-auto}" in
  false) ;;
  auto) if [ -f "$INPUT_PATH/.cedr/sync.toml" ]; then sync_json="$work/sync.json"; fi ;;
  true) sync_json="$work/sync.json" ;;
esac
if [ -n "$sync_json" ]; then
  "$cedr_bin" sync check "$INPUT_PATH" --format json >"$sync_json" 2>/dev/null || true
fi

valid_json() { [ -s "$1" ] && jq -e . "$1" >/dev/null 2>&1; }

# One findings section: the count, then the first ten findings.
findings_section() {
  valid_json "$2" || return 0
  local count errors warnings
  count=$(jq '.findings | length' "$2")
  errors=$(jq '[.findings[] | select(.severity == "error")] | length' "$2")
  warnings=$((count - errors))
  printf '### %s: %s findings (%s errors, %s warnings)\n\n' "$1" "$count" "$errors" "$warnings"
  if [ "$count" -gt 0 ]; then
    jq -r '.findings[:10][] | "- `\(.path | gsub("`"; ""))\(if .line then ":\(.line)" else "" end)` \(.rule) \(.severity): \(.message | gsub("[\\r\\n]+"; " "))"' "$2"
    if [ "$count" -gt 10 ]; then printf -- '- and %s more\n' "$((count - 10))"; fi
    printf '\n'
  fi
}

outdated_section() {
  valid_json "$1" || return 0
  local count
  count=$(jq '.updates | length' "$1")
  printf '### Server updates: %s available\n\n' "$count"
  if [ "$count" -gt 0 ]; then
    jq -r '.updates[] | "- `\(.client).\(.name | gsub("`"; ""))`: \(.source) -> \(.latest) (\(.severity); \(.changes | length) surface changes)"' "$1"
    jq -r '.updates[] | .changes[] | "  - \(.severity) \(.kind)\(if .tool then " \(.tool)" else "" end): \(.detail | gsub("[\\r\\n]+"; " ") | .[0:200])"' "$1"
    printf '\n'
  fi
}

body_file="$work/body.md"
{
  printf '%s\n## cedr\n\n' "$marker"
  findings_section "Guard lint" "$work/lint.json"
  if [ -n "$verify_json" ]; then findings_section "Guard verify" "$verify_json"; fi
  if [ -n "$outdated_json" ]; then outdated_section "$outdated_json"; fi
  if [ -n "$sync_json" ]; then findings_section "Sync" "$sync_json"; fi
  if [ -n "${COMPARE_REPORT:-}" ] && [ -s "${COMPARE_REPORT:-}" ]; then
    printf '### Compare\n\n```text\n'
    # A report fence inside the report would end ours early; the report is plain text lines.
    # shellcheck disable=SC2016 # the backticks are the literal fence being escaped
    sed 's/```/`` `/g' "$COMPARE_REPORT"
    printf '```\n'
  fi
} >"$body_file"

# GitHub refuses comment bodies over 65,536 characters; keep a margin and say so.
body=$(cat "$body_file")
if [ "${#body}" -gt 65000 ]; then
  body="${body:0:65000}
[truncated by cedr: the full results are in the Actions log]"
  printf '%s\n' "$body" >"$body_file"
fi

repo="${GITHUB_REPOSITORY:-}"
if [ -z "$repo" ]; then
  warn "comment skipped: GITHUB_REPOSITORY is not set"
  exit 0
fi

existing=$(gh api "repos/$repo/issues/$COMMENT_PR/comments?per_page=100" --paginate \
  --jq "[.[] | select(.body | contains(\"$marker\"))][0].id // empty" 2>/dev/null | head -n 1) || existing=""

if [ -n "$existing" ]; then
  gh api -X PATCH "repos/$repo/issues/comments/$existing" -F "body=@$body_file" >/dev/null ||
    warn "the pull request comment could not be updated"
else
  gh api -X POST "repos/$repo/issues/$COMMENT_PR/comments" -F "body=@$body_file" >/dev/null ||
    warn "the pull request comment could not be created"
fi
exit 0
