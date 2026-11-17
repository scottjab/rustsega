#!/usr/bin/env python3
"""Compare the reference's work RAM against ours, and say whether they agree.

Part of the external reference oracle of #525. Reads a directory of raw work-RAM
images written by ``run-ref.sh`` (``bNNNNN.bin``) and ``run-ours.sh``
(``rNNNNN.bin``) - one 64 KiB image of the 68000 address space
``$FF0000-$FFFFFF`` per frame - and reports whether the two emulators agree.

usage::

    compare.py OUTDIR [595-605 600 ...] [--sweep 4] [--anchor 0x584]
    compare.py --selftest

Why this is not a ``cmp`` of two files, which is what it looks like it should be:

**A byte count on one frame means nothing.** Work RAM churns: rustsega compared
against *itself* one frame apart differs by 11-74 bytes on the first cart this was
run on, so "26 bytes differ at frame 600" is inside the noise floor of a single
frame and says nothing about whether the two machines disagree. What carries the
claim is the **persistence test** - an address that differs in *all* m frames
sampled is not frame phase, and an address that differs in one of them is. So the
report's headline is "differs in k of m frames", never a byte count alone.

**Frame counts are not frames.** Two cores that are not cycle-identical can be a
frame apart at the point of comparison, which turns one frame's worth of churn
into a large and completely uninformative diff. So the **alignment sweep** compares
reference frame N against our frame N+shift for shift in a window and prints the
mean diff at each: a clean V with its minimum at shift 0 is evidence the two are
counting frames the same way, and a minimum somewhere else means the numbers below
it are comparing the wrong things. Measure it, do not assume it (#525's own
constraint, and #526 is the finding that came out of doing it).

Neither test says which side is wrong. Disagreement is evidence, not proof: a
second reference is wanted before behaviour changes to match one.

``--selftest`` is the harness's own check and part of ``just check``: it compares
a canned pair of images that agree, then a mutated copy, then a mutated-in-every-
frame copy, and asserts the report distinguishes all three. It is the only thing
standing between a refactor of this file and an oracle that prints "agree" for
everything.
"""

import argparse
import contextlib
import io
import os
import re
import sys
import tempfile
from dataclasses import dataclass, field

IMAGE_NAME = re.compile(r"^([br])(\d{5})\.bin$")
"""What a dump file is called: `b00600.bin` from the reference, `r00600.bin` from us."""

RAM_BASE = 0xFF0000
"""Start of the compared region: the bottom of the 68000's work RAM mirror."""

RAM_SIZE = 1 << 16
"""Bytes per image. Both dumpers write exactly this much, and they are compared
byte for byte with no header - which is what makes `cmp`-shaped reasoning safe and
`run-ref.sh` check it on the way out."""

REF_PREFIX = "b"
OURS_PREFIX = "r"


class HarnessError(Exception):
    """The harness itself is not working: a short or unreadable image.

    Distinct from "the emulators disagree", which is a result and not an error.
    An oracle that quietly skipped an unreadable frame and reported agreement on
    the rest would be worse than no oracle, so this is fatal.
    """


class MissingDump(HarnessError):
    """One side's image for one frame is not on disk.

    Its own type because of one caller: walking a frame list with a non-zero shift
    runs off the end of what was dumped, and that is the end of the data rather
    than a fault. A truncated file is never that - see :func:`load_one`.
    """


def load_one(directory, side, frame):
    """One dump: `side` is ``REF_PREFIX`` or ``OURS_PREFIX``.

    A missing or short image is an error rather than a silently skipped frame: an
    oracle that dropped a frame and reported agreement on what was left is worse
    than no oracle.
    """
    name = f"{side}{frame:05d}.bin"
    path = os.path.join(directory, name)
    if not os.path.isfile(path):
        raise MissingDump(f"no {name} for frame {frame} in {directory}")
    size = os.path.getsize(path)
    if size != RAM_SIZE:
        raise HarnessError(
            f"{name} is {size} bytes, expected {RAM_SIZE}; "
            "a truncated dump cannot be compared byte for byte"
        )
    with open(path, "rb") as handle:
        return handle.read()


