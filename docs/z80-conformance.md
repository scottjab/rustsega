# z80 conformance against the Z80 SingleStepTests

There are two gates, both in `crates/z80/tests/singlestep.rs`, both walking the
whole pinned [SingleStepTests/z80] suite: `all_opcodes` runs every case of every
file, and [`sst_sample`](#the-fast-gate-inside-just-check) runs the first twenty
cases of every file inside `just check`. This page is their record — the pin, the
measurement, how each one decides pass or fail, and the corners of the
architecture the suite cannot reach.

[SingleStepTests/z80]: https://github.com/SingleStepTests/z80

## The run

| | |
|---|---|
| Measured | 2026-11-10, `main` at `5747374` plus `issue-60-z80-conformance-gate` |
| Opcode files | **1604**, all passing |
| Cases | **1 604 000 / 1 604 000** (1000 per file) |
| Failing files | **0** |
| Gate time | `all_opcodes` 6.8 s of test time (11.3 s wall); `sst_sample` 5.4 s (8.4 s wall) |

A file stops at its first failing case, so the case count is *checked* cases;
with nothing failing the two numbers are the same, and when something does fail
the count is a lower bound rather than a lie.

Parsing, not execution, is most of the 6.8 s — which is why the sample costs
almost as much as the full sweep and why its slice is 20 cases rather than 2.

## The data

Pinned twice by `scripts/fetch-test-data.sh` — the URL names an exact commit and
the archive's SHA-256 is checked before anything is unpacked — so this page and
CI measure byte-identical files.

| | |
|---|---|
| Repo | [SingleStepTests/z80] |
| Commit | `ebe1875d48f374bcfd4b505d8eb8ee751568b5f7` |
| Tarball SHA-256 | `f44683f0edf1c1c37f98861ccd1bfda3b9bce65b5b258d4817a0f1d2012076a7` |
| Lands in | `test-roms/z80/v1/` |
| Files | 1604, all of them opcode files |
| Fetch | `nix develop -c just fetch` |

`every_opcode_file_in_the_suite_is_enumerated` in
`crates/z80/tests/singlestep.rs` asserts the 1604, so a pin bump that renames or
drops a group fails loudly instead of shrinking the gate. The 1604 split like
this:

| family | files | what is missing, and why |
|---|---|---|
| unprefixed | 252 | `cb`, `dd`, `ed`, `fd` — a prefix on its own is not an instruction |
| `cb NN` | 256 | |
| `ed NN` | 80 | the undefined half of `ED`; those are 8 T-state NOPs, not files |
| `dd NN`, `fd NN` | 252 + 252 | `dd cb` and `fd cb`, which are the families below |
| `dd cb __ NN`, `fd cb __ NN` | 256 + 256 | the indexed bit-shift space, `d` and `op` |
| chained prefixes | 0 | `dd fd 21` and friends are not in the suite at all |

That last row is why chained prefixes are pinned by unit tests in
`crates/z80/src/decode.rs` rather than by the hardware suite.

## Running the gate

```sh
nix develop -c cargo nextest run -p z80 --run-ignored all --no-capture all_opcodes
```

- `just check` does **not** run it — it runs [`sst_sample`](#the-fast-gate-inside-just-check)
  instead, which costs 5.4 s of the sweep's 6.8 s and needs no extra flag.
- `just test-all` runs it, and CI runs it as its own step on every push and pull
  request — see [In CI](#in-ci).
- `--no-capture` is what puts the per-file table in the log at all: nextest hides
  the output of a test that passes. [#186] is about doing that for every passing
  test of `just test-all`.
- With `test-roms/` absent the sweep prints `SKIPPED: missing test data: Z80
  SingleStepTests (z80/v1)` and passes, like every other data-driven test here.

[#186]: https://github.com/scottjab/rustsega/issues/186

### How it gates

`harness::sweep(case_limit)` returns the names of the files with a failing case
and asserts nothing itself; the two callers are the gate:

```rust
let failed = harness::sweep(None);
assert!(failed.is_empty(), ...);
```

There is **no allowance list**. Every file passes, so a file that fails fails the
run, with its name in the assertion message. Should a future ticket need one,
`docs/m68k-conformance.md` documents the two-way list the m68k gate uses — a
file that stops failing has to come off it, which is what keeps such a list from
going stale.

## What one case checks

`run_case` in `crates/z80/tests/harness/mod.rs` compares, per case:

- `a`/`f`, `b`/`c`, `d`/`e`, `h`/`l` and all three shadow pairs, `ix`, `iy`,
  `sp`, `pc`, `i`, `r`;
- the interrupt state: `im`, `iff1`, `iff2`, and `ei` — the one-instruction EI
  latch;
- the undocumented `p` parity latch, the `q` flag latch, and `wz` (MEMPTR);
- every RAM byte the case lists in `final.ram`;
- the whole port transcript in `ports`, reads and writes in order;
- the T-state count `Z80::step` returned, against `cycles`.

So a wrong cycle count fails a file even when every register is right, which is
what makes this suite a timing gate as well as a state gate — the point of
M2-17 (#60).

## The fast gate inside `just check`

`sst_sample` is the same sweep over `SAMPLE_CASES = 20` cases per file, not
ignored, so it runs on every `just check` and every CI `just check`. The size of
the slice comes from how the suite fails: an unimplemented or wrong opcode fails
its file the same way from the first case on, so 20 cases of every file catch
what a whole file would catch, at a third of the sweep's cost.

What it cannot see is a case past the twentieth. That is the residual hole
between `just check` and `all_opcodes`, and it is deliberate: 1604 file parses
are the cost of coverage, and coverage of *every* opcode matters more on the
first cases than on the last.

## What this ticket settled

The 316 files that were failing when #60 started were the last two corners of the
architecture, and both turned out to be instructions rather than timing:

### The rest of the `DD`/`FD` space

Every `DD`/`FD` opcode that does not name `H`, `L` or `(HL)` — 157 of the 252 in
each of the two index spaces — is a **rotted prefix**: 4 T-states of nothing,
then the byte after it executed unprefixed. `crates/z80/src/decode.rs`'s
`Z80::prefix_rot` is that one rule, and it replaced six special cases plus a
panic.

The part the suite is strict about is not the timing but the latches: the prefix
is an instruction of its own, so it clears `q`, `p` and the EI latch before the
opcode behind it runs. `SCF` and `CCF` prove it — they take `X`/`Y` from
`((q ^ F) | A) & 0x28`, and all 1000 cases each of `dd 37`, `fd 37`, `dd 3f` and
`fd 3f` want `(F | A) & 0x28`, which is what that formula gives with `q`
cleared. `ei` agrees: of every prefixed `dd`/`fd` file, only `dd fb` and `fd fb`
(`EI`, which sets the latch again) end with it set.

### `ED 77` and `ED 7F`

`LD (MEMPTR),A`: 8 T-states, and `wz` left where it was — all 2000 cases of the
two files start and end on the same `wz`, because the value goes to the address
`wz` holds rather than into it. `Z80::ld_memptr_a`.

## What the suite cannot see

Recorded here because "the gate is green" is only useful with its blind spots.

- **Where `ED 77`/`ED 7F` write.** `final.ram` lists only the two opcode bytes in
  all 2000 cases, and `compare_ram` checks a case only at the addresses it
  lists, so the destination of the store is unverified either way. The suite does
  pin the timing, the registers and the unmoved `wz`.
- **`ED 7F` as a distinct instruction.** It is documented as the 7-bit form of
  `ED 77`, which is a distinction this core has nothing to draw — its `wz` is a
  plain 16-bit pointer — and no case in either file separates them. Both are the
  full-pointer store.
- **Stray writes.** Anything the core writes to an address a case never listed
  goes unnoticed; `compare_ram`'s doc comment calls this out as a known hole.
  Timing and register state normally catch the same bug, which is why it stays a
  hole.
- **Chained prefixes.** No file in the suite covers `DD FD`, `FD DD`, `DD ED` or
  `DD DD`; their "each prefix costs a NOP, the last one decides" rule is pinned by
  `chained_prefixes_each_cost_a_nop_and_the_last_wins` in `decode.rs`.
- **The Mega Drive's Z80 bus.** Wait states, bus requests held by the 68000 and
  the memory-acknowledge behaviour around them are not part of a single-CPU
  instruction suite, and are M3's (#60 depends on nothing here).
- **The twentieth case.** Between `just check` and `main`, `sst_sample` sees 32
  080 of the 1 604 000 cases. `all_opcodes` sees the rest.

## In CI

| step | when | what runs |
|---|---|---|
| `just check` | every push and pull request | `sst_sample` (20 cases × 1604 files) |
| z80 conformance gate | every push and pull request | `all_opcodes` (all 1 604 000 cases) |
| `just test-all` | pushes to `main` only | `all_opcodes` again, with the other heavy suites |

The gate is its own step on a pull request, not just part of the `main` sweep,
for the same reason the 3.7 s m68k gate is
([`m68k-conformance.md`](m68k-conformance.md)): 6.8 s of test time is cheap next
to a conformance regression that would otherwise land first. `just check` alone
would cover every opcode with `sst_sample`, but only its first twenty cases —
which is exactly the part of a file where a wrong opcode does not show up but an
undocumented flag or an edge-case cycle count does.
