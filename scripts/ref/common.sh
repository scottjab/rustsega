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

# ref_frames <frame ...> - the frames, ascending and de-duplicated, one per line.
#
# Both halves walk the same list, so a comparison always compares the same set of
# frames on both sides even when the arguments come out of order.
ref_frames() {
  printf '%s\n' "$@" | sort -n -u
}
