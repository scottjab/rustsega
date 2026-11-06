#!/usr/bin/env bash
#
# Fetch the public CPU conformance suites and hardware test ROMs into test-roms/.
#
# Every entry in MANIFEST is pinned twice: the URL names an exact commit SHA,
# and the tarball SHA-256 is checked before anything is unpacked. This is what
# guarantees CI and local runs see byte-identical test data.
#
# The script is idempotent. A suite is skipped, with no network access at all,
# when test-roms/.stamp-<name> already holds the pinned SHA-256 and the
# destination directory is present. Downloaded tarballs are kept in
# test-roms/.cache/ so a rerun reuses them instead of refetching.
#
# Nothing here is committed: /test-roms/ is gitignored.
# See docs/TESTING.md for the pinned commits and how to bump them, and
# docs/test-roms.md for the hardware test ROMs below and for the ones that have
# to be placed by hand.

set -euo pipefail

# name | pinned URL | archive sha256 | subdir to keep | destination under test-roms/ | kind
#
# `kind` is `tar` (the default when the field is left off) or `zip`. A `tar`
# row keeps only `subdir` from inside the archive; a `zip` row is unpacked
# whole, so its `subdir` field is `-`.
MANIFEST=(
  # SingleStepTests/680x0 @ e0d5ece9670205cc84a0101081837deb446f86a3 (main)
  "680x0|https://github.com/SingleStepTests/680x0/archive/e0d5ece9670205cc84a0101081837deb446f86a3.tar.gz|f90dab5ba7519c6d9e20143be12e433403b6db6586f6bae3e5d3bf0123f01b50|68000/v1|680x0"
  # SingleStepTests/z80 @ ebe1875d48f374bcfd4b505d8eb8ee751568b5f7 (main)
  "z80|https://github.com/SingleStepTests/z80/archive/ebe1875d48f374bcfd4b505d8eb8ee751568b5f7.tar.gz|f44683f0edf1c1c37f98861ccd1bfda3b9bce65b5b258d4817a0f1d2012076a7|v1|z80"
  # Eke's Gens hardware test pack: 15 Mega Drive test ROMs (Nemesis's VDP write
  # tests, TmEE's shadow/highlight tests, the 1536-colour demo, memtest_68k).
  # The original host, genplus-gx on Google Code, is gone; this Wayback capture
  # of the download is the copy that survived.
  "md-test-pack|https://web.archive.org/web/20150610093059if_/http://genplus-gx.googlecode.com/files/md_test.zip|1b37dd77e81f5bfd8e3dc09adc69609f07a30135ea611782a850e8c7336a33ba|-|genesis/md-test-pack|zip"
  # TiTAN's Overdrive 2 (2017): the hardest known Mega Drive demo, so the
  # strictest VDP-timing, FIFO-stall and mapper stress case we can run. It is a
  # stress case, not a pass/fail suite. Mirror of the pouet.cn release.
  "overdrive2|https://archive.org/download/pouet_69648/titan-overdrive2.zip|877d3244102531052d2ffc39e0e7390c7b1ca65a7323b49cf28d49ffb3ef6110|-|genesis/overdrive2|zip"
  # ZEXDOC and ZEXALL, Frank D. Cringle's CP/M Z80 exercisers, the suite ticket
  # M2-16 asks for. zexdoc.com is gone; mdfs.net has hosted these since 2003.
  "zexall-cpm|https://mdfs.net/Software/Z80/Exerciser/CPM.zip|e0f4e6af6c88764a615e0d9fa27b9bf9a2162bd77ac7492fd1f2d4fa9cdfb3bb|-|z80/zexall-cpm|zip"
)

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_ROM_DIR="${RUSTSEGA_TEST_ROM_DIR:-$REPO_ROOT/test-roms}"
CACHE_DIR="$TEST_ROM_DIR/.cache"

log() { printf '[fetch-test-data] %s: %s\n' "$1" "${2:-}"; }

die() {
  printf 'fetch-test-data.sh: error: %s\n' "$1" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: scripts/fetch-test-data.sh [suite ...]

Fetch the pinned public CPU conformance suites and hardware test ROMs
into test-roms/.

With no arguments every suite in the manifest is fetched; otherwise only the
named suites are. Idempotent: a suite whose stamp file already matches the
pinned SHA-256 is reported "up to date" and nothing is downloaded.

Suites: 680x0, z80, md-test-pack, overdrive2, zexall-cpm
See docs/test-roms.md for what each one is and what it is good for.

Environment:
  RUSTSEGA_TEST_ROM_DIR   Destination root (default: <repo>/test-roms)
EOF
}

