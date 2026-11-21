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
# --vdp dumps the VDP's own memories instead: bvNNNNN.bin, the 64 KiB of VRAM,
# bcNNNNN.bin, the 64 colour words of CRAM as 128 raw bytes (#534), and brNNNNN.bin,
# the 24 write registers as 48 bytes of little endian words (#549). The reference
# reaches them through the `vdp:` namespace of its own debugger - `save "v.bin"
# vdp:vram`, `save/w "c.bin" vdp:cram` and `save/w "r.bin" vdp:reg` - so this needs
# no patch to blastem and takes nothing from its source. Note the `/w` on CRAM and
# on the registers: the default element size narrows each colour or register to
# one byte, which compare.py then refuses as the half-width dump it is.
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
# Exit status: 0 every named frame captured at full size; 1 the walk was
# partial or the reference died - the report names the frame the walk stopped
# at, and the captured images stay on disk with manifest.txt, one line per
# image with its size and how much of it is non-zero, so a caller can use the
# frames it got (#552); 2 usage or unreadable ROM; 3 a reference run is already
# in flight on the fixed /tmp FIFOs, refused before this run could rm theirs.
#
# --selftest runs this script against a fake blastem - a clean walk must exit
# 0, a walk the fake stops early must report the frame it stopped at and leave
# a manifest, and a second run while one is in flight must refuse - with no
# ROM, no display and no real blastem; part of `just check`.
#
# Environment:
#   REF_BLASTEM        blastem to run (default: `blastem` on PATH)
#   REF_TIMEOUT        wall seconds for the whole run (default 300)
#   REF_DRIVE_TIMEOUT  passed to terminal-standin.py (default 120)
set -euo pipefail

self_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=/dev/null
. "$self_dir/common.sh"   # ref_dump_dir, ref_frames, ref_in_flight

usage() {
  echo "usage: $(basename "$0") --selftest" >&2
  echo "       $(basename "$0") [-o OUTDIR] [--stop-at-pc PC] [--vdp] ROM FRAME [FRAME ...] [-- BLASTEM-ARG ...]" >&2
  exit 2
}