def load_at(directory, frame, shift=0):
    """The (reference, ours) pair for one frame, ours read at ``frame + shift``.

    ``shift`` is the measured alignment of the two frame counters, not a fudge: see
    :func:`sweep`.
    """
    return (
        load_one(directory, REF_PREFIX, frame),
        load_one(directory, OURS_PREFIX, frame + shift),
    )


def diff_offsets(reference, ours):
    """Offsets of the bytes the two images disagree on."""
    if len(reference) != len(ours):
        raise HarnessError("cannot compare images of different lengths")
    return [i for i, (a, b) in enumerate(zip(reference, ours)) if a != b]


def address(offset):
    """The 68000 address a byte offset in an image belongs to."""
    return RAM_BASE + offset


def ranges(offsets, limit=8):
    """A human-readable summary of a set of offsets: `$FF0000-$FF0003, $FF1234`.

    Collapsed into runs because these are always either nothing or a run of a
    dozen, and a list of 26 addresses is harder to read than two ranges.
    """
    if not offsets:
        return "-"
    ordered = sorted(set(offsets))
    runs = []
    start = previous = ordered[0]
    for item in ordered[1:]:
        if item == previous + 1:
            previous = item
            continue
        runs.append((start, previous))
        start = previous = item
    runs.append((start, previous))
    if len(runs) > limit:
        shown = runs[:limit]
        more = f" (+{len(runs) - limit} more)"
    else:
        shown, more = runs, ""
    text = ", ".join(
        f"0x{address(start):06X}" if start == end else f"0x{address(start):06X}-0x{address(end):06X}"
        for start, end in shown
    )
    return text + more


def frames_in(directory):
    """``(reference frames, our frames)`` that have an image on disk, ascending.

    Kept apart rather than intersected because they are not compared at the same
    number: the whole point of :func:`sweep` is that reference frame N is matched
    against our frame N+shift, so intersecting here would throw away exactly the
    frames the sweep needs to find its offset with.
    """
    if not os.path.isdir(directory):
        raise HarnessError(f"{directory} is not a directory; run run-ref.sh and run-ours.sh first")
    sides = {REF_PREFIX: set(), OURS_PREFIX: set()}
    for name in os.listdir(directory):
        matched = IMAGE_NAME.match(name)
        if not matched:
            continue
        sides[matched.group(1)].add(int(matched.group(2)))
    return sorted(sides[REF_PREFIX]), sorted(sides[OURS_PREFIX])


def frames_requested(spec, directory, shift=0):
    """Resolve ``--frames 595-605,600`` to reference frames with a pair at ``shift``.

    The list is a request: a frame with no image on either side is not compared and
    says so in the report, because a frame list walked at a non-zero shift runs off
    the end of what was dumped by construction. Only a request that matches nothing
    at all is an error.
    """
    available, ours = frames_in(directory)
    if not available:
        raise HarnessError(f"no bNNNNN.bin/rNNNNN.bin pair to compare in {directory}")
    wanted = set()
    if spec:
        for part in spec.split(","):
            part = part.strip()
            if not part:
                continue
            if "-" in part:
                low, high = part.split("-", 1)
                wanted.update(range(int(low), int(high) + 1))
            else:
                wanted.add(int(part))
    else:
        wanted.update(available)
    ours = set(ours)
    chosen = [frame for frame in available if frame in wanted and frame + shift in ours]
    if not chosen:
        raise HarnessError(
            f"none of the frames {sorted(wanted)[:8]} have a pair in {directory}"
            f"{f' at shift {shift:+d}' if shift else ''}; "
            f"it holds {available[:5]}{'...' if len(available) > 5 else ''}"
        )
    return chosen


