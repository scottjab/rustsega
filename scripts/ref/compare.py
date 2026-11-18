#!/usr/bin/env python3
"""Compare the reference's work RAM against ours, and say whether they agree.

Part of the external reference oracle of #525. Reads a directory of raw work-RAM
images written by ``run-ref.sh`` (``bNNNNN.bin``) and ``run-ours.sh``
(``rNNNNN.bin``) - one 64 KiB image of the 68000 address space
``$FF0000-$FFFFFF`` per frame - and reports whether the two emulators agree.

usage::

    compare.py OUTDIR [595-605 600 ...] [--sweep 4] [--anchor 0x584]
    compare.py OUTDIR [47-51] [--anchor 0x584] --meter [0xFFFF2C,0xFFFF30]
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

**``--meter`` reads a cycle meter instead of a diff** (#530). Some carts park a
68000 register set in work RAM at every vertical interrupt, and two of the parked
registers can be pointers that only ever move forwards through one run of the
program: ``Sonic & Knuckles (World)`` parks the ROM input and work-RAM output
cursors of a background decompressor it preempts at every V-Int, so those two longs
say how far the 68000 got between two vertical interrupts and are a measurement of
68000 time per frame that needs no instrumentation on either side (#529). Give
``--meter`` two such addresses and it reports, per arrival, ``ours - ref`` for each
of them and the **signed per-frame lead** that follows: positive means our 68000
reached further through the stream by that instant, negative means the reference
had, and a lead that grows by the same amount every arrival is a fixed number of
cycles a frame, which is what #530 is about. A count of differing addresses cannot
say that, and neither can a count of bytes: only the sign and the slope do.

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

METER_LONG = 4
"""Bytes a meter reads: one big-endian long, because the cursors this is for are the
68000 registers a cart parks with `MOVEM.L` (#529)."""

METER_DEFAULT = "0xFFFF2C,0xFFFF30"
"""What a bare `--meter` reads: S&K's parked decoder cursors.

`$FFFF2C` is A0, the ROM input pointer, and `$FFFF30` is A1, the work-RAM output
pointer, of the Kosinski decompressor `Sonic & Knuckles (World)` runs in the
background and saves at `$FFFF10-$FFFF37` from inside its own V-Int (#529). A cart
that parks its progress somewhere else wants its own addresses, and a cart that
park nothing has no cycle meter at all.
"""


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


def ranges_of(frames, limit=10):
    """Arrival numbers as `454-461, 466`: a meter's step list is long and mostly runs.

    Separate from :func:`ranges` because that one works in byte offsets and prints
    addresses; the two collapse runs the same way and should not pretend otherwise.
    """
    ordered = sorted(frames)
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
        runs = runs[:limit]
        return ", ".join(f"{a}-{b}" if a != b else f"{a}" for a, b in runs) + "..."
    return ", ".join(f"{start}-{end}" if start != end else f"{start}" for start, end in runs)


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


@dataclass
class Run:
    """Consecutive arrivals the meter was measuring one uninterrupted stretch of work.

    `steps` is the arrival count the run spans, `end - start`, not the number of
    samples in it: a meter sampled every fifth arrival over 45..60 measured fifteen
    frames of work, and dividing by three would call it five times the rate it was.
    Frame numbers are arrivals at a `--stop-at-pc` anchor, which is what makes the
    difference of two of them a frame count.
    """

    start: int
    end: int
    #: change in `ours - ref` across the run, signed
    gain: int
    steps: int

    @property
    def per_frame(self):
        """Signed lead gained per arrival over the run: the slope #530 is about."""
        return self.gain / self.steps if self.steps else 0.0


@dataclass
class Meter:
    """What one cycle meter found. Printed by :func:`print_meter`, asserted by the self-test.

    `shift` comes from the same alignment sweep the byte comparison uses, and it is
    not optional here: read our cursor one arrival late and a machine that is
    perfectly in step reports itself ahead by a frame's worth of work, which is the
    one thing this tool exists to be able to tell apart from a real lead.
    """

    directory: str
    #: the 68000 addresses read, ascending
    addresses: list
    #: reference frames that had a pair at `shift`, ascending
    frames: list = field(default_factory=list)
    shift: int = 0
    shift_measured: bool = False
    anchor: str = ""
    #: address -> frame -> (reference value, our value)
    values: dict = field(default_factory=dict)
    #: address -> frame -> our value - reference value, signed
    lead: dict = field(default_factory=dict)
    #: address -> the :class:`Run` spans both machines were decoding across
    runs: dict = field(default_factory=dict)
    #: address -> frame -> that frame's :func:`classify_steps` label
    labels: dict = field(default_factory=dict)
    #: reference frames with no image on our side at this shift
    dropped: list = field(default_factory=list)

    @property
    def agrees(self):
        """Does every meter read the same value on both sides at every arrival?"""
        return all(v == 0 for per_frame in self.lead.values() for v in per_frame.values())

    @property
    def unwritten(self):
        """Did every meter read 0 on both sides in every frame, which is not a measurement?

        A cursor no cart has parked there reads equal on both machines, so :attr:`agrees`
        is true of it - and "agree" out of an oracle that was pointed at nothing is the
        failure this harness calls fatal everywhere else. Worth its own exit status so a
        scripted run cannot read "the two machines agree about 68000 time per frame" out
        of an address that holds nothing.
        """
        return all(
            ref == 0 and our == 0
            for per_frame in self.values.values()
            for (ref, our) in per_frame.values()
        )

    def per_frame(self, address):
        """Signed lead per arrival for one meter, over the run(s) that were not load boundaries."""
        steps = sum(run.steps for run in self.runs.get(address, []))
        if not steps:
            return 0.0
        gain = sum(run.gain for run in self.runs.get(address, []))
        return gain / steps

    def step_signs(self, address):
        """`(ahead, behind, level)` over the measured steps of one meter.

        Counts steps rather than arrivals: a machine ahead by the same amount every
        frame is the cycle gap this tool is for, and one ahead across a single step of
        several thousand is a load restart that went undetected. The lead column alone
        cannot tell those two apart.
        """
        lead = self.lead.get(address, {})
        labels = self.labels.get(address, {})
        ahead = behind = level = 0
        for first, second in zip(self.frames, self.frames[1:]):
            if labels.get(second) != MEASURED:
                continue
            step = lead[second] - lead[first]
            ahead += step > 0
            behind += step < 0
            level += step == 0
        return ahead, behind, level


def parse_meters(spec):
    """`--meter 0xFFFF2C,0xFFFF30` -> ``[0xFFFF2C, 0xFFFF30]`, ascending and deduplicated.

    Empty means the default pair, and anything that is not a number inside the
    compared window is the harness being pointed at nonsense, which is an error
    rather than a silent empty report.
    """
    spec = (spec or METER_DEFAULT).strip()
    found = set()
    for part in spec.split(","):
        part = part.strip()
        if not part:
            continue
        try:
            value = int(part, 0)
        except ValueError:
            raise HarnessError(f"meter address {part!r} is not a number: give it as 0xFFFF2C") from None
        if not RAM_BASE <= value <= RAM_BASE + RAM_SIZE - METER_LONG:
            raise HarnessError(
                f"meter address 0x{value:06X} is outside the dumped region "
                f"0x{RAM_BASE:06X}-0x{RAM_BASE + RAM_SIZE - 1:06X}"
            )
        found.add(value)
    if not found:
        raise HarnessError("--meter wants at least one address")
    return sorted(found)


def read_long(image, address):
    """The big-endian long at a 68000 address inside a dump."""
    offset = address - RAM_BASE
    if offset < 0 or offset + METER_LONG > len(image):
        raise HarnessError(
            f"meter address 0x{address:06X} is not readable in a {len(image)}-byte image "
            f"based at 0x{RAM_BASE:06X}"
        )
    return int.from_bytes(image[offset : offset + METER_LONG], "big")


MEASURED = "measured"
"""A step where both machines moved their cursor, so the change in lead is a rate."""

RESTART = "restart"
"""A step where a cursor went backwards, or was 0: a new load started (#529), so the
lead across it belongs to two different stretches of compressed art."""

STOPPED = "stopped"
"""A step where one machine's cursor moved and the other's did not. One of them had
finished the load and stopped parking, which is the frame-count signature of #529 and
a finding in its own right - but not a rate: the lead changes by the whole advance of
the one still running, which would swamp the dozen bytes a frame this tool is for.
On S&K arrivals 454-461 of one load read exactly this, ours parked at the end of the
stream while the reference was still in it."""

IDLE = "idle"
"""A step where neither cursor moved: no decode is running, so the parked frame is
dead state and says nothing about either machine."""


def classify_steps(frames, values):
    """Label every step of one meter: :data:`MEASURED`, :data:`STOPPED`, :data:`IDLE`, :data:`RESTART`.

    `values` is ``frame -> (reference cursor, our cursor)``. The labels are decided by
    the two cursors and never by the lead: a step where the lead *shrinks* is a frame
    where the reference got further, which is exactly what a rate has to be allowed to
    average over, while a step where a cursor goes backwards is a new load starting and
    on S&K that moves the pointer by tens of kilobytes (#529: every load starts from the
    same pointers, so the lead resets to about zero). Averaging the two together is how
    a tool reports the difference between two unrelated stretches of art as a cycle gap.

    A cursor of 0 is a :data:`RESTART` too: it is the parked frame before the game ever
    wrote it, which is why arrival 46 of S&K reads 0 and arrival 47 does not.
    """
    labels = {}
    for earlier, later in zip(frames, frames[1:]):
        ref_was, our_was = values[earlier]
        ref_now, our_now = values[later]
        if min(ref_now, our_now) == 0 or ref_now < ref_was or our_now < our_was:
            labels[later] = RESTART
        elif ref_now == ref_was and our_now == our_was:
            labels[later] = IDLE
        elif ref_now == ref_was or our_now == our_was:
            labels[later] = STOPPED
        else:
            labels[later] = MEASURED
    return labels


def cursor_runs(frames, labels, lead):
    """The maximal :data:`MEASURED` stretches of one meter, with their slope.

    A run is broken by anything that is not a measured step, so the slope of a run is
    always a rate over frames in which both machines were decoding.
    """
    runs = []
    group = [frames[0]] if frames else []
    for frame in frames[1:]:
        if labels.get(frame) == MEASURED:
            group.append(frame)
        else:
            runs.append(_one_run(group, lead))
            group = [frame]
    runs.append(_one_run(group, lead))
    return [run for run in runs if run and run.steps], [
        frame for frame in frames if labels.get(frame) in (RESTART, STOPPED, IDLE)
    ]


def _one_run(group, lead):
    """One :class:`Run` over the arrival numbers in `group`, which are consecutive samples."""
    if len(group) < 2:
        return None
    return Run(
        start=group[0],
        end=group[-1],
        gain=lead[group[-1]] - lead[group[0]],
        steps=group[-1] - group[0],
    )


def measure(directory, addresses, frames, shift=0):
    """Read each meter out of every frame pair at `shift`: the values, the lead, the slope.

    Frames whose shifted image was never dumped drop out exactly as they do in
    :func:`compare`; a truncated or missing reference image is fatal there and here.
    """
    meter = Meter(directory=directory, addresses=list(addresses), shift=shift)
    for frame in frames:
        reference = load_one(directory, REF_PREFIX, frame)
        try:
            ours = load_one(directory, OURS_PREFIX, frame + shift)
        except MissingDump:
            if shift == 0:
                raise
            meter.dropped.append(frame)
            continue
        meter.frames.append(frame)
        for address in addresses:
            ref_value = read_long(reference, address)
            our_value = read_long(ours, address)
            meter.values.setdefault(address, {})[frame] = (ref_value, our_value)
            meter.lead.setdefault(address, {})[frame] = our_value - ref_value
    if not meter.frames:
        raise HarnessError(f"no frame of {len(frames)} could be metered in {directory}")
    for address in addresses:
        meter.labels[address] = classify_steps(meter.frames, meter.values[address])
        meter.runs[address], _ = cursor_runs(meter.frames, meter.labels[address], meter.lead[address])
    return meter


def run_meter(directory, addresses, frames, span=4):
    """Measure the frame alignment, then meter at it: the cursor twin of :func:`run_comparison`.

    Same order and for the same reason: a cursor read at the wrong shift is a
    different arrival, and the lead printed for it is the distance between two
    arrivals rather than the difference between two machines.
    """
    if span <= 0:
        return measure(directory, addresses, frames, 0)
    _, shift = sweep(directory, frames, span)
    meter = measure(directory, addresses, frames, shift)
    meter.shift_measured = True
    return meter


def print_meter(meter, stream=None):
    """The per-arrival table, the slope, and a RESULT line in the same shape as the diff report."""
    stream = sys.stdout if stream is None else stream

    def say(text):
        print(text, file=stream)

    say(f"dir      {meter.directory}")
    if meter.anchor:
        say(f"point    both sides stopped before 0x{meter.anchor} (frame N = arrival N there)")
    if meter.frames:
        say(
            f"frames   {len(meter.frames)}: {meter.frames[0]}..{meter.frames[-1]} "
            "(reference numbering)"
        )
    if meter.shift:
        basis = "measured" if meter.shift_measured else "asked for"
        say(f"shift    {meter.shift:+d} ({basis}: reference frame N read against our frame N{meter.shift:+d})")
    else:
        basis = "measured" if meter.shift_measured else "assumed, --sweep 0"
        say(f"shift    +0 ({basis})")
    if meter.dropped:
        say(f"         {len(meter.dropped)} frame(s) had no image on our side at this shift")
    for address in meter.addresses:
        lead = meter.lead[address]
        labels = meter.labels.get(address, {})
        say("")
        say(f"meter 0x{address:06X}    lead = ours - ref, signed")
        say("   frame         ref        ours        lead   per frame")
        previous = None
        for frame in meter.frames:
            ref_value, our_value = meter.values[address][frame]
            label = labels.get(frame, "")
            step = "-" if previous is None or label != MEASURED else f"{lead[frame] - previous:+d}"
            say(
                f"{frame:8d}  {ref_value:9X}  {our_value:9X}  {lead[frame]:+10d}  {step:>9}"
                f"{'' if label in ('', MEASURED) else ' ' + label}"
            )
            previous = lead[frame]
        for run in meter.runs.get(address, []):
            say(
                f"  run {run.start}..{run.end}: lead {run.gain:+d} over {run.steps} "
                f"frame(s) = {run.per_frame:+.1f} per frame"
            )
        if not meter.runs.get(address):
            say("  no two arrivals where both machines were decoding, so no slope")
        for label, what in (
            (RESTART, "the cursor went backwards, so the lead is a new load's, not a frame's"),
            (STOPPED, "only one machine was still decoding: one of them finished the load"),
            (IDLE, "neither cursor moved: no decode is running, so the table is dead state"),
        ):
            frames = [frame for frame in meter.frames if labels.get(frame) == label]
            if frames:
                say(f"  {label} at {ranges_of(frames)}: {what}")
        ahead, behind, level = meter.step_signs(address)
        say(
            f"  ours ahead in {ahead} measured step(s) of {ahead + behind + level}, "
            f"behind in {behind}, level in {level}; "
            f"per-frame lead {meter.per_frame(address):+.1f}"
        )
    say("")
    if meter.agrees:
        say("RESULT: agree - every meter reads the same value on both sides at every arrival")
    else:
        # The meter with the steepest slope is the one to name in the headline: a lead
        # that is large but flat is a load boundary, and a lead of 12 bytes a frame is
        # the finding, wherever its absolute value happens to sit.
        steepest = max(meter.addresses, key=lambda a: abs(meter.per_frame(a)))
        slope = meter.per_frame(steepest)
        direction = "ours ahead" if slope > 0 else "reference ahead" if slope < 0 else "no slope"
        leads = meter.lead[steepest].values()
        say(
            f"RESULT: disagree - 0x{steepest:06X} leads {min(leads):+d}..{max(leads):+d}, "
            f"{slope:+.1f} per frame ({direction})"
        )


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


def mutate_long(directory, frame, side, addr, value):
    """Write one big-endian long of one dump on one side, for the meter's self-test."""
    path = os.path.join(directory, f"{side}{frame:05d}.bin")
    with open(path, "r+b") as handle:
        handle.seek(addr - RAM_BASE)
        handle.write(int(value).to_bytes(METER_LONG, "big"))


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

        print("selftest: the cycle meter (#530)")
        # S&K parks the two cursors of its background decompressor in work RAM at
        # every V-Int (#529), so two longs say how far each machine's 68000 got in a
        # frame. Four ways for that measurement to be wrong are worth canned dumps:
        # reporting a lead where the cursors match, getting the sign of one that does
        # not match, calling it a count instead of a per-frame rate, and averaging a
        # load restart into the slope.
        def capture(printer, thing):
            """One printer into a string: most of these checks are about the printed line."""
            with contextlib.redirect_stdout(io.StringIO()) as out:
                printer(thing)
            return out.getvalue()

        meter_frames = [47, 48, 49, 50, 51]
        meter_images = churned_images(60, seed=0x530)
        a0, a1 = 0xFFFF2C, 0xFFFF30
        check("a bare --meter reads S&K's two parked cursors",
              parse_meters("") == [a0, a1], f"{[f'0x{a:06X}' for a in parse_meters('')]}")
        try:
            parse_meters("0x10")
            check("a meter outside the dumped region is an error", False, "it was accepted")
        except HarnessError as error:
            check("a meter outside the dumped region is an error",
                  "outside the dumped region" in str(error), str(error))
        try:
            parse_meters("the-pc")
            check("a meter that is not an address is an error", False, "it was accepted")
        except HarnessError as error:
            check("a meter that is not an address is an error",
                  "not a number" in str(error), str(error))

        def meter_case(name, trajectory, frames=None):
            """One directory of dumps with `trajectory[frame] = (ref cursor, our cursor)`."""
            case = os.path.join(work, name)
            os.mkdir(case)
            build(case, sorted(trajectory) if frames is None else frames, meter_images)
            for frame, (ref_value, our_value) in trajectory.items():
                mutate_long(case, frame, REF_PREFIX, a1, ref_value)
                mutate_long(case, frame, OURS_PREFIX, a1, our_value)
            return case

        step = [0, 2500, 5100, 7700, 10300]
        cursor_at = lambda index: 0xFFFF4E19 + step[index]
        in_step = meter_case(
            "meter-in-step",
            {frame: (cursor_at(i), cursor_at(i)) for i, frame in enumerate(meter_frames)},
        )
        meter = run_meter(in_step, [a1], meter_frames, span=0)
        check("cursors that match give no lead",
              meter.agrees and meter.per_frame(a1) == 0.0, f"{meter.lead[a1]}")
        with contextlib.redirect_stdout(io.StringIO()) as out:
            print_meter(meter)
        check("and a meter with nothing in it says agree", "RESULT: agree" in out.getvalue(),
              out.getvalue()[-200:])

        ahead = meter_case(
            "meter-ahead",
            {frame: (cursor_at(i), cursor_at(i) + 46 + 11 * i)
             for i, frame in enumerate(meter_frames)},
        )
        meter = run_meter(ahead, [a1], meter_frames, span=0)
        check("a cursor planted in our favour is seen at its arrival",
              meter.lead[a1][47] == 46, f"{meter.lead[a1][47]:+d}")
        check("its slope is a signed per-frame lead, not a count",
              meter.per_frame(a1) == 11.0, f"{meter.per_frame(a1):+.1f}")
        check("and every step of it is counted as ours ahead",
              meter.step_signs(a1) == (4, 0, 0), f"{meter.step_signs(a1)}")
        with contextlib.redirect_stdout(io.StringIO()) as out:
            print_meter(meter)
        check("the report prints the slope with its sign", " +11.0 per frame" in out.getvalue(),
              out.getvalue()[-260:])
        check("and names which machine is ahead", "ours ahead" in out.getvalue(),
              out.getvalue()[-140:])

        behind = meter_case(
            "meter-behind",
            {frame: (cursor_at(i), cursor_at(i) - 46 - 11 * i)
             for i, frame in enumerate(meter_frames)},
        )
        meter = run_meter(behind, [a1], meter_frames, span=0)
        check("a cursor planted the other way is negative, not merely different",
              meter.per_frame(a1) == -11.0 and meter.lead[a1][51] == -90,
              f"{meter.per_frame(a1):+.1f}, {meter.lead[a1][51]:+d}")
        check("our cursor falling behind is a measured step, not a restart",
              set(meter.labels[a1].values()) == {MEASURED}, f"{meter.labels[a1]}")
        with contextlib.redirect_stdout(io.StringIO()) as out:
            print_meter(meter)
        check("and the report says the reference is the one ahead",
              "reference ahead" in out.getvalue(), out.getvalue()[-160:])

        # A long window sampled every tenth arrival, which is what a first pass over
        # 600 frames looks like. The lead grows 11 an arrival, so the answer is 11.0
        # per frame; divide by the three samples instead of the twenty arrivals between
        # the ends and the report says 110 and nobody notices.
        sparse = meter_case(
            "meter-sparse",
            {frame: (0xFFFF4E19 + 300 * frame, 0xFFFF4E19 + 300 * frame + 11 * frame)
             for frame in (10, 20, 30)},
        )
        meter = run_meter(sparse, [a1], [10, 20, 30], span=0)
        check("a meter sampled every tenth arrival divides by the frames it spanned",
              meter.per_frame(a1) == 11.0, f"{meter.per_frame(a1):+.1f}")

        # Arrival 49 is the next load starting: the output pointer goes back to a lower
        # address on both sides. A lead that grows +20 a frame and then sits flat is
        # +20 over the one frame that worked and 0 over the two afterwards; averaged
        # across the restart the whole window reads -3.25, which is the same number
        # with the wrong sign. This is the one way the meter can invent a finding.
        restart = {
            frame: (cursor_at(i), cursor_at(i) + 20 + 20 * i)
            for i, frame in enumerate(meter_frames[:2])
        }
        restart.update(
            {frame: (0x0040_0000 + 900 * i, 0x0040_0000 + 900 * i + 7)
             for i, frame in enumerate(meter_frames[2:])}
        )
        loaded = meter_case("meter-restart", restart)
        meter = run_meter(loaded, [a1], meter_frames, span=0)
        check("a cursor that goes backwards is a load restart, not a frame",
              meter.labels[a1][49] == RESTART, f"{meter.labels[a1]}")
        check("and the window splits into the two loads",
              [(r.start, r.end) for r in meter.runs[a1]] == [(47, 48), (49, 51)],
              f"{[(r.start, r.end) for r in meter.runs[a1]]}")
        check("so the per-frame lead is the two runs, not the slope across their join",
              abs(meter.per_frame(a1) - 20 / 3) < 0.01, f"{meter.per_frame(a1):+.2f}")
        with contextlib.redirect_stdout(io.StringIO()) as out:
            print_meter(meter)
        check("and the report says the lead is not claimed across the restart",
              "restart at 49" in out.getvalue(), out.getvalue()[-300:])

        # Arrivals 454-461 of one #530 load are the two machines finishing at different
        # times: ours parks and stops moving while the reference is still writing, then
        # both go quiet. Neither step is a rate, and the first of them moves the lead by
        # 694 bytes on its own: counted as a frame it turned a window where we are ahead
        # into one whose headline said the reference was.
        stopping = {frame: (cursor_at(i), cursor_at(i) + 40 + 10 * i)
                    for i, frame in enumerate(meter_frames[:3])}
        stopping.update({frame: (cursor_at(i) + 9000, stopping[meter_frames[2]][1])
                         for i, frame in enumerate(meter_frames[3:])})
        stopped_case = meter_case("meter-one-stopped", stopping)
        meter = run_meter(stopped_case, [a1], meter_frames, span=0)
        check("a frame where only one machine is still decoding is not a measured step",
              meter.labels[a1][50] == STOPPED and meter.labels[a1][49] == MEASURED,
              f"{meter.labels[a1]}")
        check("so the slope is the two comparable frames, not the four sampled ones",
              meter.per_frame(a1) == 10.0, f"{meter.per_frame(a1):+.1f}")
        check("and who finished first is still in the report, because that is #529's "
              "frame-count signature",
              "stopped at 50-51" in capture(print_meter, meter),
              capture(print_meter, meter)[-200:])

        frozen = {frame: (cursor_at(0) + 5000, cursor_at(0) + 5000 + 30)
                  for frame in meter_frames}
        frozen_case = meter_case("meter-frozen", frozen)
        meter = run_meter(frozen_case, [a1], meter_frames, span=0)
        check("a cursor nobody is moving measures nothing",
              set(meter.labels[a1].values()) == {IDLE} and not meter.runs[a1],
              f"{meter.labels[a1]}")
        check("and a meter that measured nothing does not claim a rate",
              meter.per_frame(a1) == 0.0 and "no slope" in capture(print_meter, meter),
              capture(print_meter, meter)[-200:])

        # Our images holding the previous arrival. The byte comparison calls this
        # alignment and the meter has to agree: read at the measured shift the lead is
        # zero, and read at shift 0 it is a frame's worth of work pretending to be a
        # cycle gap, which is the exact mistake #526 was made of.
        stepped = os.path.join(work, "meter-stepped")
        os.mkdir(stepped)
        for index, frame in enumerate(meter_frames):
            with open(os.path.join(stepped, f"{REF_PREFIX}{frame:05d}.bin"), "wb") as handle:
                handle.write(meter_images[frame])
            with open(os.path.join(stepped, f"{OURS_PREFIX}{frame:05d}.bin"), "wb") as handle:
                handle.write(meter_images[frame - 1])
            mutate_long(stepped, frame, REF_PREFIX, a1, cursor_at(index))
            mutate_long(stepped, frame, OURS_PREFIX, a1, cursor_at(index - 1))
        meter = run_meter(stepped, [a1], meter_frames, span=2)
        check("a cursor one arrival behind is alignment, not a lead",
              meter.shift == 1 and meter.agrees,
              f"shift {meter.shift:+d}, {meter.lead[a1]}")
        check("which is worth more than the same data read at the shift it was not taken at",
              run_meter(stepped, [a1], meter_frames, span=0).per_frame(a1) != 0.0)

        # An address no cart has parked in reads equal everywhere, which is not the same
        # as a measurement: an oracle that says "agree" about nothing is the failure this
        # file calls fatal in its own inputs, so it is an exit status rather than a pass.
        blank = os.path.join(work, "meter-blank")
        os.mkdir(blank)
        build(blank, meter_frames, {frame: bytes(RAM_SIZE) for frame in meter_frames})
        meter = run_meter(blank, [a1], meter_frames, span=0)
        check("a cursor nothing parked at is called what it is, not agreement",
              meter.unwritten, f"{meter.values[a1][47]}")
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()) as err:
            status = main([blank, "47-51", "--meter", f"0x{a1:06X}", "--sweep", "0"])
        check("and pointing the meter at it fails the run instead of passing it",
              status == 2 and "nothing was measured" in err.getvalue(), f"status {status}")
        check("a parked cursor is not unwritten",
              not run_meter(ahead, [a1], meter_frames, span=0).unwritten)

        with contextlib.redirect_stdout(io.StringIO()) as out:
            status = main([ahead, "47-51", "--meter", f"0x{a1:06X}", "--sweep", "0"])
        check("--meter on the command line reaches the meter and not the diff",
              status == 1 and "per-frame lead +11.0" in out.getvalue(),
              f"status {status}: {out.getvalue()[-160:]}")
        with contextlib.redirect_stderr(io.StringIO()) as err:
            status = main([ahead, "47-51", "--meter", "0x10"])
        check("a bad meter address is the harness failing, not an agreement",
              status == 2 and "outside the dumped" in err.getvalue(), f"status {status}")

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
        "--meter",
        metavar="ADDRESS,ADDRESS",
        nargs="?",
        default="",
        const=METER_DEFAULT,
        help="read these addresses as a cycle meter instead of diffing the image: one "
        f"big-endian long each, comma-separated (bare --meter is {METER_DEFAULT}, "
        "S&K's parked decoder cursors, #529). Reports ours - ref per arrival and the "
        "signed lead per frame. Use it at a --stop-at-pc anchor",
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

    # A Report and a Meter carry the same three things the harness needs before it
    # prints: the shift it aligned at, the anchor that says what the frame numbers
    # mean, and whether the two sides agree. Only the printing differs.
    try:
        frames = frames_requested(",".join(args.frames), args.directory)
        if args.meter:
            outcome = run_meter(args.directory, parse_meters(args.meter), frames, args.sweep)
        else:
            outcome = run_comparison(args.directory, frames, args.sweep)
        outcome.anchor = args.anchor.strip().removeprefix("0x").upper()
        if outcome.shift:
            print(
                f"compare: the two frame counters differ by {outcome.shift:+d}; "
                "the table below is ours read at that offset",
                file=sys.stderr,
            )
    except HarnessError as error:
        print(f"compare: {error}", file=sys.stderr)
        return 2
    if args.meter:
        print_meter(outcome)
        if outcome.unwritten:
            print(
                "compare: every meter read 0 in every frame, so those addresses hold no "
                "parked cursor in this cart and nothing was measured",
                file=sys.stderr,
            )
            return 2
        return 0 if outcome.agrees else 1
    print_report(outcome)
    return 0 if outcome.agrees else 1


if __name__ == "__main__":
    sys.exit(main())
