#!/usr/bin/env bash
# No vendor CLI, GitHub credentials, or network is needed. Run with bash scripts/test-council-action.sh.
set -euo pipefail

script="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/council-action.sh"
tmp=$(mktemp -d)
trap 'rm -r -- "$tmp"' EXIT
export TMP_FIXTURE="$tmp"
mkdir "$tmp/bin"
export PATH="$tmp/bin:$PATH"

cat >"$tmp/bin/cedr" <<'STUB'
#!/usr/bin/env bash
set -eu
test -z "${GH_TOKEN:-}"
echo run >>"$TMP_FIXTURE/calls"
printf '%s\n' "$@" >"$TMP_FIXTURE/args"
while [ "$1" != --report-json ]; do shift; done
cat >"$2" <<'REPORT'
{"gate":"block","block":"P1","quorum":2,"reviewers":["a","b"],"next":"revise",
 "incomplete_reports":[],
 "groups":[
   {"file":"src/a","line":13,"severity":"P1","findings":[{"member":"a","line":13,"claim":"Inline regression","severity":"P1"},{"member":"b","line":14,"claim":"Related concern","severity":"P2","scenario":"With input B"}]},
   {"file":"src/a","line":20,"severity":"P2","findings":[{"member":"b","line":20,"claim":"Off-diff context","severity":"P2"}]},
   {"file":"src/a","line":42,"severity":"P2","findings":[{"member":"a","line":42,"claim":"Truncated hunk","severity":"P2"}]},
   {"file":"src/old","line":1,"severity":"P2","findings":[{"member":"a","line":1,"claim":"Removed line","severity":"P2"}]},
   {"file":"src/b","line":9,"severity":"P1","findings":[{"member":"a","line":9,"claim":"Second page","severity":"P1"}]}
 ]}
REPORT
echo "::error file=src/a,line=13,title=P1::Inline regression"
exit "${TEST_GATE_CODE:-1}"
STUB
cat >"$tmp/bin/gh" <<'STUB'
#!/usr/bin/env bash
set -eu
test "$GH_TOKEN" = stub-token
test "$1" = api
shift
echo "$*" >>"$TMP_FIXTURE/calls"
case "$*" in
  'repos/example/repo/pulls/7/files?per_page=100 --paginate')
    # Include a deleted hunk and one with fewer visible lines than its header advertises.
    cat <<'FILES'
[{"filename":"src/a","patch":"@@ -12,2 +12,3 @@\n context\n-old\n+new\n+added\n@@ -40,3 +40,3 @@\n one"},
 {"filename":"src/old","patch":"@@ -1 +0,0 @@\n-removed"}]
[{"filename":"src/b","patch":"@@ -9 +9 @@\n-old\n+new"}]
FILES
    ;;
  'repos/example/repo/pulls/7/reviews?per_page=100 --paginate')
    cat <<'REVIEWS'
[{"id":2,"user":{"login":"github-actions[bot]"},"state":"COMMENTED","body":"<!-- cedr-council -->\n## Previous review"},
 {"id":3,"user":{"login":"github-actions[bot]"},"state":"COMMENTED","body":"Superseded by a newer cedr Council run.\n\n<!-- cedr-council -->\n## Even older"},
 {"id":4,"user":{"login":"someone"},"state":"COMMENTED","body":"<!-- cedr-council -->\nUser review"},
 {"id":5,"user":{"login":"github-actions[bot]"},"state":"COMMENTED","body":"Other bot review"}]
REVIEWS
    ;;
  'repos/example/repo/pulls/7 --jq .head.sha')
    echo "${CURRENT_HEAD:-0123456789abcdef0123456789abcdef01234567}" ;;
  '-X POST repos/example/repo/pulls/7/reviews --input '*)
    test "${FAIL_POST:-false}" = false
    cp "$5" "$TMP_FIXTURE/new-review" ;;
  '-X PUT repos/example/repo/pulls/7/reviews/2 --input '*)
    test -f "$TMP_FIXTURE/new-review"
    cp "$5" "$TMP_FIXTURE/superseded" ;;
  *) echo "unexpected call: $*" >&2; exit 76 ;;
