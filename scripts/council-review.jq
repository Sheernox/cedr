# Indented Markdown keeps reviewer-provided text inert in summaries and review comments.
def code: gsub("\r\n?"; "\n") | split("\n") | map("    " + .) | join("\n");
def finding:
  "  \(.member) \(.severity) :\(.line) \(.claim)"
  + (if .scenario then "\n    Scenario: \(.scenario)" else "" end);
def group:
  "\(.severity) \(.file):\(.line)\n" + ([.findings[] | finding] | join("\n"));
def overview:
  "Council: \(.gate) (\(.reviewers | length) of quorum \(.quorum) reviewed, \(.block) and above block)."
  + (if (.reviewers | length) > 0 then
       .scores as $scores
       | "\nReviewers: " + (.reviewers | map(. as $member
           | . + (if $scores[$member] != null then " (\($scores[$member])/10)" else "" end)) | join(", "))
     else "" end)
  + (if (.spares | length) > 0 then
       "\nSpares: " + (.spares | to_entries | map("\(.key) for \(.value)") | join(", "))
     else "" end)
  + (if (.incomplete_reports | length) > 0
     then "\nIncomplete reports: \(.incomplete_reports | join(", "))." else "" end)
  + "\nNext: \(.next).";
def group_text:
  if length > 0 then map(group) | join("\n\n") else "No findings." end;
def in_diff($locations):
  $locations[.file].lines[(.line | tostring)] == true;

# Read only present right-side lines. Deletions consume left-side line numbers, and a truncated
# patch may stop before the hunk's advertised right-side range ends.
def right_lines:
  reduce (split("\n")[]) as $line (
    {next: 0, end: 0, lines: {}};
    if ($line | startswith("@@ ")) then
      ([$line | capture("^@@ -[0-9]+(?:,[0-9]+)? \\+(?<start>[0-9]+)(?:,(?<count>[0-9]+))? @@")][0]) as $hunk
      | if $hunk then
          .next = ($hunk.start | tonumber)
          | .end = (.next + ($hunk.count // "1" | tonumber))
        else .end = 0 end
    elif .next < .end and ($line | test("^[ +]")) then
      .lines[(.next | tostring)] = true | .next += 1
    else . end
  ) | .lines;

if $mode == "summary" then
  "## Cedr Council\n\n" + ((overview + "\n\n" + (.groups | group_text)) | code)
else
  (INDEX($files[][] | select(.patch != null); .filename)) as $patches
  | (INDEX(.groups | unique_by(.file)[] | .file as $name
       | {name: $name, lines: ($patches[$name].patch // "" | right_lines)}; .name)) as $locations
  | (.groups | map(select(in_diff($locations) | not))) as $outside
  | {event: "COMMENT", commit_id: $head,
     body: ($marker + "\n## Cedr Council\n\n" + (overview | code)
            + (if ($outside | length) == 0 then ""
               else "\n\n### Findings outside the diff\n\n" + ($outside | group_text | code) end)),
     comments: [.groups[] | select(in_diff($locations))
                | {path: .file, line, side: "RIGHT", body: ("**Council finding**\n\n" + (group | code))}]}
  | .body |= if length > 65000 then .[:65000] + "\n\nSee the Actions job summary for the remaining findings." else . end
  | .comments[].body |= if length > 65000 then .[:65000] + "\n\nSee the Actions job summary for the full finding." else . end
end
