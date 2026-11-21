#!/usr/bin/env python3
"""Stand-in for ``x-terminal-emulator``, so blastem's ``-d`` debugger is a pipe.

Part of the external reference oracle of #525. Blastem's ``-d`` option makes two
FIFOs - ``/tmp/blastem_input`` (debugger commands in) and ``/tmp/blastem_output``
(text out) - and then execs ``x-terminal-emulator -title "BlastEm Debugger" -e
<path>/termhelper`` to drive them. NixOS has no ``x-terminal-emulator``, so
``blastem -g -d`` segfaults out of the box. Putting this on ``PATH`` in place of
the terminal makes the debugger scriptable with no display and no patch to
blastem: this process *is* the terminal, and it speaks blastem's own documented
terminal protocol at the FIFOs it makes for itself. Nothing of blastem's is
copied or reimplemented here beyond using that protocol - see the GPL note in
README.md.

usage::

    terminal-standin.py <command-file> <log-file>

``<command-file>`` is one debugger command per line; ``#`` starts a comment. The
commands are released one per ``>`` prompt rather than all at once, which is what
makes a dump land on the frame it names: ``frames N`` only returns to a prompt
after N frames have run, so pacing follows emulation and not wall clock. The
debugger transcript goes to ``<log-file>``.

Blastem execs this with its own arguments (``-title ... -e .../termhelper``) and
they are ignored on purpose: we are not launching its terminal helper, we are
replacing the terminal. The two paths above are how the caller says what to do.

Environment:

``REF_DRIVE_TIMEOUT``
    Seconds for the whole session, default 120. Blastem ignores ``SIGTERM``, so
    this is the only thing that ends a wedged run from in here; ``run-ref.sh``
    wraps it in ``timeout -k 5`` as well.
"""

import os
import re
import select
import subprocess
import sys
import tempfile
import time

INPUT_FIFO = "/tmp/blastem_input"
"""Commands go in here; blastem creates it."""

OUTPUT_FIFO = "/tmp/blastem_output"
"""Text comes out of here; blastem creates it."""

PROMPT = re.compile(rb"(?m)^>")
"""The debugger prompt, counted at line starts only.

Counting every ``>`` byte reads the ``>`` of other people's output as a prompt
and releases a command early - which lands a ``save`` on the wrong frame, the one
failure this driver must not have. Blastem prints its prompt at the start of a
line (``> ``), so a line-anchored match is both sufficient and much rarer than
the bare byte."""


def open_fifo(path, flags, deadline):
    """Open ``path``, retrying until ``deadline``.

    The FIFO is created by blastem, which may not have got there first: this is
    exec'd by it and races it by milliseconds either way. ``O_NONBLOCK`` on the
    write side is what stops ``open`` blocking forever before the other end
    exists.
    """
    while time.monotonic() < deadline:
        try:
            return os.open(path, flags)
        except OSError:
            time.sleep(0.05)
    raise SystemExit(f"terminal-standin: could not open {path} before the timeout")


def read_commands(path):
    """The command file as a list of byte strings, comments and blanks gone."""
    with open(path, "rb") as handle:
        lines = handle.read().split(b"\n")
    return [line.strip() for line in lines if line.strip() and not line.strip().startswith(b"#")]


def drive(command_file, log_path, timeout):
    """Pump ``command_file`` through the FIFOs, logging everything, until done."""
    commands = read_commands(command_file)
    deadline = time.monotonic() + timeout
    out_fd = open_fifo(OUTPUT_FIFO, os.O_RDONLY | os.O_NONBLOCK, deadline)
    in_fd = None
    prompts = 0
    sent = 0
    try:
        in_fd = open_fifo(INPUT_FIFO, os.O_WRONLY | os.O_NONBLOCK, deadline)
        with open(log_path, "wb") as log:
            while sent < len(commands) and time.monotonic() < deadline:
                ready, _, _ = select.select([out_fd], [], [], 0.2)
                if not ready:
                    continue
                try:
                    data = os.read(out_fd, 65536)
                except BlockingIOError:
                    continue
                if not data:
                    break
                log.write(data)
                log.flush()
                prompts += len(PROMPT.findall(data))
                while sent < len(commands) and prompts > sent:
                    try:
                        os.write(in_fd, commands[sent] + b"\n")
                    except OSError:
                        break
                    sent += 1
            # Blastem answers even the final `quit` with one more prompt,
            # written as it shuts down - after this driver has nothing left
            # to send. Leaving the FIFO the moment the last command is sent
            # turns that write into a broken pipe and kills the reference
            # with signal 13 after it has already written every dump (#544):
            # run-ref.sh then aborts before its size checks, and the sweep
            # records a death that never touched a frame. Hold the read end
            # open until the reference exits and closes its end - the EOF
            # below - or a short grace passes, so the shutdown lands in the
            # transcript instead of in a broken pipe.
            drain_deadline = min(time.monotonic() + 2.0, deadline)
            while time.monotonic() < drain_deadline:
                ready, _, _ = select.select([out_fd], [], [], 0.2)
                if not ready:
                    continue
                try:
                    data = os.read(out_fd, 65536)
                except BlockingIOError:
                    continue
                if not data:
                    break
                log.write(data)
                log.flush()
    finally:
        # An exec'd driver would close these by exiting; closing them here is
        # the same thing without dying, and it is what makes the teardown
        # observable: after we let go of the read end, a write from the
        # reference side breaks the pipe, and only a driver that waited out
        # the reference's shutdown has kept its own hands clean.
        for fd in (out_fd, in_fd):
            if fd is not None:
                os.close(fd)
    if sent < len(commands):
        raise SystemExit(
            f"terminal-standin: sent {sent} of {len(commands)} commands before the "
            f"{timeout:g}s timeout; the rest never got a prompt"
        )


