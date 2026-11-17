#!/usr/bin/env bash
# Dump our side of a work-RAM comparison: run a cartridge with no input and save
# its 68000 work RAM at named frames.
#
# The other half is scripts/ref/run-ref.sh, which does the same job in blastem;
# scripts/ref/compare.py then compares the two sets. See scripts/ref/README.md.
#
# usage: run-ours.sh [-o OUTDIR] [--region REGION] [--tmss] ROM FRAME [FRAME ...]
#
# Writes OUTDIR/rNNNNN.bin - one raw 64 KiB work-RAM image per named frame, the
# 68000 address space $FF0000-$FFFFFF, the same region and naming run-ref.sh
# produces. Frame N means the state after exactly N frames, counting from reset,
# on both sides.
#
# This is the `Genesis::new` + `run_frame()` x N + `Bus::ram` half of the oracle,
# run through crates/genesis/examples/ram_dump.rs. Nothing here is a debug build
# or a debug build of anything: the emulator is the one `cargo build --release`
# produces.
set -euo pipefail

self_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=/dev/null
. "$self_dir/common.sh"   # ref_out_dir, ref_frames

usage() {
  echo "usage: $(basename "$0") [-o OUTDIR] [--region auto|usa|japan|europe] [--tmss] ROM FRAME [FRAME ...]" >&2
  exit 2
}

out=""
rom=""
region=""
tmss=()
frames=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o)
      [[ $# -ge 2 ]] || usage
      out=$2
      shift 2
      ;;
    --region)
      [[ $# -ge 2 ]] || usage
      region=$2
      shift 2
      ;;
    --tmss)
      tmss=(--tmss)
      shift
      ;;
    -*)
      echo "$(basename "$0"): unknown option $1" >&2
      usage
      ;;
    *)
      if [[ -z $rom ]]; then
        rom=$1
      elif [[ $1 =~ ^[0-9]+$ ]]; then
        frames+=("$1")
      elif [[ $1 =~ ^([0-9]+)-([0-9]+)$ ]]; then
        # A range, so the two halves of a comparison are given the same argument and
        # cannot end up dumping different frame lists.
        while IFS= read -r ranged; do
          frames+=("$ranged")
        done < <(seq "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}")
      else
        echo "$(basename "$0"): $1 is not a frame number or range" >&2
        exit 2
      fi
      shift
      ;;
  esac
done

[[ -n $rom ]] || usage
[[ ${#frames[@]} -gt 0 ]] || usage
rom=$(readlink -f "$rom")
[[ -r $rom ]] || { echo "$(basename "$0"): cannot read $rom" >&2; exit 2; }

repo=$(cd -- "$self_dir/../.." && pwd)
out=${out:-$(ref_out_dir "$repo" "$rom")}
mkdir -p "$out"
out=$(readlink -f "$out")

# The example, built once for the whole list rather than once per frame. The
# target directory is cargo's default unless the caller moved it, which is the
# only way this path can be wrong and the reason it is not just `$repo/target`.
target=${CARGO_TARGET_DIR:-$repo/target}
bin=$target/release/examples/ram_dump
cargo build --quiet --manifest-path "$repo/Cargo.toml" --release -p genesis --example ram_dump

machine=()
[[ -z $region ]] || machine+=(--region "$region")
(( ${#tmss[@]} == 0 )) || machine+=("${tmss[@]}")

count=0
for frame in $(ref_frames "${frames[@]}"); do
  file=$out/$(printf 'r%05d.bin' "$frame")
  "$bin" "$rom" --frames "$frame" --out "$file" "${machine[@]}"
  count=$((count + 1))
done

echo "ours: $count work-RAM image(s) in $out"