# Echo the `|`-separated manifest record for a suite name.
manifest_names() {
  local record names=""
  for record in "${MANIFEST[@]}"; do
    names+="${names:+ }${record%%|*}"
  done
  printf '%s' "$names"
}

manifest_entry() {
  local want="$1" record
  for record in "${MANIFEST[@]}"; do
    if [[ "${record%%|*}" == "$want" ]]; then
      printf '%s\n' "$record"
      return 0
    fi
  done
  return 1
}

sha_matches() {
  local file="$1" expected="$2"
  printf '%s  %s\n' "$expected" "$file" | sha256sum -c --quiet - >/dev/null 2>&1
}

# Fetch one suite: reuse the cached tarball when it has the pinned hash,
# otherwise (re)download and verify, then unpack only the needed subdir.
fetch_suite() {
  local name="$1"
  local record url sha256 subdir dest kind stamp archive
  record="$(manifest_entry "$name")"
  IFS='|' read -r _ url sha256 subdir dest kind <<<"$record"
  kind="${kind:-tar}"

  # `dest` is rm -rf'd before extraction; refuse anything suspicious.
  if [[ -z "$subdir" || -z "$dest" || "$dest" == */ || "$dest" == *..* ]]; then
    die "manifest entry for '$name' has an unsafe destination '$dest'"
  fi
  dest="$TEST_ROM_DIR/$dest"
  stamp="$TEST_ROM_DIR/.stamp-$name"

  local archive
  case "$kind" in
    tar)
      [[ "$subdir" != '-' ]] || die "$name: a tar row needs a subdir"
      archive="$CACHE_DIR/$name.tar.gz"
      ;;
    zip) archive="$CACHE_DIR/$name.zip" ;;
    *) die "$name: unknown archive kind '$kind'" ;;
  esac

  if [[ -f "$stamp" && "$(<"$stamp")" == "$sha256" && -n "$(ls -A "$dest" 2>/dev/null)" ]]; then
    log "$name" "up to date (pinned $sha256)"
    return
  fi

  mkdir -p "$CACHE_DIR"
  local attempt
  for attempt in 1 2; do
    if [[ -f "$archive" ]]; then
      if sha_matches "$archive" "$sha256"; then
        log "$name" "using cached archive"
        break
      fi
      log "$name" "checksum mismatch on $archive, discarding it"
      rm -f "$archive"
    fi
    log "$name" "downloading $url"
    curl -fsSL --retry 3 --retry-delay 1 -o "$archive" "$url"
    if sha_matches "$archive" "$sha256"; then
      break
    fi
    log "$name" "checksum mismatch on the freshly downloaded archive"
    rm -f "$archive"
    [[ $attempt -eq 1 ]] ||
      die "$name: cannot fetch $url with SHA-256 $sha256 (pinned value is wrong, or the download is corrupt)"
  done

  rm -rf "$dest"
  mkdir -p "$dest"
  case "$kind" in
    # GitHub archive tarballs are rooted at <repo>-<sha>/; the glob keeps only
    # the suite's subdir and --strip-components drops that root, so the files
    # land at e.g. test-roms/680x0/68000/v1/ and test-roms/z80/v1/.
    tar)
      tar --extract --gzip --file="$archive" --directory="$dest" \
        --wildcards --strip-components=1 -- "*/$subdir/*"
      ;;
    zip) unzip -q -o "$archive" -d "$dest" ;;
  esac

  local count
  count="$(find "$dest" -type f | wc -l)"
  [[ "$count" -gt 0 ]] || die "$name: nothing was unpacked from $archive"

  printf '%s\n' "$sha256" >"$stamp"
  log "$name" "extracted $count files to ${dest#"$TEST_ROM_DIR"/}"
}

main() {
  case "${1:-}" in
    -h | --help)
      usage
      exit 0
      ;;
  esac

  local -a requested=()
  if [[ $# -gt 0 ]]; then
    local name
    for name in "$@"; do
      manifest_entry "$name" >/dev/null || die "unknown suite '$name' (known: $(manifest_names))"
      requested+=("$name")
    done
  else
    local record
    for record in "${MANIFEST[@]}"; do
      requested+=("${record%%|*}")
    done
  fi

  local suite
  for suite in "${requested[@]}"; do
    fetch_suite "$suite"
  done
}

main "$@"
