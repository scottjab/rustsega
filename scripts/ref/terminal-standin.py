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
import sys
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
    in_fd = open_fifo(INPUT_FIFO, os.O_WRONLY | os.O_NONBLOCK, deadline)
    prompts = 0
    sent = 0
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
    if sent < len(commands):
        raise SystemExit(
            f"terminal-standin: sent {sent} of {len(commands)} commands before the "
            f"{timeout:g}s timeout; the rest never got a prompt"
        )


def main(argv):
    if len(argv) != 3:
        print(__doc__.strip().splitlines()[0], file=sys.stderr)
        return 2
    drive(argv[1], argv[2], float(os.environ.get("REF_DRIVE_TIMEOUT", "120")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