@dataclass
class Report:
    """What a comparison found. Printed by :func:`print_report`, asserted on by the self-test."""

    directory: str
    #: reference frame numbers the caller asked for
    frames: list
    #: reference frame -> offsets that differ, ours read at frame + shift
    per_frame: dict = field(default_factory=dict)
    #: (shift, mean differing bytes, pairs) for each shift the sweep tried
    sweep: list = field(default_factory=list)
    #: the shift the comparison was made at, from :func:`sweep`
    shift: int = 0
    #: whether that shift was measured rather than assumed (`--sweep 0`)
    shift_measured: bool = False
    #: offset -> number of frames it differs in
    persistent: dict = field(default_factory=dict)
    #: offsets that differ in some but not all of the frames
    phase: list = field(default_factory=list)
    #: reference frames with no image on our side at this shift (the list ran off the end)
    dropped: list = field(default_factory=list)
    #: the address both dumpers stopped at (`--anchor`, `--stop-at-pc`), or "" when
    #: the dumps are end-of-frame ones. It changes no arithmetic: it says what the
    #: frame numbers mean, which is the difference between a dump of a moment and a
    #: dump of whatever each machine's frame loop happened to end on (#526).
    anchor: str = ""

    @property
    def agrees(self):
        """Do the two sides agree, frame for frame at the comparison's shift?"""
        return not any(self.per_frame.values())

    @property
    def differs_in_all(self):
        """Offsets differing in every frame compared - the ones worth chasing."""
        return sorted(self.persistent)

    @property
    def compared(self):
        """The reference frames actually compared, ascending."""
        return sorted(self.per_frame)


def compare(directory, frames, shift=0):
    """Compare each reference frame against our frame ``frame + shift``.

    The persistence test runs over the frames compared here, which is why the
    shift has to be right before this is called: compare a reference frame against
    an off-by-one image and every byte that moves in a frame looks like a
    persistent divergence. On #526's cart that turned 25 real addresses into 35.

    A frame whose shifted image was never dumped is dropped and reported, because
    a frame list walked with a shift runs off the end of the dumps by construction.
    Anything else - a truncated file, a missing reference frame - is fatal.
    """
    report = Report(directory=directory, frames=list(frames), shift=shift)
    for frame in frames:
        reference = load_one(directory, REF_PREFIX, frame)
        try:
            ours = load_one(directory, OURS_PREFIX, frame + shift)
        except MissingDump:
            if shift == 0:
                raise
            report.dropped.append(frame)
            continue
        report.per_frame[frame] = diff_offsets(reference, ours)
    if not report.per_frame:
        raise HarnessError(f"no frame of {len(frames)} could be compared in {directory}")
    counts = {}
    for offsets in report.per_frame.values():
        for offset in offsets:
            counts[offset] = counts.get(offset, 0) + 1
    every = len(report.per_frame)
    report.persistent = {o: c for o, c in counts.items() if c == every}
    report.phase = sorted(o for o, c in counts.items() if c < every)
    return report


def sweep(directory, frames, span):
    """Mean differing bytes at each frame offset, over the frames given.

    ``shift`` is added to the frame number of *our* side only: shift +1 compares
    reference frame N against our frame N+1, so a minimum at +1 says our images are
    a frame behind the reference's. Frames with no image at the shifted number drop
    out of that shift's mean, which is why the count is printed with it.
    """
    results = []
    best = None
    for shift in range(-span, span + 1):
        total = 0
        pairs = 0
        for frame in frames:
            try:
                reference, ours = load_at(directory, frame, shift)
            except MissingDump:
                continue
            total += len(diff_offsets(reference, ours))
            pairs += 1
        if not pairs:
            continue
        mean = total / pairs
        results.append((shift, mean, pairs))
        if best is None or mean < best[0]:
            best = (mean, shift)
    if best is None:
        raise HarnessError(f"no frame pair to sweep in {directory}")
    return results, best[1]


def run_comparison(directory, frames, span=4):
    """Measure the frame alignment, then compare at it: the whole pipeline.

    The order matters and is the whole reason the two are one function. Comparing
    at shift 0 first and only reporting the sweep afterwards - which is what this
    file did before the first real run - produces a confident, wrong answer on any
    cart whose two frame counters are not identical: #526's cart needs shift +1,
    and at shift 0 this reports 35 persistent addresses where the truth is 25.
    """
    if span <= 0:
        return compare(directory, frames, 0)
    results, shift = sweep(directory, frames, span)
    report = compare(directory, frames, shift)
    report.sweep = results
    report.shift_measured = True
    return report


