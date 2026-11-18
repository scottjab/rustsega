#!/usr/bin/env bash
# Sweep the VDP-memory comparison over the commercial list, one resumable cart at a time.
#
# Part of the external reference oracle of #525; this is the #539 sweep. For each cart
# in `crates/genesis/tests/commercial_list.toml` it runs `run-ref.sh --vdp` and
# `run-ours.sh --vdp` over a short frame window, then `compare.py --vdp --tsv`, and
# records one row per cart: frames, shift, mean VRAM bytes differing per frame, the
# first differing byte, the span and run shape of the divergence (which is how
# contiguous it is), the CRAM colours differing, and the verdict of the
# real-versus-phase screen. See scripts/ref/README.md for what those mean.
#
# usage: sweep-vdp.sh [--frames A-B] [--sweep N] [--limit N] [--only key[,key...]]
#                     [--force] [--redump] [--phase-safe] [--anchor key=ADDR] [--dry-run] [key ...]
#
# `--frames` is the window dumped for every cart. Without it each cart uses the last 16
# frames of the run its own `frames` entry in the list describes, which is where the
# smoke tests say the cart has a picture worth comparing. A window has to be wider than
# the `--sweep` on both sides or the alignment sweep runs out of images.
#
# `--only` and trailing key arguments limit the run to named carts; `--limit N` stops
# after N carts. That is the "a handful at a time" the ticket asks for, and it is not
# about throughput: blastem's debugger talks over fixed FIFOs in /tmp, so one reference
# run at a time machine-wide whether anyone means to or not.
#
# Resumable, because blastem costs minutes per cart. A cart with a row in
# `target/ref/vdp-sweep/results.tsv` is skipped, and a cart whose dumps are on disk for
# the requested window skips straight to the comparison - so an interrupted sweep
# resumes rather than restarting, and `--only` can be pointed at one cart without
# touching the rest. `--force` redoes a cart that already has a row, which with the
# dumps still on disk means a re-compare and nothing else; `--redump` is what pays
# blastem again.
#
# Nothing here changes the emulator or a golden. The rows and the dumps live under
# `target/`, which is gitignored; the doc table gets written by hand from them.
#
# Environment:
#   DISPLAY        blastem needs a real X display; default :0 (README, caveats)
#   RUSTSEGA_ROM_DIR  the cartridge collection, same as the commercial smoke tests
#   SWEEP_DIR      where the rows go (default <repo>/target/ref/vdp-sweep)
set -euo pipefail

self_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=/dev/null
. "$self_dir/common.sh"   # ref_dump_dir

repo=$(cd -- "$self_dir/../.." && pwd)
rom_dir=${RUSTSEGA_ROM_DIR:-$HOME/games/roms/genesis}
state=${SWEEP_DIR:-$repo/target/ref/vdp-sweep}
list=$repo/crates/genesis/tests/commercial_list.toml
results=$state/results.tsv
window=""
sweep=4
limit=0
only=()
force=0
redump=0
dry=0
anchors=()
phase_safe=0

usage() {
  echo "usage: $(basename "$0") [--frames A-B] [--sweep N] [--limit N] [--only key[,key...]] [--force] [--redump] [--phase-safe] [--anchor key=ADDR] [--dry-run] [key ...]" >&2
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --frames)
      [[ $# -ge 2 ]] || usage
      window=$2
      shift 2
      ;;
    --sweep)
      [[ $# -ge 2 ]] || usage
      sweep=$2
      shift 2
      ;;
    --limit)
      [[ $# -ge 2 ]] || usage
      limit=$2
      shift 2
      ;;
    --only)
      [[ $# -ge 2 ]] || usage
      IFS=, read -r -a picked <<<"$2"
      only+=("${picked[@]}")
      shift 2
      ;;
    --anchor)
      [[ $# -ge 2 ]] || usage
      anchors+=("$2")
      shift 2
      ;;
    --phase-safe)
      phase_safe=1
      shift
      ;;
    --force)
      force=1
      shift
      ;;
    --redump)
      redump=1
      shift
      ;;
    --dry-run)
      dry=1
      shift
      ;;
    -*)
      echo "$(basename "$0"): unknown option $1" >&2
      usage
      ;;
    *)
      only+=("$1")
      shift
      ;;
  esac
done

# blastem is a GUI emulator. With no DISPLAY it dies on `Unable to init SDL`, and
# under xvfb-run it blocks on a zenity dialog about vsync, so default to the
# session's own display rather than either of those (README, caveats).
export DISPLAY=${DISPLAY:-:0}

mkdir -p "$state"
[[ -r $list ]] || { echo "sweep-vdp: cannot read $list" >&2; exit 2; }

