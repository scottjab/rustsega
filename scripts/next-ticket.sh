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
#   - every issue it depends on is closed
#
# Dependencies come from two places, in this order of trust:
#   1. GitHub's own blocking links, created with
#        gh issue edit <candidate> --add-blocked-by <dependency>
#      and read back from the `blockedBy` field of the single bulk issue call
#      below. Declare ordering there: it is data, so it cannot be misspelled and
#      cannot drift from the body text.
#   2. a "Depends on:" line in the issue body, read with or without the bold
#      markers and with any separator between the numbers it names
#      ("**Depends on:** #12, #34", "Depends on: #12 and #34"), because every
#      issue written before #504 spells its ordering that way.
#
# Nothing else in a body is obeyed, but nothing goes unsaid either. A candidate
# whose body reads like it declares a dependency the parser could not resolve,
# or whose dependency is not in the issue list it fetched, is still offered -
# but with a `next-ticket:` line on stderr naming exactly what could not be
# checked. Both cases were silent before #504, and a guard that cannot fire
# while the code reads as if it does is worse than no guard: the ticket gets
# taken anyway, and the agent working it cannot know a rule was skipped.
#
# Usage: scripts/next-ticket.sh            # prints "#123 <title>" then the body
#        scripts/next-ticket.sh --number   # prints just the number
# stdout is the ticket and nothing else, in both forms; every comment, warning
# and the "nothing ready" message goes to stderr.
set -euo pipefail

warn() { printf 'next-ticket: %s\n' "$*" >&2; }

# A body line declaring dependencies: "Depends on:" at the start of a line, in
# either case, with or without the markdown bold markers around it.
DEP_LINE_RE='^[[:space:]]*(\*\*)?[Dd]epends on'

# Dependency wording within a few words of an issue reference: how ordering looks
# when nobody used the line above. Reported, never obeyed - guessing at ordering
# is how a selection script starts skipping tickets for the wrong reason, and a
# wrong skip is invisible where a warning is not.
DEP_PROSE_RE='([Dd]epends? on|[Bb]locked by|[Gg]ated (on|by)|[Pp]rerequisite|[Rr]equires?|[Aa]fter)[^.#]{0,40}#[0-9]+'

# Issue numbers named on its stdin, one per line, without the '#'.
numbers_of() {
  grep -oE '#[0-9]+' | tr -d '#' || true
}

tracking=$(gh issue list --label tracking --state open --limit 1 --json number --jq '.[0].number')
if [[ -z "$tracking" ]]; then
  echo "No open tracking issue found (label: tracking)." >&2
  exit 1
fi

# Task list lines look like: "- [ ] #123 M1-04: title"
mapfile -t listed < <(gh issue view "$tracking" --json body --jq '.body' \
  | grep -oE '^- \[[ x]\] #[0-9]+' | grep -oE '[0-9]+$')

# Fetch all issue states, labels and blocking links once (one API call, not one
# per issue). The unlisted pass below reads the same snapshot, so it sees at most
# the 1000 issues gh returns here. Columns are tab-separated: an issue with no
# labels and no blocking links would otherwise have its columns folded together
# by awk's whitespace splitting.
states=$(gh issue list --state all --limit 1000 \
  --json number,state,labels,blockedBy \
  --jq '.[] | "\(.number)\t\(.state)\t\([.labels[].name] | join(","))\t\([.blockedBy.nodes[].number] | join(","))"')

state_of() { awk -F'\t' -v n="$1" '$1==n {print $2}' <<<"$states"; }
labels_of() { awk -F'\t' -v n="$1" '$1==n {print $3}' <<<"$states"; }
blocked_by() { awk -F'\t' -v n="$1" '$1==n {gsub(/,/, " ", $4); print $4}' <<<"$states"; }

# Dependencies of candidate $1, one space-separated set of numbers per line: the
# blocking links GitHub holds for it, then its body's "Depends on:" lines. Any
# dependency-shaped prose naming an issue none of those cover is warned about
# rather than guessed at (#504).
deps_of() {
  local n=$1 body=$2
  local line nums num
  local declared
  declared=" $(blocked_by "$n") "
  blocked_by "$n"

  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    nums=$(numbers_of <<<"$line" | tr '\n' ' ')
    if [[ -z "$nums" ]]; then
      warn "#$n: its \"Depends on\" line names no issue, so there is nothing to check; make the dependency one with: gh issue edit $n --add-blocked-by <number>"
      continue
    fi
    printf '%s\n' "$nums"
    declared="$declared$nums "
  done < <(grep -iE "$DEP_LINE_RE" <<<"$body" || true)

  while IFS= read -r num; do
    [[ -n "$num" ]] || continue
    [[ "$declared" == *" $num "* ]] && continue
    warn "#$n: its body puts dependency wording next to #$num without declaring it; make it a dependency with: gh issue edit $n --add-blocked-by $num"
  done < <(grep -oiE "$DEP_PROSE_RE" <<<"$body" | numbers_of | sort -un)

  return 0
}

# Open issues from that snapshot that are neither the tracking issue nor
# already on the task list, lowest number first. Label and dependency filters
# live in the loop below so they are spelled only once, and only candidates
# that survive those cheap filters pay for a body fetch.
mapfile -t unlisted < <(awk -F'\t' -v tracking="$tracking" -v listed="${listed[*]-}" '
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
  ready=1
  for d in $(deps_of "$n" "$body"); do
    dep_state=$(state_of "$d")
    if [[ -z "$dep_state" ]]; then
      warn "#$n: dependency #$d is not in the issue list gh returned (past --limit 1000?), so it cannot be checked; treating it as open"
      ready=0
      break
    fi
    if [[ "$dep_state" != "CLOSED" ]]; then
      ready=0
      break
    fi
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
