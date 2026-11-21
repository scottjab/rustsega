# shellcheck shell=bash
# Sourced by run-ref.sh and run-ours.sh. Not a command in itself.
#
# The one thing worth sharing is where a comparison's dumps go, because the two
# halves are matched by filename (`bNNNNN.bin` from the reference against
# `rNNNNN.bin` from us) and two scripts that each guessed at the directory would
# eventually guess differently.

# ref_slug <rom> - a directory name for a cartridge, from its file name.
ref_slug() {
  local name
  name=$(basename "$1")
  name=${name%.*}
  printf '%s' "${name// /_}"
}

# ref_out_dir <repo> <rom> - where a cart's dumps go by default.
#
# Under the gitignored target/, so running an oracle cannot leave the working
# tree dirty and cannot commit a ROM, a reference dump or a debugger transcript.
# Anything worth keeping gets copied out of here, or written elsewhere with -o.
ref_out_dir() {
  printf '%s/target/ref/%s' "$1" "$(ref_slug "$2")"
}

# ref_out_dir_anchored <repo> <rom> - where an anchored (--stop-at-pc) dump goes.
#
# Apart from the end-of-frame ones of the same cart, because a directory holding
# both is one where `b00600.bin` names two different moments and the comparator
# cannot tell the difference - it is only given a directory and a frame list.
ref_out_dir_anchored() {
  printf '%s/anchor' "$(ref_out_dir "$1" "$2")"
}

# ref_dump_dir <repo> <rom> <stop_at> - the default directory for a run of either
# half: the anchored one when a --stop-at-pc address was given, the end-of-frame
# one when it was not. One spelling so the two halves never disagree about it.
ref_dump_dir() {
  if [[ -n ${3:-} ]]; then
    ref_out_dir_anchored "$1" "$2"
  else
    ref_out_dir "$1" "$2"
  fi
}

# ref_frames <frame ...> - the frames, ascending and de-duplicated, one per line.
#
# Both halves walk the same list, so a comparison always compares the same set of
# frames on both sides even when the arguments come out of order.
ref_frames() {
  printf '%s\n' "$@" | sort -n -u
}

# ref_in_flight <blastem> - what is holding the reference's fixed /tmp FIFOs,
# as pgrep -a lines, empty when the channel is free.
#
# The names are machine-wide, so a second reference run cannot share them, and
# the `rm -f` before a run would delete the channel a live blastem is talking
# through and read as a dead cart instead of a collision (#552). Two things
# mark a run in flight: the blastem executable itself, matched on the whole
# command line so REF_BLASTEM by path works, and the FIFO holder through its
# pidfile (/tmp/blastem_ref_holder.pid), because the holder can outlive the
# blastem it held the FIFO open for. A pidfile pointing at a live PID is only
# believed when that process is the holder's own `sleep 86400`, so a reused
# PID cannot invent a collision; a stale pidfile fails the liveness check and
# is ignored. pgrep never matches itself; a process that merely says blastem
# (an editor here, a grep) is refused too, and the caller prints what it saw
# so the operator can tell. pgrep lines are dropped from the result: a
# concurrent check's own pgrep carries the pattern in its argv, and two
# starts at the same instant must refuse each other for a real blastem, not
# for the sight of one another looking.
ref_in_flight() {
  local lines holder
  lines=$(pgrep -af "$1" 2>/dev/null | grep -vE '^[0-9]+ pgrep( |$)' || true)
  if [[ -r /tmp/blastem_ref_holder.pid ]]; then
    holder=$(cat /tmp/blastem_ref_holder.pid 2>/dev/null || true)
    if [[ $holder =~ ^[0-9]+$ ]] && kill -0 "$holder" 2>/dev/null \
      && tr '\0' ' ' <"/proc/$holder/cmdline" 2>/dev/null | grep -q 'sleep 86400'; then
      lines+="${lines:+$'\n'}${holder} blastem-ref-holder (FIFO holder, pidfile /tmp/blastem_ref_holder.pid)"
    fi
  fi
  printf '%s\n' "$lines"
}
