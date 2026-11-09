#!/usr/bin/env bash
# Prints the next ticket an agent should work on.
#
# Candidates are the union of two lists, tried in this order:
#   1. the `- [ ] #N` task-list lines of the tracking issue (label: tracking),
#      in list order;
#   2. every other open issue, lowest issue number first. Workers file
#      follow-up issues straight with `gh issue create` (AGENTS.md §3) and
#      tick-tracking.sh only ticks lines that already exist, so without this
#      second pass a worker-filed issue never reaches the loop.
# The tracking issue itself is never a candidate.
#
# A candidate is ready when it is
#   - open
#   - not labeled in-progress, blocked, or needs-split
#   - every issue in its "Depends on:" line is closed
#
# Usage: scripts/next-ticket.sh            # prints "#123 <title>" then the body
#        scripts/next-ticket.sh --number   # prints just the number
set -euo pipefail

tracking=$(gh issue list --label tracking --state open --limit 1 --json number --jq '.[0].number')
if [[ -z "$tracking" ]]; then
  echo "No open tracking issue found (label: tracking)." >&2
  exit 1
fi

# Task list lines look like: "- [ ] #123 M1-04: title"
mapfile -t listed < <(gh issue view "$tracking" --json body --jq '.body' \
  | grep -oE '^- \[[ x]\] #[0-9]+' | grep -oE '[0-9]+$')

# Fetch all issue states once (one API call, not one per issue). The unlisted
# pass below reads the same snapshot, so it sees at most the 1000 issues gh
# returns here.
states=$(gh issue list --state all --limit 1000 --json number,state,labels \
  --jq '.[] | "\(.number) \(.state) \([.labels[].name] | join(","))"')

state_of() { awk -v n="$1" '$1==n {print $2}' <<<"$states"; }
labels_of() { awk -v n="$1" '$1==n {print $3}' <<<"$states"; }

# Open issues from that snapshot that are neither the tracking issue nor
# already on the task list, lowest number first. Label and dependency filters
# live in the loop below so they are spelled only once, and only candidates
# that survive those cheap filters pay for a body fetch.
mapfile -t unlisted < <(awk -v tracking="$tracking" -v listed="${listed[*]-}" '
    BEGIN {
      count = split(listed, line, " ")
      for (i = 1; i <= count; i++) on_list[line[i]] = 1
    }
    $2 == "OPEN" && $1 != tracking && !($1 in on_list) { print $1 }' <<<"$states" \
  | sort -n)

for n in "${listed[@]}" "${unlisted[@]}"; do
  [[ "$(state_of "$n")" == "OPEN" ]] || continue
  labels=",$(labels_of "$n"),"
  [[ "$labels" == *",in-progress,"* || "$labels" == *",blocked,"* || "$labels" == *",needs-split,"* ]] && continue

  body=$(gh issue view "$n" --json body --jq '.body')
  deps=$(grep -m1 -iE '^\*\*Depends on:\*\*' <<<"$body" | grep -oE '#[0-9]+' | tr -d '#' || true)
  ready=1
  for d in $deps; do
    if [[ "$(state_of "$d")" != "CLOSED" ]]; then ready=0; break; fi
  done
  [[ $ready -eq 1 ]] || continue

  if [[ "${1:-}" == "--number" ]]; then
    echo "$n"
  else
    gh issue view "$n" --json number,title,body --jq '"#\(.number) \(.title)\n\n\(.body)"'
  fi
  exit 0
done

echo "Nothing ready. Everything is done, in progress, or blocked on open dependencies." >&2
exit 2