# --selftest drives the three runs a caller has to tell apart (#552) against a
# fake blastem: a clean walk exits 0 with every image; a walk the fake stops
# early names the frame it stopped at and leaves the captured images with a
# manifest; and a second run while one is in flight refuses (exit 3) without
# deleting the live run's FIFOs. The fake is a python script speaking this
# script's own command file over the real /tmp FIFO names - those fixed names
# are part of what is tested, so the selftest needs the machine's reference
# channel free - and parks forever once FAKE_SERVE_FRAMES `frames` steps are
# served, which is what a reference that stops answering looks like from the
# outside; the run's own REF_TIMEOUT is what ends it.
selftest() {
  local tmp rc out
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT

  if [[ -n $(ref_in_flight "${REF_BLASTEM:-blastem}") ]]; then
    echo "run-ref selftest: FAIL: a reference run is already in flight; the selftest needs the machine's /tmp/blastem_* FIFOs free:" >&2
    ref_in_flight "${REF_BLASTEM:-blastem}" >&2
    exit 1
  fi

  cat >"$tmp/fake-blastem.py" <<'PY'
#!/usr/bin/env python3
"""A fake blastem for run-ref.sh --selftest (#552): the same command file, the
same two FIFOs, none of the emulation. It answers each `frames N` step with a
prompt and materialises each `save` as a file in the working directory, size
taken from the memory letter in the name - the sizes the real `vdp:`
namespaces answer with, 64 KiB for work RAM and VRAM, 128 for CRAM, 48 for the
registers. FAKE_SERVE_FRAMES parks it forever once that many `frames` steps
are served. Fake only: it uses blastem's documented FIFO protocol the way the
driver does, and copies nothing of blastem's code - see the GPL note in
README.md."""

import os
import time

INPUT_FIFO = "/tmp/blastem_input"
OUTPUT_FIFO = "/tmp/blastem_output"
SIZES = {"v": 65536, "c": 128, "r": 48}


def main():
    serve = int(os.environ.get("FAKE_SERVE_FRAMES", 1 << 30))
    os.mkfifo(INPUT_FIFO)
    os.mkfifo(OUTPUT_FIFO)
    out = os.open(OUTPUT_FIFO, os.O_RDWR)  # both ends at once: no blocking, no EPIPE
    os.write(out, b"fake blastem\n> \n")
    log = os.environ.get("REF_LOG")
    with open(os.environ["REF_CMDS"]) as commands:
        lines = [l.strip() for l in commands if l.strip() and not l.strip().startswith("#")]
    steps = 0
    for line in lines:
        if line == "quit":
            os.write(out, b"fake exit\n> \n")
            return
        if line.startswith("frames "):
            if steps >= serve:
                os.write(out, b"fake stops answering\n")
                time.sleep(3600)
            steps += 1
            if log:
                with open(log, "a") as handle:
                    handle.write("== frame step %d ==\n" % steps)
            os.write(out, b"ran a step\n> \n")
        elif line.startswith("save"):
            name = line.split('"')[1]
            size = SIZES.get(name[1], 65536)
            with open(name, "wb") as image:
                image.write(b"\xa5" * size)
            os.write(out, b"> \n")
        else:
            os.write(out, b"> \n")


main()
PY
  chmod +x "$tmp/fake-blastem.py"
  : >"$tmp/rom"

  # The #544 holder leaves a pidfile next to the FIFOs; pin that the check
  # sees a live holder through it, without a whole run around it.
  sleep 86400 &
  holder_pid=$!
  printf '%s\n' "$holder_pid" >/tmp/blastem_ref_holder.pid
  sleep 0.1
  if ! ref_in_flight blastem | grep -q blastem-ref-holder; then
    echo "run-ref selftest: FAIL: ref_in_flight does not see a parked FIFO holder" >&2
    kill "$holder_pid" 2>/dev/null || true
    rm -f /tmp/blastem_ref_holder.pid
    exit 1
  fi
  kill "$holder_pid" 2>/dev/null || true
  wait "$holder_pid" 2>/dev/null || true
  rm -f /tmp/blastem_ref_holder.pid

  # A clean fake walk: every frame served, exit 0, every image at full size -
  # the success behaviour this change must not alter.
  echo "run-ref selftest: clean walk"
  out=$tmp/clean
  rc=0
  REF_BLASTEM="$tmp/fake-blastem.py" REF_TIMEOUT=20 \
    "$self_dir/run-ref.sh" --vdp -o "$out" "$tmp/rom" 1 2 >"$tmp/clean.out" 2>"$tmp/clean.err" || rc=$?
  if ((rc != 0)); then
    echo "run-ref selftest: FAIL: a clean fake walk exited $rc, not 0" >&2
    cat "$tmp/clean.err" >&2
    exit 1
  fi
  if ! grep -q "reference: 2 frame(s) x 3 image(s) in $out" "$tmp/clean.out"; then
    echo "run-ref selftest: FAIL: the clean walk did not print the success line" >&2
    cat "$tmp/clean.out" >&2
    exit 1
  fi
  for image in bv00001:65536 bc00001:128 br00001:48 bv00002:65536 bc00002:128 br00002:48; do
    if [[ $(wc -c <"$out/${image%:*}.bin" 2>/dev/null || echo 0) -ne ${image#*:} ]]; then
      echo "run-ref selftest: FAIL: $out/${image%:*}.bin is not ${image#*:} bytes" >&2
      exit 1
    fi
  done

  # A fake that stops answering after 2 of 3 frames: the run reports the frame
  # it stopped at, counts what landed, and leaves the captured images with a
  # manifest - the partial walk a caller must be able to use (#552). The
  # parked fake is ended by this run's own REF_TIMEOUT, the honest shape of a
  # wedged reference.
  echo "run-ref selftest: partial walk"
  out=$tmp/partial
  rc=0
  REF_BLASTEM="$tmp/fake-blastem.py" FAKE_SERVE_FRAMES=2 REF_TIMEOUT=3 \
    "$self_dir/run-ref.sh" --vdp -o "$out" "$tmp/rom" 5 100 300 >"$tmp/partial.out" 2>"$tmp/partial.err" || rc=$?
  if ((rc != 1)); then
    echo "run-ref selftest: FAIL: a partial walk exited $rc, not 1" >&2
    cat "$tmp/partial.err" >&2
    exit 1
  fi
  if ! grep -q "stopped at frame 300" "$tmp/partial.err"; then
    echo "run-ref selftest: FAIL: the partial report does not name frame 300" >&2
    cat "$tmp/partial.err" >&2
    exit 1
  fi
  if ! grep -q "captured 2 of 3 frames" "$tmp/partial.err"; then
    echo "run-ref selftest: FAIL: the partial report does not count the frames that landed" >&2
    cat "$tmp/partial.err" >&2
    exit 1
  fi
  if [[ ! -f $out/manifest.txt ]] \
    || ! grep -q "bv00100.bin: 65536 bytes," "$out/manifest.txt" \
    || ! grep -q "bv00300.bin: not written" "$out/manifest.txt"; then
    echo "run-ref selftest: FAIL: manifest.txt does not carry the per-image evidence" >&2
    cat "$out/manifest.txt" 2>/dev/null >&2 || true
    exit 1
  fi
  for kept in bv00005 bc00005 br00005 bv00100 bc00100 br00100; do
    if [[ ! -s $out/$kept.bin ]]; then
      echo "run-ref selftest: FAIL: the captured $out/$kept.bin did not stay on disk" >&2
      exit 1
    fi
  done

  # A run while one is in flight: refuse (exit 3) before deleting the live
  # run's FIFOs - the collision check (#552). The live fake parks at its first
  # frame step and its run's own REF_TIMEOUT ends it.
  echo "run-ref selftest: collision refusal"
  # The scenarios before this one left their FIFOs in /tmp; clear them so the
  # poll below can only fire on the live run's own fake creating them.
  rm -f /tmp/blastem_input /tmp/blastem_output
  rc=0
  REF_BLASTEM="$tmp/fake-blastem.py" FAKE_SERVE_FRAMES=0 REF_TIMEOUT=4 \
    "$self_dir/run-ref.sh" --vdp -o "$tmp/live" "$tmp/rom" 1 >"$tmp/live.out" 2>"$tmp/live.err" &
  live=$!
  for _ in $(seq 1 100); do
    [[ -p /tmp/blastem_input ]] && break
    sleep 0.1
  done
  if [[ ! -p /tmp/blastem_input ]]; then
    echo "run-ref selftest: FAIL: the live fake never created the input FIFO" >&2
    cat "$tmp/live.err" >&2
    wait "$live" 2>/dev/null || true
    exit 1
  fi
  rc=0
  REF_BLASTEM="$tmp/fake-blastem.py" \
    "$self_dir/run-ref.sh" --vdp -o "$tmp/refused" "$tmp/rom" 1 >"$tmp/refused.out" 2>"$tmp/refused.err" || rc=$?
  if ((rc != 3)); then
    echo "run-ref selftest: FAIL: a second run while one is in flight exited $rc, not 3" >&2
    cat "$tmp/refused.err" >&2
    kill "$live" 2>/dev/null || true
    wait "$live" 2>/dev/null || true
    exit 1
  fi
  if ! grep -q "in flight" "$tmp/refused.err"; then
    echo "run-ref selftest: FAIL: the refusal does not say a run is in flight" >&2
    cat "$tmp/refused.err" >&2
    exit 1
  fi
  if [[ ! -p /tmp/blastem_input ]]; then
    echo "run-ref selftest: FAIL: the refused run deleted the live run's FIFO" >&2
    exit 1
  fi
  rc=0
  wait "$live" 2>/dev/null || rc=$?
  if ! grep -q "stopped at frame 1" "$tmp/live.err"; then
    echo "run-ref selftest: FAIL: the timed-out live run did not report its stopped-at frame" >&2
    cat "$tmp/live.err" >&2
    exit 1
  fi

  # Leave the machine as bare as it was found: the FIFOs the fakes made are
  # this script's fixed names, and no run of ours is holding them any more.
  if [[ -z $(ref_in_flight "${REF_BLASTEM:-blastem}") ]]; then
    rm -f /tmp/blastem_input /tmp/blastem_output
  fi

  echo "run-ref selftest: ok - clean walk exits 0, partial walk names its frame with a manifest, a second run refuses"
  exit 0
}

if [[ ${1:-} == --selftest ]]; then
  selftest   # runs the fake-blastem scenarios and exits the script
fi

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
  kinds=(v:65536 c:128 r:48)
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
    r) printf 'save/w "br%05d.bin" vdp:reg\n' "$frame" ;;
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
#
# The same isolation also covers save data: blastem 1.0.0 writes save SRAM and
# EEPROM under ~/.local/share/blastem/<rom>/ and loads them again on the next
# boot of the same cart, so without it an SRAM or EEPROM cart ran on whatever
# the last session left behind (#544: the throwaway HOME was built but never
# applied, and psii/psiv/nbajam all booted on real saves). XDG_DATA_HOME would
# override the derived path on a machine that sets it, so it goes into the
# throwaway home too.
home=$(mktemp -d)
trap 'rm -rf "$home" "$cmds"' EXIT
export HOME="$home"
export XDG_DATA_HOME="$home/.local/share"
export XDG_CONFIG_HOME="$home/.config"

# Bind no real gamepad. The comparison is a no-input run by design (ours gets
# none), but blastem binds the session's controllers through SDL and, while its
# window has focus, delivers whatever they emit - one stray axis report from an
# idle pad steers the cart's attract loop, and the reference's memories then
# diverge from the same cart's clean run. #648 chased a 908 bytes/frame verdict
# on Road Rash II that lived entirely in the reference half: our side
# byte-identical across every run, the reference different only in the runs the
# desktop was in use for, with the pad's CRAM state shifted with it. SDL's
# ignore-except hint naming a device that cannot exist turns every real pad
# off; the emulated ports still have their gamepads, they just never hear
# anyone. The transcript proves it: no "Joystick N added" line.
export SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT="0x0000/0x0000"

# Refuse to start over a live reference run (#552). The FIFOs are fixed
# machine-wide names, and the rm below would delete the channel a running
# blastem is talking through: its next write would land on a FIFO this run
# made, and the collision would read as the cart under test being broken -
# which is how #357's batch produced false failures. The check is one pgrep on
# the blastem executable plus the FIFO holder's pidfile; on a single-operator
# machine the check-to-rm race is not worth more machinery.
command -v pgrep >/dev/null || {
  echo "run-ref: no pgrep on PATH; cannot check for a live reference run" >&2
  exit 3
}
in_flight=$(ref_in_flight "$blastem")
if [[ -n $in_flight ]]; then
  echo "run-ref: a reference run is already in flight; refusing to delete and reuse its /tmp/blastem_* FIFOs:" >&2
  printf '%s\n' "$in_flight" >&2
  echo "run-ref: stop it (or set REF_BLASTEM past it) and run again" >&2
  exit 3
fi

# The FIFO names are fixed in /tmp, so two runs at once cannot work; stale ones
# from an interrupted run would be opened instead of the fresh pair.
rm -f /tmp/blastem_input /tmp/blastem_output

# Hold a reader on the output FIFO past the driver's death. Blastem answers
# `quit` by SIGKILLing the terminal process it exec'd (the driver) and only
# then writes its exit-time output to the same FIFO - "Saved SRAM to ...",
# the last prompt. With every reader dead that write is a broken pipe, and
# blastem, which does not ignore SIGPIPE, dies with signal 13 after doing
# all of its work (#544): run-ref.sh then aborts before the size checks and
# the sweep records a death that never touched a frame. This holder waits
# for blastem to create the FIFO, opens it read-write - both ends at once,
# so nothing can EPIPE on it and the driver's drain sees no EOF before its
# grace - and stays attached until the run below is over. Blastem can kill
# its own terminal child; it cannot kill us.
holder_wait() {
  for _ in $(seq 1 200); do
    [[ -p /tmp/blastem_output ]] && return 0
    sleep 0.05
  done
  return 1
}
(
  if holder_wait; then
    exec 9<>/tmp/blastem_output
    # The pidfile is what a later run's collision check reads: this holder can
    # outlive the blastem it is holding the FIFO for, and the next run must
    # see it rather than rm through its channel (#552).
    printf '%s\n' "$$" >/tmp/blastem_ref_holder.pid
    exec sleep 86400
  fi
) &
holder=$!

export PATH="$self_dir/bin:$PATH"
export REF_CMDS="$cmds"
export REF_LOG="$out/blastem.log"
export REF_DRIVE_TIMEOUT="${REF_DRIVE_TIMEOUT:-120}"

# Blastem ignores SIGTERM, hence -k. The dumps are written relative to its
# working directory, hence the cd.
cd "$out"
status=0
timeout -k 5 "${REF_TIMEOUT:-300}" "$blastem" -g -d "${extra[@]}" "$rom" || status=$?
kill "$holder" 2>/dev/null || true
wait "$holder" 2>/dev/null || true
rm -f /tmp/blastem_ref_holder.pid

# The per-frame walk runs however the reference ended (#552): the whole point
# is that a walk that stops early is reportable as partial rather than reading
# as an unbootable cart. Every image still has to be exactly the size the
# debugger was asked for - a `save` whose expression failed writes nothing and
# prints no more than a prompt.
frame_list=$(ref_frames "${frames[@]}")
total=0
ok=0
bad=0
stopped_at=""
first_bad=""
for frame in $frame_list; do
  total=$((total + 1))
  frame_bad=""
  for kind_size in "${kinds[@]}"; do
    kind=${kind_size%:*}
    want=${kind_size#*:}
    name=$(printf 'b%s%05d.bin' "$kind" "$frame")
    if [[ ! -s $out/$name ]]; then
      echo "run-ref: frame $frame: $name was not written" >&2
      frame_bad=$name
    elif [[ $(wc -c <"$out/$name") -ne "$want" ]]; then
      echo "run-ref: frame $frame: $name is $(wc -c <"$out/$name") bytes, not $want" >&2
      frame_bad=$name
    fi
  done
  if [[ -z $frame_bad ]]; then
    ok=$((ok + 1))
  else
    if ((bad == 0)); then
      stopped_at=$frame
      first_bad=$frame_bad
    fi
    bad=$((bad + 1))
  fi
done

if ((bad == 0)) && ((status == 0)); then
  # A clean run leaves no stale report behind: a manifest from an earlier
  # partial walk of this directory would otherwise outlive its own run.
  rm -f "$out/manifest.txt"
  echo "reference: $total frame(s) x ${#kinds[@]} image(s) in $out"
  exit 0
fi

# A partial or unclean run: the frames it captured stay usable. manifest.txt
# carries one line per image - its size and how much of it is non-zero, which
# is the evidence a caller reads "blastem booted it" out of (#552's
# `bv00100.bin: 26351 bytes non-zero`) - and a later clean run removes it.
{
  if ((bad > 0)); then
    echo "# stopped at frame $stopped_at; $ok of $total frames captured"
  else
    echo "# all $total frames captured; blastem exited $status"
  fi
  echo "# transcript: $out/blastem.log"
  for frame in $frame_list; do
    for kind_size in "${kinds[@]}"; do
      name=$(printf 'b%s%05d.bin' "${kind_size%:*}" "$frame")
      if [[ -s $out/$name ]]; then
        echo "$name: $(wc -c <"$out/$name") bytes, $(tr -d '\0' <"$out/$name" | wc -c) non-zero"
      else
        echo "$name: not written"
      fi
    done
  done
} >"$out/manifest.txt"

if ((bad > 0)); then
  echo "run-ref: stopped at frame $stopped_at of ${frame_list//$'\n'/ }: $first_bad" >&2
  echo "run-ref: captured $ok of $total frames; they stay on disk in $out, per-image evidence in $out/manifest.txt" >&2
  echo "run-ref: a partial reference run says nothing about the cart; ask again, frame by frame, before calling it unbootable (README caveats)" >&2
else
  echo "run-ref: all $total frames captured, but blastem exited $status; images are on disk in $out, per-image evidence in $out/manifest.txt" >&2
fi
exit 1