def print_report(report, stream=None):
    """The human-facing report. The exit code is what scripts read.

    ``stream`` is resolved on every call, not as a default argument: a default of
    ``sys.stdout`` is bound once at import time and then survives
    ``contextlib.redirect_stdout``, which is how the self-test would otherwise
    scribble four reports over the operator's terminal.
    """
    stream = sys.stdout if stream is None else stream

    def say(text):
        print(text, file=stream)

    say(f"dir      {report.directory}")
    say(f"region   0x{RAM_BASE:06X}-0x{RAM_BASE + RAM_SIZE - 1:06X}, {RAM_SIZE} B per frame")
    if report.anchor:
        say(f"point    both sides stopped before 0x{report.anchor} (frame N = arrival N there)")
    compared = report.compared
    if len(compared) > 1:
        say(f"frames   {len(compared)}: {compared[0]}..{compared[-1]} (reference numbering)")
    elif compared:
        say(f"frames   {compared[0]} (reference numbering)")
    if report.shift:
        basis = "measured" if report.shift_measured else "asked for"
        say(f"shift    {report.shift:+d} ({basis}: reference frame N read against our frame N{report.shift:+d})")
    else:
        basis = "measured" if report.shift_measured else "assumed, --sweep 0"
        say(f"shift    +0 ({basis})")
    if report.dropped:
        say(f"         {len(report.dropped)} frame(s) had no image on our side at this shift")
    say("")
    if report.per_frame:
        say("frame        differ  addresses")
    for frame in compared:
        offsets = report.per_frame[frame]
        say(f"{frame:7d} {len(offsets):9d}  {ranges(offsets)}")
    if report.sweep:
        say("")
        say("alignment (mean differing bytes of %d, our frame number shifted):" % RAM_SIZE)
        for shift, mean, pairs in report.sweep:
            mark = " <- best" if shift == report.shift else ""
            say(f"  shift {shift:+d} {mean:9.1f}  over {pairs} frame pair(s){mark}")
        if report.anchor and report.shift:
            # Unanchored, a measured shift is a property of two frame counters. At
            # an anchor there is only one counter, the one the cart's own interrupt
            # gives both machines, so a shift here is the two machines having taken
            # that interrupt a different number of times - worth saying out loud,
            # because it is the finding the shift used to hide.
            say(
                f"         anchored at 0x{report.anchor} but {report.shift:+d} arrivals apart: "
                "the two machines have not"
            )
            say("         taken that interrupt the same number of times by this point")
    say("")
    if compared:
        every = len(compared)
        if report.differs_in_all:
            say(f"differs in all {every} of {every} frames - persistent divergence:")
            reference, ours = load_at(report.directory, compared[-1], report.shift)
            for offset in report.differs_in_all:
                count = report.persistent[offset]
                say(
                    f"  0x{address(offset):06X}  ref={reference[offset]:02X}  "
                    f"ours={ours[offset]:02X}  ({count} of {every} frames)"
                )
        else:
            say(f"nothing differs in all {every} frame(s)")
        if report.phase:
            say(f"differ in fewer frames - frame phase, {len(report.phase)} addresses: "
                f"{ranges(report.phase)}")
    say("")
    if report.agrees:
        say("RESULT: agree")
    else:
        total = sum(len(v) for v in report.per_frame.values()) / len(report.per_frame)
        say(
            f"RESULT: disagree at shift {report.shift:+d}, "
            f"{total:.1f} of {RAM_SIZE} bytes per frame on average, "
            f"{len(report.differs_in_all)} addresses in every frame compared"
        )