def main(argv):
    if len(argv) == 2 and argv[1] == "--selftest":
        return selftest()
    if len(argv) != 3:
        print(__doc__.strip().splitlines()[0], file=sys.stderr)
        return 2
    drive(argv[1], argv[2], float(os.environ.get("REF_DRIVE_TIMEOUT", "120")))
    return 0


FAKE_BLASTEM = r"""
import os, sys, time

# The teardown that kills the real reference (#544): blastem answers even the
# final `quit` with one more prompt, written as it shuts down - a moment after
# the driver has nothing left to send. This one delays that write so a driver
# that closes its end early deterministically breaks the pipe, and reports a
# broken pipe as exit 13 so the assertion can name it.
def main():
    out_fd = os.open(sys.argv[1], os.O_WRONLY)
    in_fd = os.open(sys.argv[2], os.O_RDONLY | os.O_NONBLOCK)
    buf = b""
    os.write(out_fd, b"blastem banner\n> \n")
    sent_quit = False
    started = time.monotonic()
    while not sent_quit and time.monotonic() - started < 10.0:
        try:
            data = os.read(in_fd, 4096)
        except BlockingIOError:
            time.sleep(0.01)
            continue
        if not data:
            # No writer connected yet: the driver opens its end a beat after
            # it opens ours, and an O_NONBLOCK read reports that as EOF.
            time.sleep(0.01)
            continue
        buf += data
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            if line.strip() == b"quit":
                sent_quit = True
                break
            os.write(out_fd, b"> \n")
    if sent_quit:
        time.sleep(0.4)
        os.write(out_fd, b"teardown\n> \n")
    os.close(out_fd)
    os.close(in_fd)

try:
    main()
    os._exit(0)
except BrokenPipeError:
    os._exit(13)
"""


def selftest():
    """Pin the teardown contract: the driver holds the FIFO open past `quit`.

    Plants a fake blastem that walks the same prompt-per-command protocol and
    writes its shutdown prompt late. The fake must survive that write (exit 0,
    not 13) and the transcript must hold it, which is only possible if the
    driver keeps its read end open until the reference exits.
    """
    with tempfile.TemporaryDirectory(prefix="ref-standin-selftest-") as tmp:
        out_fifo = os.path.join(tmp, "out")
        in_fifo = os.path.join(tmp, "in")
        os.mkfifo(out_fifo)
        os.mkfifo(in_fifo)
        fake_path = os.path.join(tmp, "fake-blastem.py")
        with open(fake_path, "w") as handle:
            handle.write(FAKE_BLASTEM)
        fake = subprocess.Popen([sys.executable, fake_path, out_fifo, in_fifo])
        log_path = os.path.join(tmp, "transcript.log")
        global INPUT_FIFO, OUTPUT_FIFO
        saved = INPUT_FIFO, OUTPUT_FIFO
        INPUT_FIFO, OUTPUT_FIFO = in_fifo, out_fifo
        try:
            cmds = os.path.join(tmp, "cmds")
            with open(cmds, "wb") as handle:
                handle.write(b"frames 1\nquit\n")
            drive(cmds, log_path, 15.0)
        finally:
            INPUT_FIFO, OUTPUT_FIFO = saved
            rc = fake.wait(timeout=15)
        transcript = open(log_path, "rb").read()
    problems = []
    if rc == 13:
        problems.append("the reference-side write after quit broke the pipe (exit 13)")
    elif rc != 0:
        problems.append(f"the fake reference exited {rc}, not 0")
    if b"teardown" not in transcript:
        problems.append("the teardown prompt is missing from the transcript")
    if problems:
        for problem in problems:
            print(f"terminal-standin selftest: FAIL: {problem}", file=sys.stderr)
        return 1
    print("terminal-standin selftest: ok - teardown write landed, no broken pipe")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
