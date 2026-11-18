#!/usr/bin/env bash
# Dump the reference side of a work-RAM comparison: run a cartridge in blastem's
# debugger and save its 68000 work RAM at named frames.
#
# Part of the external reference oracle of #525 (see scripts/ref/README.md).
#
# usage: run-ref.sh [-o OUTDIR] [--stop-at-pc PC] [--vdp] ROM FRAME [FRAME ...] [-- BLASTEM-ARG ...]
#
# Writes OUTDIR/bNNNNN.bin - one raw 64 KiB work-RAM image per named frame, the
# 68000 address space $FF0000-$FFFFFF - and OUTDIR/blastem.log, the debugger
# transcript, which carries a `== frame N ==` marker and a `vdpregs` dump by each
# of them.
#
# --vdp dumps the VDP's own memories instead: bvNNNNN.bin, the 64 KiB of VRAM, and
# bcNNNNN.bin, the 64 colour words of CRAM as 128 raw bytes (#534). The reference
# reaches them through the `vdp:` namespace of its own debugger - `save "v.bin"
# vdp:vram` and `save/w "c.bin" vdp:cram` - so this needs no patch to blastem and
# takes nothing from its source. Note the `/w` on CRAM: the default element size
# narrows each colour to one byte and writes 64 of them, which compare.py then
# refuses as the half-width dump it is.
#
# Without --stop-at-pc the dump point is blastem's own end of frame, reached with
# `frames N`. That point is not the same place in the instruction stream as our
# end of frame, and #526 found the difference large enough to be mistaken for 25
# memory divergences. With --stop-at-pc PC the dump point is the Nth time the 68000
# is about to execute PC (`breakpoint PC` then N `continue`s), which is a point both
# machines can stop at exactly; run-ours.sh --stop-at-pc is the other half. Give it
# an address the cart reaches once a frame - see "The comparison point" in
# README.md.
#
# Blastem is a separate process, invoked by path ($REF_BLASTEM, default
# `blastem` from the nix dev shell). Nothing it produces is a build input to
# anything here and no blastem source or data is in this repository; see the GPL
# note in README.md.
#
# Environment:
#   REF_BLASTEM        blastem to run (default: `blastem` on PATH)
#   REF_TIMEOUT        wall seconds for the whole run (default 300)
#   REF_DRIVE_TIMEOUT  passed to terminal-standin.py (default 120)
set -euo pipefail

self_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=/dev/null
. "$self_dir/common.sh"   # ref_dump_dir, ref_frames

usage() {
  echo "usage: $(basename "$0") [-o OUTDIR] [--stop-at-pc PC] [--vdp] ROM FRAME [FRAME ...] [-- BLASTEM-ARG ...]" >&2
  exit 2
}

out=""
rom=""
stop_at=""
vdp=""
frames=()
extra=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o)
      [[ $# -ge 2 ]] || usage
      out=$2
      shift 2
      ;;
    --stop-at-pc)
      [[ $# -ge 2 ]] || usage
      stop_at=$2
      shift 2
      ;;
    --vdp)
      vdp=vdp
      shift
      ;;
    --)
      shift
      extra=("$@")
      break
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
        # A range, because compare.py takes one and retyping 595 596 597 ... is how
        # the two halves of a comparison end up with different frame lists.
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
if [[ -n $stop_at && ! $stop_at =~ ^(0x)?[0-9A-Fa-f]+$ ]]; then
  echo "$(basename "$0"): --stop-at-pc wants a hex address, not $stop_at" >&2
  exit 2
fi
rom=$(readlink -f "$rom")
[[ -r $rom ]] || { echo "$(basename "$0"): cannot read $rom" >&2; exit 2; }

repo=$(cd -- "$self_dir/../.." && pwd)
blastem=${REF_BLASTEM:-blastem}
command -v "$blastem" >/dev/null || {
  echo "$(basename "$0"): no blastem on PATH; run under 'nix develop' or set REF_BLASTEM" >&2
  exit 1
}

# Where the dumps go: the same default directory run-ours.sh writes to, so the
# two halves of one comparison meet in one place (common.sh).
mkdir -p "${out:-$(ref_dump_dir "$repo" "$rom" "$stop_at")}"
out=$(readlink -f "${out:-$(ref_dump_dir "$repo" "$rom" "$stop_at")}")

# The saves one frame produces, as `file-letter:size` pairs: the letter is what
# appears in the file name between the side and the frame number, and the size is
# what the way out checks against.
#
# Without --vdp that is work RAM, which is what this harness compared first. With
# it, the two memories of the VDP, reached through the reference's `vdp:` debugger
# namespace: `vdp:vram` is a byte array of the 64 KiB, and `vdp:cram` is 64 words,
# so CRAM is saved `/w` or it arrives as 64 narrowed bytes (README, #534).
if [[ -n $vdp ]]; then
  kinds=(v:65536 c:128)
else
  kinds=(:65536)
fi

# One `save` line for one frame of one memory. `kind` is the empty string for work
# RAM and the memory letter otherwise, matching the file names of run-ours.sh.
dump_line() {
  local kind=$1
  local frame=$2
  case $kind in
    v) printf 'save "bv%05d.bin" vdp:vram\n' "$frame" ;;
    c) printf 'save/w "bc%05d.bin" vdp:cram\n' "$frame" ;;
    *) printf 'save "b%05d.bin" [0xFF0000] 65536\n' "$frame" ;;
  esac
}