def build(directory, frames, images):
    """Write ``images[frame]`` as both the reference and our dump for that frame.

    Test scaffolding: the self-test needs canned dumps of the right size, and this
    keeps it from needing a ROM or an emulator to make them.
    """
    for frame in frames:
        data = images[frame]
        for prefix in (REF_PREFIX, OURS_PREFIX):
            with open(os.path.join(directory, f"{prefix}{frame:05d}.bin"), "wb") as handle:
                handle.write(data)


def mutate(directory, frame, side, offset, value):
    """Flip one byte of one dump on one side."""
    path = os.path.join(directory, f"{side}{frame:05d}.bin")
    with open(path, "r+b") as handle:
        handle.seek(offset)
        handle.write(bytes([value]) if isinstance(value, int) else value)


def churned_images(count, seed=0x5250):
    """``count`` 64 KiB images that differ from each other in a handful of bytes.

    Work RAM is never identical between frames, so a self-test built on identical
    images would pass a comparator that only ever looked at frame 0; and the churn
    has to be *small*, or a test about a one-frame misalignment cannot tell a
    aligned comparison from a misaligned one. The generator is a plain LCG, so a
    failing self-test reproduces byte for byte.
    """
    state = seed & 0xFFFFFFFF

    def noise():
        nonlocal state
        state = (state * 1103515245 + 12345) & 0xFFFFFFFF
        return (state >> 16) & 0xFF

    base = bytearray(noise() for _ in range(RAM_SIZE))
    images = {}
    for frame in range(count):
        body = bytearray(base)
        # A move of a few bytes, at a different place and by a different amount in
        # every frame: the shape of what a running game does to its scratch RAM.
        for offset in range(0x1000 + frame * 4, 0x1000 + frame * 4 + 8):
            body[offset] ^= (frame * 7 + 1) & 0xFF
        images[frame] = bytes(body)
    return images


