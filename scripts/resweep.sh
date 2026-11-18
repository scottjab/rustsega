#!/usr/bin/env bash
# Re-measure named carts with `headless sweep` and fold the new rows into a committed report.
#
# `docs/compat.csv` is generated: `headless sweep` over the whole collection, one row per
# cart, with `status`, `notes` and the Boots/Blank decision made by `sweep::classify`. A
# ticket that re-measures a subset of those carts therefore has two ways to update the
# table: run the generator over just those carts and merge, or read numbers off
# `headless run` and write the rows by hand. This script is the first one. A hand-written
# row is a row nobody can re-derive, and the moment one says something `classify` would
# not have said the report has started lying about the machine.
#
# usage: scripts/resweep.sh <cart-list> [--rom-dir DIR] [--csv PATH]
#                          [--frames N] [--run-frames N] [--timeout-secs N] [--jobs N]
#                          [--out PATH] [--dry-run]
#
# `<cart-list>` is one cart file name per line - the `file` column of the CSV, never a
# path - and `#` starts a comment. Generate one from a report with, e.g.:
#
#   python3 -c 'import csv; [print(r["file"]) for r in csv.DictReader(open("docs/compat.csv"))
#              if "has not raised" in r["notes"]]' > /tmp/carts.txt
#   scripts/resweep.sh /tmp/carts.txt
#
# Names resolve against the ROM dir (`--rom-dir`, else `$RUSTSEGA_ROM_DIR`, else
# `~/games/roms/genesis`), the same discovery the sweep and the smoke tests use, so no
# cart path lands in a report or a log.
#
# Frames default to the committed sweep's settings (600-frame checkpoint, 1800-frame
# watched run, 30s wall clock per cart), because a row that means something against
# `docs/compat.csv` has to be the same measurement as its neighbours. `--frames`/
# `--run-frames`/`--timeout-secs`/`--jobs` pass straight to `headless sweep`.
#
# The merge rewrites only the rows it measured, keyed on `file`:
#
#   - a name in the sweep output that is not in the target CSV is an error, not an insert;
#   - a name in `<cart-list>` that is not on disk is reported and left alone, so a cart
#     nobody has locally keeps its stale row rather than quietly becoming a pass;
#   - the row count, the header and the file-name order have to survive, and the script
#     stops if any of them does not.
#
# `--dry-run` writes the measured rows to `--out` (default
# `target/resweep/rows.csv`, which is under gitignored `target/`) and touches nothing.
#
# Nothing here regenerates `docs/compat.md`: `headless sweep --markdown` emits only the
# header, the counts and the problem tables, and the committed page carries hand-written
# triage between them. Those tables are spliced by hand from the merged CSV, which is
# what makes the merge above worth scripting in the first place.
#
# No emulator behaviour, no golden, no ROM read into git.
set -euo pipefail

usage() { sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; }

cart_list=""
rom_dir="${RUSTSEGA_ROM_DIR:-$HOME/games/roms/genesis}"
csv=docs/compat.csv
sweep_args=()
out=target/resweep/rows.csv
dry_run=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rom-dir) rom_dir="$2"; shift 2 ;;
    --csv) csv="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --frames | --run-frames | --timeout-secs | --jobs)
      sweep_args+=("$1" "$2")
      shift 2
      ;;
    --dry-run) dry_run=1; shift ;;
    -h | --help)
      usage
      exit 0
      ;;
    -*)
      echo "resweep: unknown flag $1" >&2
      exit 2
      ;;
    *)
      [[ -n "$cart_list" ]] && {
        echo "resweep: one cart list at a time" >&2
        exit 2
      }
      cart_list="$1"
      shift
      ;;
  esac
done

if [[ -z "$cart_list" || ! -f "$cart_list" ]]; then
  echo "resweep: <cart-list> is a file of cart file names, one per line" >&2
  exit 2
fi
if [[ ! -f "$csv" ]]; then
  echo "resweep: no report to merge into at $csv (--csv, or --dry-run)" >&2
  exit 2
fi

exe="${HEADLESS_EXE:-}"
if [[ -z "$exe" ]]; then
  # The sweep re-runs its own executable per cart, so building first is what makes
  # a resweep a measurement of this tree rather than of whatever was last built.
  cargo build --release -p headless
  exe=target/release/headless
fi

mkdir -p "$(dirname "$out")"
work="$(dirname "$out")/dir-$$"
rm -rf "$work"
mkdir -p "$work"
trap 'rm -rf "$work"' EXIT

# Symlinks, not copies: the collection is gigabytes, `collect_roms` follows a symlink
# to a file, and nothing below needs to know the carts live somewhere else.
wanted=0
missing=0
while IFS= read -r line; do
  name="${line%%#*}"
  name="${name#"${name%%[![:space:]]*}"}"
  name="${name%"${name##*[![:space:]]}"}"
  [[ -z "$name" ]] && continue
  wanted=$((wanted + 1))
  if [[ -e "$rom_dir/$name" ]]; then
    ln -s "$rom_dir/$name" "$work/$name"
  else
    missing=$((missing + 1))
    echo "resweep: not on disk, row left alone: $name" >&2
  fi
done <"$cart_list"

if [[ $wanted -eq $missing ]]; then
  echo "resweep: none of the $wanted carts is in $rom_dir, nothing measured" >&2
  exit 1
fi

"$exe" sweep "$work" "${sweep_args[@]}" --csv "$out"

if [[ $dry_run -eq 1 ]]; then
  echo "resweep: rows written to $out, $csv untouched (--dry-run)"
  exit 0
fi

out="$out" csv="$csv" python3 - <<'PY'
"""Fold the measured rows into the report, keyed on `file`.

Every way this can go wrong is an exit rather than a warning: the report is a
committed claim about 2775 carts, and a merge that silently dropped one is worse
than a merge that refused to run.
"""

import csv
import os
import sys

rows_path = os.environ["out"]
csv_path = os.environ["csv"]

with open(csv_path, newline="") as handle:
    reader = csv.reader(handle)
    header = next(reader)
    old_rows = list(reader)
index = {row[0]: position for position, row in enumerate(old_rows)}
if len(index) != len(old_rows):
    sys.exit(f"resweep: {csv_path} names one cart twice, refusing to merge")

with open(rows_path, newline="") as handle:
    reader = csv.reader(handle)
    measured_header = next(reader)
    measured = list(reader)

if measured_header != header:
    sys.exit(
        f"resweep: {rows_path} was written with columns {measured_header}, "
        f"{csv_path} has {header}; regenerate rather than merge"
    )

transitions = {}
for row in measured:
    position = index.get(row[0])
    if position is None:
        sys.exit(f"resweep: {row[0]!r} is not a cart of {csv_path}, refusing to insert it")
    before = old_rows[position][header.index("status")]
    after = row[header.index("status")]
    transitions[(before, after)] = transitions.get((before, after), 0) + 1
    old_rows[position] = row

if len(old_rows) != len(index):
    sys.exit("resweep: the merge changed the number of rows, refusing to write")

with open(csv_path, "w", newline="") as handle:
    writer = csv.writer(handle, lineterminator="\n")
    writer.writerow(header)
    writer.writerows(old_rows)

print(f"resweep: {len(measured)} rows folded into {csv_path}")
for (before, after), count in sorted(transitions.items()):
    if before != after:
        print(f"resweep:   {before} -> {after}: {count}")
    else:
        print(f"resweep:   {before} -> {after}: {count} (status unchanged, numbers refreshed)")
PY
