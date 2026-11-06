#!/usr/bin/env bash
# Prints the next ticket an agent should work on.
#
# Reads the tracking issue (label: tracking), walks its task list in order,
# and prints the first issue that is:
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
mapfile -t candidates < <(gh issue view "$tracking" --json body --jq '.body' \
  | grep -oE '^- \[[ x]\] #[0-9]+' | grep -oE '[0-9]+$')

# Fetch all issue states once (one API call, not one per issue).
states=$(gh issue list --state all --limit 1000 --json number,state,labels \
  --jq '.[] | "\(.number) \(.state) \([.labels[].name] | join(","))"')

state_of() { awk -v n="$1" '$1==n {print $2}' <<<"$states"; }
labels_of() { awk -v n="$1" '$1==n {print $3}' <<<"$states"; }

for n in "${candidates[@]}"; do
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