# The frames to dump, ascending and without repeats: the debugger only runs
# forward, so each `frames N` below is a step from the frame before it, and a
# repeated or out-of-order argument would otherwise silently dump another frame.
cmds=$(mktemp)
trap 'rm -f "$cmds"' EXIT
: >"$cmds"
if [[ -n $stop_at ]]; then
  {
    # Stop at an address, not at a frame. Blastem keeps a breakpoint armed through a
    # hit, so N `continue`s is N arrivals, and `continue` does return a prompt when a
    # breakpoint is what stopped it (`cont` with nothing armed is the case that never
    # returns and is what the caveats in README.md are about). PC is printed at every
    # dump so the transcript proves which instruction the machine was stopped on.
    printf '# dumping at the Nth arrival at %s, not at end of frame\n' "$stop_at"
    printf 'breakpoint %s\n' "$stop_at"
    last=0
    for frame in $(ref_frames "${frames[@]}"); do
      for _ in $(seq "$((frame - last))"); do
        printf 'continue\n'
      done
      printf 'printf "== arrival %d pc=%%x ==\\n" PC\n' "$frame"
      for kind_size in "${kinds[@]}"; do
        dump_line "${kind_size%:*}" "$frame"
      done
      last=$frame
    done
  } >>"$cmds"
else
  last=0
  for frame in $(ref_frames "${frames[@]}"); do
    {
      printf 'frames %d\n' "$((frame - last))"
      printf 'printf "== frame %d ==\\n"\n' "$frame"
      printf 'vdpregs\n'
      for kind_size in "${kinds[@]}"; do
        dump_line "${kind_size%:*}" "$frame"
      done
    } >>"$cmds"
    last=$frame
  done
fi
printf 'quit\n' >>"$cmds"

# A private HOME, so the reference always runs the configuration it ships with
# (`ram_init zero`, `default_region U`, `force_region off`) rather than whatever
# the operator left in ~/.gen, and so that this run writes nothing there: blastem
# creates ~/.gen on first run. `ram_init zero` is part of why that matters: work
# RAM randomised at power-on on one side and zeroed on the other makes every byte
# of a first frame meaningless to compare, and #526 lists it as a checked
# confound.
home=$(mktemp -d)
trap 'rm -rf "$home" "$cmds"' EXIT

# The FIFO names are fixed in /tmp, so two runs at once cannot work; stale ones
# from an interrupted run would be opened instead of the fresh pair.
rm -f /tmp/blastem_input /tmp/blastem_output

export PATH="$self_dir/bin:$PATH"
export REF_CMDS="$cmds"
export REF_LOG="$out/blastem.log"
export REF_DRIVE_TIMEOUT="${REF_DRIVE_TIMEOUT:-120}"

# Blastem ignores SIGTERM, hence -k. The dumps are written relative to its
# working directory, hence the cd.
cd "$out"
timeout -k 5 "${REF_TIMEOUT:-300}" "$blastem" -g -d "${extra[@]}" "$rom"

# Every image has to be exactly the size the debugger was asked for. A `save` whose
# expression failed writes nothing and prints no more than a prompt, and a frame
# that quietly arrived short is the failure this harness calls fatal everywhere else.
missing=0
for frame in $(ref_frames "${frames[@]}"); do
  for kind_size in "${kinds[@]}"; do
    kind=${kind_size%:*}
    want=${kind_size#*:}
    file=$out/$(printf 'b%s%05d.bin' "$kind" "$frame")
    if [[ ! -s $file ]]; then
      echo "run-ref: $file was not written; the transcript is in $out/blastem.log" >&2
      missing=1
    elif [[ $(wc -c <"$file") -ne "$want" ]]; then
      echo "run-ref: $file is $(wc -c <"$file") bytes, not $want" >&2
      missing=1
    fi
  done
done
((missing == 0)) || exit 1

echo "reference: $(ref_frames "${frames[@]}" | wc -l) frame(s) x ${#kinds[@]} image(s) in $out"
