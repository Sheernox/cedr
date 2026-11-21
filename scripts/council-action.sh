#!/usr/bin/env bash
# Run reviewers once, then publish the same merge to the summary and (on PRs) GitHub.
# The composite action supplies INPUT_COUNCIL*, INPUT_PATH, COUNCIL_* and GH_TOKEN.
set -euo pipefail

notice() { echo "::notice title=cedr Council::$1"; }
warn() { echo "::warning title=cedr Council::$1"; }
fail() { echo "::error title=cedr Council::$1"; exit 2; }

if [ -z "${INPUT_COUNCIL:-}" ]; then exit 0; fi
case "${COUNCIL_EVENT:-}" in
  pull_request)
    if [ -z "${COUNCIL_HEAD_REPO:-}" ] || [ "$COUNCIL_HEAD_REPO" != "${COUNCIL_BASE_REPO:-}" ]; then
      notice "Council skipped: the pull request head is outside the base repository"
      exit 0
    fi
    ;;
  push|workflow_dispatch) ;;
  *) notice "Council skipped: requires pull_request, push or workflow_dispatch"; exit 0 ;;
esac

comment=${INPUT_COUNCIL_COMMENT:-true}
case "$comment" in true|false) ;; *) fail "council-comment must be true or false" ;; esac
command -v jq >/dev/null || fail "install jq on the runner to render the Council report"

jq_script="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/council-review.jq"
work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/cedr-council.XXXXXXXX")
trap 'rm -r -- "$work"' EXIT

cd -- "${INPUT_PATH:-.}"
base=${INPUT_COUNCIL_BASE:-}
args=(council run --format github --require-api-keys --timeout "${INPUT_COUNCIL_TIMEOUT:-900}"
      --out "$work/reports" --report-json "$work/merged.json")
if [ "$COUNCIL_EVENT" = pull_request ]; then
  [ -n "${COUNCIL_HEAD:-}" ] && [ -n "${COUNCIL_BASE_REF:-}" ] ||
    fail "the event must supply the pull request's head commit and base branch"
  # checkout normally leaves HEAD on GitHub's merge commit. Findings must use the PR head's lines.
  args+=("--head=$COUNCIL_HEAD")
  base=${base:-"refs/remotes/origin/$COUNCIL_BASE_REF"}
fi
if [ -n "$base" ]; then args+=("--base=$base"); fi

status=0
# Only the publishing code needs the GitHub token. Treat reviewer-derived diagnostics as data.
CEDR_TELEMETRY=0 CEDR_NO_AUTO_UPDATE=1 env -u GH_TOKEN -u GITHUB_TOKEN \
  cedr "${args[@]}" -- "$INPUT_COUNCIL" 2>"$work/diagnostics" || status=$?
jq -Rr '"cedr Council: " + (@json | gsub("##\\["; "## ["))' "$work/diagnostics" >&2
if [ ! -s "$work/merged.json" ]; then exit "$status"; fi
jq -r --arg mode summary --argjson files '[]' --arg head '' --arg marker '' -f "$jq_script" "$work/merged.json" \
  >> "${GITHUB_STEP_SUMMARY:-/dev/null}" || warn "could not write the job summary"

if [ "$COUNCIL_EVENT" != pull_request ] || [ "$comment" = false ]; then exit "$status"; fi
if ! command -v gh >/dev/null; then
  warn "install gh on the runner to post Council reviews"
  exit "$status"
fi
if ! [[ ${COUNCIL_PR:-} =~ ^[0-9]+$ ]]; then fail "the event must supply the pull request number"; fi

marker='<!-- cedr-council -->'
superseded='Superseded by a newer cedr Council run.'
pr="repos/$COUNCIL_BASE_REPO/pulls/$COUNCIL_PR"

if ! gh api "$pr/files?per_page=100" --paginate >"$work/files" 2>/dev/null; then
  warn "could not read the pull request diff; findings will go in the review body"
  printf '[]\n' >"$work/files"
fi
if ! gh api "$pr/reviews?per_page=100" --paginate >"$work/reviews" 2>/dev/null; then
  warn "could not list Council reviews; check pull-requests: write"
  exit "$status"
fi

if ! jq --arg mode review --slurpfile files "$work/files" --arg head "$COUNCIL_HEAD" \
    --arg marker "$marker" -f "$jq_script" "$work/merged.json" >"$work/review.json"; then
  warn "could not prepare the pull request review"
  exit "$status"
fi
if ! jq -c --arg marker "$marker" --arg superseded "$superseded" '
  .[] | select(.user.login == "github-actions[bot]" and .state == "COMMENTED")
  | select(.body | startswith($marker + "\n"))
  | {id, body: ($superseded + "\n\n" + .body[:64900])}
' "$work/reviews" >"$work/previous"; then
  warn "could not identify previous Council reviews"
  exit "$status"
fi

# The PR might have advanced while reviewers ran. Pin the review to the head we actually read.
if ! head=$(gh api "$pr" --jq .head.sha 2>/dev/null); then
  warn "could not verify the pull request's current head; review skipped"
  exit "$status"
fi
if [ "$head" != "$COUNCIL_HEAD" ]; then
  notice "review skipped: the pull request head changed during Council"
  exit "$status"
fi
if ! gh api -X POST "$pr/reviews" --input "$work/review.json" >/dev/null 2>&1; then
  warn "could not post the Council review; check pull-requests: write and the job summary"
  exit "$status"
fi

# Only supersede older reviews after the new one exists. Keep their original summaries.
while IFS= read -r review; do
  id=$(jq -r .id <<< "$review")
  jq 'del(.id)' <<< "$review" >"$work/update.json"
  gh api -X PUT "$pr/reviews/$id" --input "$work/update.json" >/dev/null 2>&1 ||
    warn "could not mark an older Council review as superseded"
done <"$work/previous"
exit "$status"