def selftest():
    """Check the harness on canned dumps. Returns 0 if the harness works, 1 if not."""
    failures = []

    def check(name, condition, detail=""):
        print(f"  {'ok  ' if condition else 'FAIL'}  {name}{'' if condition else ': ' + detail}")
        if not condition:
            failures.append(name)

    with tempfile.TemporaryDirectory() as work:
        frames = [10, 11, 12, 13]
        images = churned_images(20)
        build(work, frames, images)

        print("selftest: a pair that agrees")
        report = compare(work, frames)
        with contextlib.redirect_stdout(io.StringIO()):
            print_report(report)
        check("identical dumps report agree", report.agrees, f"{report.per_frame}")
        check("identical dumps name no divergence", not report.differs_in_all)

        print("selftest: a pair that differs in one frame")
        flip = 0xFFFDF0 - RAM_BASE
        mutate(work, 12, OURS_PREFIX, flip, images[12][flip] ^ 0xFF)
        report = compare(work, frames)
        with contextlib.redirect_stdout(io.StringIO()):
            print_report(report)
        check("one mutated byte is not reported as agreement", not report.agrees)
        check(
            "the mutation is the only difference at its frame",
            report.per_frame[12] == [flip],
            f"{[address(o) for o in report.per_frame[12]]}",
        )
        check(
            "other frames are still clean",
            all(not report.per_frame[f] for f in frames if f != 12),
        )
        check(
            "a one-frame difference is frame phase, not persistence",
            report.differs_in_all == [],
            f"{[address(o) for o in report.differs_in_all]}",
        )
        check("and it is reported as phase", report.phase == [flip])

        print("selftest: a pair that differs in every frame")
        build(work, frames, images)
        for frame in frames:
            mutate(work, frame, OURS_PREFIX, flip, images[frame][flip] ^ 0xA5)
        report = compare(work, frames)
        with contextlib.redirect_stdout(io.StringIO()):
            print_report(report)
        check(
            "an address differing in all frames is persistent",
            report.differs_in_all == [flip],
            f"{[address(o) for o in report.differs_in_all]}",
        )
        check("and it is not counted as phase", report.phase == [])

        print("selftest: the alignment sweep")
        align = os.path.join(work, "align")
        os.mkdir(align)
        base = churned_images(20, seed=0x1234)
        # Ours one frame behind: r[n] == b[n-1], so shift +1 is the truth.
        shift_frames = [2, 3, 4, 5]
        for frame in shift_frames:
            with open(os.path.join(align, f"{REF_PREFIX}{frame:05d}.bin"), "wb") as handle:
                handle.write(base[frame])
            with open(os.path.join(align, f"{OURS_PREFIX}{frame:05d}.bin"), "wb") as handle:
                handle.write(base[frame - 1])
        results, best = sweep(align, shift_frames, 2)
        check("sweep finds the offset that aligns a one-frame-late dump", best == 1, f"best {best:+d}")
        at_zero = next(mean for shift, mean, _ in results if shift == 0)
        at_one = next(mean for shift, mean, _ in results if shift == best)
        check("and does not call the misaligned comparison the cheap one", at_one < at_zero)
        whole = run_comparison(align, shift_frames, span=2)
        check("comparing at the measured shift finds no divergence", whole.agrees,
              f"{ {f: len(o) for f, o in whole.per_frame.items()} }")
        check("and reports the shift it used", whole.shift == 1, f"{whole.shift:+d}")
        check("our end of the last frame has nothing left to compare", 5 in whole.dropped,
              f"{whole.dropped}")
        # The same data, compared as a shift-0 reader would: a one-frame-late dump
        # must look like a divergence, or the sweep is the only thing standing
        # between this tool and a false "agree".
        naive = compare(align, shift_frames, 0)
        check("and the misaligned comparison is not the quiet one", not naive.agrees)

        # The shape #526 actually is: aligned only at a shift, and divergent there.
        # A one-byte change to every one of our images, which is only visible once
        # the shift is applied, is the case a comparator has to survive.
        # Our image for reference frame `f` is r{f+1} here, and it holds base[f].
        stuck = 0xFFB000 - RAM_BASE
        for frame in shift_frames:
            if not os.path.exists(os.path.join(align, f"{OURS_PREFIX}{frame + 1:05d}.bin")):
                continue
            mutate(align, frame + 1, OURS_PREFIX, stuck, (base[frame][stuck] + 0x5A) & 0xFF)
        whole = run_comparison(align, shift_frames, span=2)
        check("a divergence only visible at the measured shift is found",
              whole.differs_in_all == [stuck],
              f"{[address(o) for o in whole.differs_in_all]}")
        check("and it is not diluted into frame phase", whole.phase == [],
              f"{[address(o) for o in whole.phase[:3]]}")
        with contextlib.redirect_stdout(io.StringIO()) as out:
            print_report(whole)
        check("the report prints the sweep", "shift" in out.getvalue())
        check("the report says which shift it compared at", "shift    +1 (measured" in out.getvalue(),
              out.getvalue()[:200])

        print("selftest: an anchored comparison")
        # The anchor of #526: both dumpers stop at one instruction, so a frame
        # number is an arrival and the two machines are being looked at at the same
        # moment. Two things have to stay true of that mode, and they are the two
        # ways this harness fails worst: going blind, and inventing a divergence out
        # of ordinary frame phase.
        anchor = "584"
        planted = 0xFFE100 - RAM_BASE
        at_anchor = os.path.join(work, "anchored")
        os.mkdir(at_anchor)
        build(at_anchor, frames, images)
        for frame in frames:
            mutate(at_anchor, frame, OURS_PREFIX, planted, images[frame][planted] ^ 0x5C)
        report = compare(at_anchor, frames)
        report.anchor = anchor
        with contextlib.redirect_stdout(io.StringIO()) as out:
            print_report(report)
        check(
            "an anchored comparison still sees a difference planted in every frame",
            report.differs_in_all == [planted],
            f"{[address(o) for o in report.differs_in_all]}",
        )
        check("and prints the instruction it stopped at",
              f"point    both sides stopped before 0x{anchor}" in out.getvalue(),
              out.getvalue()[:200])

        # Our images holding the previous arrival: one raster of phase, nothing else.
        phase = os.path.join(work, "anchored-phase")
        os.mkdir(phase)
        phase_frames = [4, 5, 6, 7]
        for frame in phase_frames:
            with open(os.path.join(phase, f"{REF_PREFIX}{frame:05d}.bin"), "wb") as handle:
                handle.write(images[frame])
            with open(os.path.join(phase, f"{OURS_PREFIX}{frame:05d}.bin"), "wb") as handle:
                handle.write(images[frame - 1])
        report = run_comparison(phase, phase_frames, span=2)
        report.anchor = anchor
        with contextlib.redirect_stdout(io.StringIO()) as out:
            print_report(report)
        check(
            "an anchored pair one arrival out of step is alignment, not divergence",
            report.shift == 1 and report.agrees and not report.differs_in_all,
            f"shift {report.shift:+d}, {len(report.differs_in_all)} persistent",
        )
        check("and the report says the arrivals are apart rather than naming bytes",
              "arrivals apart" in out.getvalue(), out.getvalue()[-300:])

        print("selftest: broken input is an error, not an agreement")
        truncated = os.path.join(work, "trunc")
        os.mkdir(truncated)
        build(truncated, [7], images)
        with open(os.path.join(truncated, "r00007.bin"), "r+b") as handle:
            handle.truncate(123)
        try:
            compare(truncated, [7])
            check("a truncated dump raises", False, "compare() accepted it")
        except HarnessError as error:
            check("a truncated dump raises", "expected 65536" in str(error), str(error))

        missing = os.path.join(work, "missing")
        os.mkdir(missing)
        with open(os.path.join(missing, "b00007.bin"), "wb") as handle:
            handle.write(images[7])
        try:
            compare(missing, [7])
            check("a frame with only one side raises", False, "compare() accepted it")
        except HarnessError:
            check("a frame with only one side raises", True)
        check("a frame with one side only is not a pair", frames_in(missing) == ([7], []))
        try:
            frames_requested("", os.path.join(work, "not-there"))
            check("a directory that is not there is an error, not a traceback", False)
        except HarnessError:
            check("a directory that is not there is an error, not a traceback", True)
        check("the two sides are listed apart, so a shift can still be found",
              frames_in(align) == ([2, 3, 4, 5], [2, 3, 4, 5]))

    print(f"selftest: {'FAIL' if failures else 'pass'} ({len(failures)} failure(s))")
    return 1 if failures else 0


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="Compare a reference emulator's work RAM dumps against ours (#525).",
        epilog="exit status: 0 agree, 1 disagree, 2 the harness could not run",
    )
    parser.add_argument(
        "directory",
        nargs="?",
        help="directory holding bNNNNN.bin (reference) and rNNNNN.bin (ours)",
    )
    parser.add_argument(
        "frames",
        nargs="*",
        help="frames to compare: 595-605, 600, or both (default: every frame dumped)",
    )
    parser.add_argument(
        "--sweep",
        type=int,
        default=4,
        help="frame offsets to try either way (default 4); 0 skips the sweep",
    )
    parser.add_argument(
        "--anchor",
        metavar="ADDRESS",
        default="",
        help="address both dumpers stopped at, as given to --stop-at-pc: labels the "
        "report and says what a non-zero shift means at an anchor",
    )
    parser.add_argument(
        "--selftest",
        action="store_true",
        help="check this harness on canned dumps and exit (part of `just check`)",
    )
    args = parser.parse_args(argv)

    if args.selftest:
        return selftest()
    if not args.directory:
        parser.error("a directory of dumps is needed (or --selftest)")

    try:
        frames = frames_requested(",".join(args.frames), args.directory)
        report = run_comparison(args.directory, frames, args.sweep)
        report.anchor = args.anchor.strip().removeprefix("0x").upper()
        if report.shift:
            print(
                f"compare: the two frame counters differ by {report.shift:+d}; "
                "the table below is ours read at that offset",
                file=sys.stderr,
            )
    except HarnessError as error:
        print(f"compare: {error}", file=sys.stderr)
        return 2
    print_report(report)
    return 0 if report.agrees else 1


if __name__ == "__main__":
    sys.exit(main())