esac
STUB
chmod +x "$tmp/bin/cedr" "$tmp/bin/gh"

export INPUT_COUNCIL=review.release COUNCIL_EVENT=pull_request COUNCIL_PR=7
export COUNCIL_BASE_REPO=example/repo COUNCIL_BASE_REF=main
export COUNCIL_HEAD=0123456789abcdef0123456789abcdef01234567
export INPUT_PATH="$tmp" GITHUB_STEP_SUMMARY="$tmp/summary" GH_TOKEN=stub-token

COUNCIL_HEAD_REPO=other/repo bash "$script" >"$tmp/log"
grep -q '^::notice .*outside the base repository' "$tmp/log"
test ! -e "$tmp/calls"

export COUNCIL_HEAD_REPO="$COUNCIL_BASE_REPO"
status=0
bash "$script" >"$tmp/log" || status=$?
test "$status" -eq 1
test "$(grep -cx run "$tmp/calls")" -eq 1
grep -qx -- "--head=$COUNCIL_HEAD" "$tmp/args"
grep -qx -- '--base=refs/remotes/origin/main' "$tmp/args"
grep -qx -- '--require-api-keys' "$tmp/args"
grep -qx -- '900' "$tmp/args"
grep -qx -- 'github' "$tmp/args"
grep -q '^::error .*Inline regression' "$tmp/log"
grep -q 'Council: block (2 of quorum 2 reviewed' "$tmp/summary"
grep -q 'Inline regression' "$tmp/summary"
grep -q 'Off-diff context' "$tmp/summary"
jq -e --arg head "$COUNCIL_HEAD" '
  .event == "COMMENT" and .commit_id == $head and
  ([.comments[] | [.path, .line, .side]] == [["src/a", 13, "RIGHT"], ["src/b", 9, "RIGHT"]]) and
  (.comments[0].body | contains("Related concern") and contains("Scenario: With input B")) and
  (.body | startswith("<!-- cedr-council -->\n") and contains("Off-diff context") and
    contains("Truncated hunk") and contains("Removed line") and (contains("Inline regression") | not))
' "$tmp/new-review" >/dev/null
jq -e '.body == "Superseded by a newer cedr Council run.\n\n<!-- cedr-council -->\n## Previous review"' \
  "$tmp/superseded" >/dev/null
test "$(grep -c '^-X POST ' "$tmp/calls")" -eq 1
test "$(grep -c '^-X PUT ' "$tmp/calls")" -eq 1

: >"$tmp/calls"
status=0
FAIL_POST=true TEST_GATE_CODE=3 bash "$script" >"$tmp/log" || status=$?
test "$status" -eq 3
grep -q '^::warning .*could not post' "$tmp/log"
if grep -q '^-X PUT ' "$tmp/calls"; then exit 1; fi

: >"$tmp/calls"
CURRENT_HEAD=another-commit TEST_GATE_CODE=0 bash "$script" >"$tmp/log"
grep -q '^::notice .*head changed' "$tmp/log"
if grep -q '^-X ' "$tmp/calls"; then exit 1; fi

for event in push workflow_dispatch; do
  : >"$tmp/calls"
  COUNCIL_EVENT="$event" INPUT_COUNCIL_BASE=before TEST_GATE_CODE=0 bash "$script" >"$tmp/log"
  grep -qx -- '--base=before' "$tmp/args"
  test "$(cat "$tmp/calls")" = run
done

: >"$tmp/calls"
INPUT_COUNCIL_COMMENT=false INPUT_COUNCIL_TIMEOUT=15 TEST_GATE_CODE=0 bash "$script" >"$tmp/log"
grep -qx -- '15' "$tmp/args"
test "$(cat "$tmp/calls")" = run

echo "Council Action tests passed"