# A phase-safe pass writes its own rows rather than overwriting the end-of-frame ones,
# because the whole point of the second pass is that the two may disagree: a divergence
# that was there at every frame's end and not at one shared instruction was frame phase
# wearing a block costume, and that is only visible if both rows survive (#539).
if ((phase_safe == 1)); then
  results=$state/results-anchored.tsv
fi

# The cart list, tab-separated: key, prefix, frames. Parsed out of the smoke tests'
# own list rather than restated here, because a second list is a second thing that
# goes out of date and then the sweep quietly measures a different set of carts from
# the one the tests do.
mapfile -t carts < <(
  python3 - "$list" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as handle:
    games = tomllib.load(handle)["game"]
for game in games:
    print("\t".join([game["key"], game["prefix"], str(game["frames"])]))
PY
)

# The columns of the row file: the per-cart summary, one row per cart. The VRAM
# fields are the `--vdp` line of `compare.py --tsv` and the CRAM fields the CRAM line,
# so the numbers here are the comparator's and not a restatement of them.
header=$'key\tname\tframes\tshift\tvram_mean\tvram_first\tvram_span\tvram_runs\tvram_largest\tcram_mean\tcram_persistent\tcram_verdict\tverdict\tphase_safe'
if [[ ! -f $results ]]; then
  mkdir -p "$state"
  printf '%s\n' "$header" >"$results"
fi

# row_for <key> <name> - the stored row for a cart, if any.
row_for() {
  awk -F '\t' -v key="$1" '$1 == key { found = 1; print; exit } END { exit !found }' "$results"
}

# row_put <row> - replace this cart's row, or append it. So a `--force` re-run and a
# phase-safe pass both update one row rather than leaving two opinions about a cart.
row_put() {
  local row=$1
  local key=${row%%$'\t'*}
  local tmp=$results.tmp
  awk -F '\t' -v key="$key" '$1 != key' "$results" >"$tmp"
  printf '%s\n' "$row" >>"$tmp"
  mv "$tmp" "$results"
}

# anchor_for <key> - the `--stop-at-pc` address given with --anchor for this cart, if any.
anchor_for() {
  local entry
  for entry in "${anchors[@]:-}"; do
    [[ -n $entry ]] || continue
    if [[ ${entry%%=*} == "$1" ]]; then
      printf '%s' "${entry#*=}"
      return 0
    fi
  done
  return 1
}

# one <field> <line> - the field of a `compare.py --tsv` line, 1-based.
one() {
  printf '%s' "$2" | cut -f"$1"
}

