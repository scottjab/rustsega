#!/usr/bin/env bash
# Ticks the checkbox for every closed issue in the tracking issue's task list.
# Safe to run any time. Usage: scripts/tick-tracking.sh
set -euo pipefail
tracking=$(gh issue list --label tracking --state open --limit 1 --json number --jq '.[0].number')
closed=$(gh issue list --state closed --limit 1000 --json number --jq '.[].number')
body=$(gh issue view "$tracking" --json body --jq '.body')
for n in $closed; do
  body=$(sed -E "s/^- \[ \] #${n}([^0-9]|$)/- [x] #${n}\1/" <<<"$body")
done
gh issue edit "$tracking" --body "$body" >/dev/null
echo "Tracking issue #$tracking updated."