count=0
skipped=0
for cart in "${carts[@]}"; do
  IFS=$'\t' read -r key prefix frames <<<"$cart"

  if ((${#only[@]} > 0)); then
    wanted=0
    for pick in "${only[@]}"; do
      [[ $pick == "$key" ]] && wanted=1
    done
    ((wanted == 1)) || continue
  fi

  if ((phase_safe == 0)) && ((force == 0)) && row_for "$key" >/dev/null; then
    skipped=$((skipped + 1))
    continue
  fi

  rom=""
  for ext in md bin gen smd; do
    if [[ -f $rom_dir/$prefix.$ext ]]; then
      rom=$rom_dir/$prefix.$ext
      break
    fi
  done

  if ((limit > 0 && count >= limit)); then
    break
  fi
  count=$((count + 1))

  if [[ -z $rom ]]; then
    echo "sweep-vdp: $key: no $prefix in $rom_dir" >&2
    row_put "$(printf '%s\t%s\t-\t-\t-\t-\t-\t-\t-\t-\t-\tno-rom\t-' "$key" "$prefix")"
    continue
  fi

  # The window: the tail of the cart's own smoke-test run, so the cart is doing the
  # same thing the tests say it is doing at that frame. 16 frames is wider than the
  # alignment sweep needs at its default span of 4 either way.
  this_window=$window
  [[ -n $this_window ]] || this_window=$((frames - 15 + 1))-$frames

  dir=$(ref_dump_dir "$repo" "$rom" "")
  marker=$state/$key.dumped
  if ((redump == 0)) && [[ -d $dir && $(cat "$marker" 2>/dev/null || true) == "$this_window" ]]; then
    echo "sweep-vdp: $key: dumps for $this_window already in $dir"
  else
    echo "sweep-vdp: $key: dumping both sides at --vdp $this_window"
    if ((dry == 1)); then
      echo "sweep-vdp: $key: would run run-ref.sh/run-ours.sh on $rom then compare.py --vdp"
      continue
    fi
    if ! "$self_dir/run-ref.sh" --vdp "$rom" "$this_window"; then
      echo "sweep-vdp: $key: the reference half failed; row not written, rerun to retry" >&2
      continue
    fi
    if ! "$self_dir/run-ours.sh" --vdp "$rom" "$this_window"; then
      echo "sweep-vdp: $key: our half failed; row not written, rerun to retry" >&2
      continue
    fi
    printf '%s\n' "$this_window" >"$marker"
  fi

  # The phase-safe pass: the same window at one shared instruction, for the carts the
  # end-of-frame screen flagged. It replaces the verdict rather than adding to it, and
  # a divergence that survives it is the kind an issue gets filed for (#539).
  phase_safe_row="-"
  if ((phase_safe == 1)); then
    if ! anchor=$(anchor_for "$key"); then
      echo "sweep-vdp: $key: --phase-safe wants --anchor $key=0xADDR (see README: the cart's own V-Int vector)" >&2
      continue
    fi
    anchor_dir=$(ref_dump_dir "$repo" "$rom" "$anchor")
    if [[ ! $(cat "$state/$key.anchor.dumped" 2>/dev/null || true) == "$this_window" ]]; then
      echo "sweep-vdp: $key: re-running phase-safe at 0x$anchor"
      if ! "$self_dir/run-ref.sh" --vdp --stop-at-pc "$anchor" "$rom" "$this_window"; then
        echo "sweep-vdp: $key: the anchored reference half failed: is 0x$anchor reached once a frame?" >&2
        continue
      fi
      if ! "$self_dir/run-ours.sh" --vdp --stop-at-pc "$anchor" "$rom" "$this_window"; then
        echo "sweep-vdp: $key: the anchored half of ours failed: is 0x$anchor reached once a frame?" >&2
        continue
      fi
      printf '%s\n' "$this_window" >"$state/$key.anchor.dumped"
    fi
    dir=$anchor_dir
    phase_safe_row=anchored@"$anchor"
  fi

  # compare.py exits 1 when the two machines disagree, which is a result here rather
  # than a failure of the run, so only status 2 (the harness could not run) stops a cart.
  # At an anchor there is no frame-counter offset to measure: both machines stopped at
  # one instruction, so the comparison is at shift 0 and a non-zero shift *there* is a
  # finding the sweep would otherwise eat. `--anchor` labels the report the same way.
  at_sweep=$sweep
  at_anchor=()
  if ((phase_safe == 1)); then
    at_sweep=0
    at_anchor=(--anchor "$anchor")
  fi
  tsv=$(mktemp)
  trap 'rm -f "$tsv"' EXIT
  status=0
  python3 "$self_dir/compare.py" "$dir" "$this_window" --vdp --sweep "$at_sweep" \
    "${at_anchor[@]}" --tsv >"$tsv" || status=$?
  if ((status > 1)); then
    echo "sweep-vdp: $key: compare.py said the harness could not run (status $status)" >&2
    continue
  fi
  vram=$(awk -F '\t' '$2 == "VRAM" { print; exit }' "$tsv")
  cram=$(awk -F '\t' '$2 == "CRAM" { print; exit }' "$tsv")
  rm -f "$tsv"
  if [[ -z $vram || -z $cram ]]; then
    echo "sweep-vdp: $key: compare.py printed no VRAM and CRAM line, row not written" >&2
    continue
  fi

  # The row, in the order of $header. Fields 3/5/6/8/9/10/12 of the VRAM line are the
  # frames, shift, mean, run count, widest run, span and verdict; CRAM contributes its
  # mean and its persistent colour count. Nothing is recomputed here.
  # The cart-level verdict is the worse of the two memories: a cart whose VRAM matches
  # but whose palette diverges is not a cart that agrees, and the first version of this
  # row took the VRAM verdict alone and called Streets of Rage 2, which had two moving
  # CRAM colours in every frame, `agree`.
  vram_verdict=$(one 12 "$vram")
  cram_verdict=$(one 12 "$cram")
  verdict=$vram_verdict
  case $cram_verdict:$vram_verdict in
    real:*) verdict=real ;;
    phase:agree) verdict=phase ;;
  esac
  row=$(printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s' \
    "$key" "$prefix" \
    "$(one 3 "$vram")" "$(one 5 "$vram")" "$(one 6 "$vram")" "$(one 11 "$vram")" \
    "$(one 10 "$vram")" "$(one 8 "$vram")" "$(one 9 "$vram")" \
    "$(one 6 "$cram")" "$(one 7 "$cram")" "$cram_verdict" "$verdict" "$phase_safe_row")
  row_put "$row"
  echo "sweep-vdp: $key: $(printf '%s' "$row" | cut -f3-)"
done

echo "sweep-vdp: ran $count cart(s), $skipped already had a row; rows in $results"
