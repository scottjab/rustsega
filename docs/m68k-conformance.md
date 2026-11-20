# m68k conformance against the 68000 SingleStepTests

The gate is `all_instructions` in `crates/m68k/tests/singlestep.rs`: every
instruction file in the pinned [SingleStepTests/680x0] suite, in one run, at the
strictest `Check` level the core can currently be held to. This page is its
record — the pin, the measurement, and every failure with the issue that carries
it.

[SingleStepTests/680x0]: https://github.com/SingleStepTests/680x0

## The run

| | |
|---|---|
| Measured | 2026-11-19, `main` at the #241 fix |
| Check level | `harness::Check::StateAndCyclesSkipExceptions` |
| Instruction files | **124** — 123 clean, 1 failing |
| Cases | **821 971 / 1 000 060** passed |
| Cases skipped | **178 087**, spread over 62 files |
| Cases failing | **2**, in 1 file |
| Sweep time | 3.7 s of test time; 4.3 s wall including `nix develop` startup with the test binary already built |

`Check::StateAndCyclesSkipExceptions` compares every data and address
register, both stored stack pointers, SR, PC, both prefetch words, every RAM
byte the case lists, **and** the clock count `step` returned against the case's
`length`. It leaves out the cases the suite expects to end in its own exception
handler, which are counted as *skipped*, not passed — see
[What the gate leaves out](#what-the-gate-leaves-out).

Skipped is a real hole, not a rounding error: 17.8 % of the suite. The
per-file `passed` column in the table below has to be read against that file's
`skipped` column, never on its own.

## The data

Pinned twice by `scripts/fetch-test-data.sh` — the URL names an exact commit and
the archive's SHA-256 is checked before anything is unpacked — so this table and
CI's are measured on byte-identical bytes.

| | |
|---|---|
| Repo | [SingleStepTests/680x0] |
| Commit | `e0d5ece9670205cc84a0101081837deb446f86a3` |
| Tarball SHA-256 | `f90dab5ba7519c6d9e20143be12e433403b6db6586f6bae3e5d3bf0123f01b50` |
| Lands in | `test-roms/680x0/68000/v1/` |
| Instruction files | 124 (the directory holds 125 entries; the extra one is `README.md`) |
| Fetch | `nix develop -c just fetch` |

`every_instruction_file_in_the_suite_is_enumerated` in
`crates/m68k/tests/singlestep.rs` asserts the 124, so a pin bump that renames or
adds a file fails loudly instead of silently shrinking the gate.

## Running the gate

```sh
nix develop -c cargo nextest run -p m68k --run-ignored all --no-capture all_instructions
```

- `just check` does **not** run it: the test is `#[ignore]`d so an ordinary
  commit does not have to inflate 195 MB of suite, and `just check` has to stay
  green with `test-roms/` absent.
- `just test-all` runs it, and CI runs `just test-all` on pushes to `main`.
- CI also runs the command above as its own step on every push and every PR —
  see [In CI](#in-ci).
- Nextest hides the output of a *passing* test, so `--no-capture` is what puts
  the table in the log at all here. `just test-all` does it for every passing
  test via `--success-output final` (#186): the sweep's counts reach the CI log
  from that step.

[#186]: https://github.com/scottjab/rustsega/issues/186

### How it gates

The sweep fails unless the set of failing files is exactly `KNOWN_FAILURES` in
`crates/m68k/tests/singlestep.rs`, checked **both** ways:

- a file that fails and is not in `KNOWN_FAILURES` fails the run — a new
  conformance regression cannot land quietly;
- a file in `KNOWN_FAILURES` that no longer fails fails the run too — the list
  cannot go stale, and a file that has vanished from the suite counts as
  "no longer fails", so a pin bump that drops a file is caught here as well.

`KNOWN_FAILURES` and the [known failures](#known-failures-2-files-4-950-cases)
section below are the same list, and they move together.

## Known failures (1 file, 2 cases)

This one is a measured failure, not an excused one. The other known failure,
`CHK`'s 4 948 clock mismatches, left this list in #241: its clock law turned out
to be Motorola's own trap timing (yacht.txt prices `trap : Dn > Src` at
`38(5/3) + ea` and `trap : Dn < 0` at `40(5/3) + ea`, no-trap at `10(1/0) + ea`),
the core now models it, and the file moved up to cycle checking in the same
commit that removed it from `KNOWN_FAILURES`.

### `ASL.b` — 2 cases, state — [#209]

| column | value |
|---|---|
| `total` / `passed` / `failed` / `skipped` | 8065 / 8063 / 2 / 0 |
| Axis that fails | state (`d2`, `sr`) — clocks match |

Two cases ask a byte shift to modify the upper three bytes of `D2`:

```text
case "e502 [ASL.b Q, D2] 1583": d2: expected 0x2E5E4304, got 0xCDFB7FF8; sr: expected 0x2713, got 0x270A
case "e502 [ASL.b Q, D2] 1761": d2: expected 0x6461D390, got 0x417C7EF4
```

The suite data is wrong and no emulator behaviour can produce it. Both are
`$E502` = `ASL.B #2,D2`, and both run exactly one instruction — `pc` goes `$C00`
→ `$C02`, and only `d2`, `sr`, `pc` and the prefetch queue differ — so the
destination is `D2`'s low byte and the three bytes above it have to come out as
they went in:

| case | `initial.d2` | `final.d2` recorded | `ASL.B #2,D2` gives |
|---|---|---|---|
| `e502 [ASL.b Q, D2] 1583` | `$CDFB7FBE` | `$2E5E4304` | `$CDFB7FF8` |
| `e502 [ASL.b Q, D2] 1761` | `$417C7E7D` | `$6461D390` | `$417C7EF4` |

Each recorded final *is* a correct `ASL.B #2` — data and all five condition
codes — of some other register: `$2E5E4341` gives `$2E5E4304` with
`N=0 Z=0 V=1 C=X=1`, which is the recorded `sr $2713`, and `$6461D364` gives
`$6461D390` with `N=1 Z=0 V=1 C=X=1`, which is the recorded `sr $271B`. The two
snapshots are individually plausible and mispaired, so the `initial` is simply
not a preimage of its own `final`. The file shows it can do these two correctly:
`e502 [ASL.b Q, D2] 1341` and `… 1722` execute `ASL.B #2,D2` on the same two
`initial.d2` values and record the results above. And of the 4 070 cases of this
file whose destination is a data register, exactly these 2 ask for bits above the
instruction's own width, so it is two cross-wired pairs rather than a systematic
difference in some mode.

`shifts.rs` [`ASL_B_BROKEN_CASES`] drops the same two cases and documents the
above at length. Where the hole sits is the decided shape of [#209]:

- **Not at fetch.** `scripts/fetch-test-data.sh` pins this suite by the archive's
  SHA-256, so rewriting a file on unpack would put bytes on disk that the pin
  does not attest — and would delete the one signal that tells us upstream fixed
  the data.
- **Not in the gate.** `all_instructions` still runs all 8 065 cases, so the two
  stay counted as failures and `ASL.b` still reads 8063/8065 above; only the
  per-file `shifts.rs` test drops them, and it asserts both names are still in
  the file, so a pin bump that repairs them fails that test instead of quietly
  restoring coverage.

We are not the first to notice it. The suite's own repository carries
[SingleStepTests/680x0#4], opened by `howprice` on 2024-08-11, which names these
same two cases and predicts `$CDFB7FF8` with `sr $270A` and `$417C7EF4` — exactly
what this core computes — and it is still open with no reply. That issue is the
one to watch. We post nothing there: the decision on [#209] is to work around the
data rather than lobby for a fix, which is why the drop list is the whole of the
remedy.

Expect the workaround to be long-lived. That repository has one commit, `e0d5ece`
"Import initial tests." of 2024-05-14, nothing has been pushed since, and
`e0d5ece` is what [the pin](#the-data) points at — so there is no upstream change
to bump to, and `ASL.b` should be read as 8063/8065 for as long as the pin stands.

[`ASL_B_BROKEN_CASES`]: ../crates/m68k/tests/shifts.rs
[#209]: https://github.com/scottjab/rustsega/issues/209
[SingleStepTests/680x0#4]: https://github.com/SingleStepTests/680x0/issues/4

## What the gate leaves out

Things this run cannot see, with the issue that owns each:

- **The 178 087 skipped cases (62 files).** These are the cases that end in the
  suite's exception handler at $00000C. `is_exception_case` skips them because
  the core could not take them when the harness was written; the exception path
  has since been built out (#37, #41), and enabling those cases is [#223].
- **"Skipped" is not "address error"** ([#185], closed by the rename). The suite
  records its address error at $00000C, not at hardware's $000008, because the
  core it was recorded against routes the address error and the illegal
  instruction to the same slot — `EXC_ADDRESS_ERROR == EXC_ILLEGAL == 3`, see
  `crates/m68k/src/address_error.rs`. So the $0C slot the predicate reads is the
  right one, but it cannot name which exception fired: the skipped set includes
  genuine illegal-instruction cases and is not a set of hardware address errors.
  A case ending at a *different* vector is not skipped either — `CHK`'s 4 272
  traps end at $00002000 and are run, and failed on, by the gate.
- **The privilege-violation vector in user mode** ([#194]): the one measured
  vector the suite cannot pin, so nothing here asserts it.
- **Bus transactions.** The suite's `transactions` array is deliberately not
  parsed (M1-03); comparing it is out of scope and is what keeps a 1.4 GB suite
  readable.
- **RAM the case does not list.** Only the addresses in a case's `final.ram` are
  compared, so a stray write elsewhere is invisible. See the harness's
  `compare_ram` doc comment.

[#185]: https://github.com/scottjab/rustsega/issues/185
[#194]: https://github.com/scottjab/rustsega/issues/194
[#223]: https://github.com/scottjab/rustsega/issues/223

## In CI

`.github/workflows/ci.yml` runs the sweep as its own step on every push and
every pull request, at 3.7 s of test time (about 5 s with nix and cargo
overhead once `just check` has built the test binaries) — comfortably inside the
ticket's 2-minute budget, so the gate protects a PR before merge instead of
only reporting on `main` afterwards.

The step passes `--no-capture`, so the table above is what a CI log shows for a
green run and the failing cases' first ten lines per file is what it shows for a
red one. `just test-all` on `main` runs the sweep again; `--success-output
final` (#186) carries its counts into that log too.

## The table

Verbatim output of the gate, so a re-run diffs against this. Columns are
`total`, `passed`, `failed`, `skipped`; rows are sorted by file name, which is
what `harness::all_files` returns.

```text
ABCD                8065    8065       0       0
ADD.b               8065    8065       0       0
ADD.l               8065    5143       0    2922
ADD.w               8065    5117       0    2948
ADDA.l              8065    5372       0    2693
ADDA.w              8065    5431       0    2634
ADDX.b              8065    8065       0       0
ADDX.l              8065    5472       0    2593
ADDX.w              8065    5465       0    2600
AND.b               8065    8065       0       0
AND.l               8065    4745       0    3320
AND.w               8065    4668       0    3397
ANDItoCCR           8065    8065       0       0
ANDItoSR            8065    8065       0       0
ASL.b               8065    8063       2       0
ASL.l               8065    8065       0       0
ASL.w               8065    7127       0     938
ASR.b               8065    8065       0       0
ASR.l               8065    8065       0       0
ASR.w               8065    7142       0     923
BCHG                8065    8065       0       0
BCLR                8065    8065       0       0
BSET                8065    8065       0       0
BSR                 8065    4085       0    3980
BTST                8065    8065       0       0
Bcc                 8065    5865       0    2200
CHK                 8065    4948       0    3117
CLR.b               8065    8065       0       0
CLR.l               8065    4903       0    3162
CLR.w               8065    4896       0    3169
CMP.b               8065    8065       0       0
CMP.l               8065    5034       0    3031
CMP.w               8065    5024       0    3041
CMPA.l              8065    5450       0    2615
CMPA.w              8065    5360       0    2705
DBcc                8065    6101       0    1964
DIVS                8065    4992       0    3073
DIVU                8065    4964       0    3101
EOR.b               8065    8065       0       0
EOR.l               8065    4904       0    3161
EOR.w               8065    4930       0    3135
EORItoCCR           8065    8065       0       0
EORItoSR            8065    8065       0       0
EXG                 8065    8065       0       0
EXT.l               8065    8065       0       0
EXT.w               8065    8065       0       0
JMP                 8065    4259       0    3806
JSR                 8065    4183       0    3882
LEA                 8065    8065       0       0
LINK                8065    8065       0       0
LSL.b               8065    8065       0       0
LSL.l               8065    8065       0       0
LSL.w               8065    7161       0     904
LSR.b               8065    8065       0       0
LSR.l               8065    8065       0       0
LSR.w               8065    7103       0     962
MOVE.b              8065    8065       0       0
MOVE.l              8065    3270       0    4795
MOVE.q              8065    8065       0       0
MOVE.w              8065    3368       0    4697
MOVEA.l             8065    5379       0    2686
MOVEA.w             8065    5322       0    2743
MOVEM.l             8065    4223       0    3842
MOVEM.w             8065    4281       0    3784
MOVEP.l             8065    8065       0       0
MOVEP.w             8065    8065       0       0
MOVEfromSR          8065    5017       0    3048
MOVEfromUSP         8065    8065       0       0
MOVEtoCCR           8065    4958       0    3107
MOVEtoSR            8065    4938       0    3127
MOVEtoUSP           8065    8065       0       0
MULS                8065    4943       0    3122
MULU                8065    4992       0    3073
NBCD                8065    8065       0       0
NEG.b               8065    8065       0       0
NEG.l               8065    5031       0    3034
NEG.w               8065    4911       0    3154
NEGX.b              8065    8065       0       0
NEGX.l              8065    4919       0    3146
NEGX.w              8065    4979       0    3086
NOP                 8065    8065       0       0
NOT.b               8065    8065       0       0
NOT.l               8065    4925       0    3140
NOT.w               8065    4957       0    3108
OR.b                8065    8065       0       0
OR.l                8065    4730       0    3335
OR.w                8065    4650       0    3415
ORItoCCR            8065    8065       0       0
ORItoSR             8065    8065       0       0
PEA                 8065    8065       0       0
RESET               8065    8065       0       0
ROL.b               8065    8065       0       0
ROL.l               8065    8065       0       0
ROL.w               8065    7074       0     991
ROR.b               8065    8065       0       0
ROR.l               8065    8065       0       0
ROR.w               8065    7187       0     878
ROXL.b              8065    8065       0       0
ROXL.l              8065    8065       0       0
ROXL.w              8065    7128       0     937
ROXR.b              8065    8065       0       0
ROXR.l              8065    8065       0       0
ROXR.w              8065    7172       0     893
RTE                 8065    4011       0    4054
RTR                 8065    4038       0    4027
RTS                 8065    4008       0    4057
SBCD                8065    8065       0       0
SUB.b               8065    8065       0       0
SUB.l               8065    5093       0    2972
SUB.w               8065    5121       0    2944
SUBA.l              8065    5329       0    2736
SUBA.w              8065    5386       0    2679
SUBX.b              8065    8065       0       0
SUBX.l              8065    5481       0    2584
SUBX.w              8065    5387       0    2678
SWAP                8065    8065       0       0
Scc                 8065    8065       0       0
TAS                 8065    8065       0       0
TRAP                8065    8065       0       0
TRAPV               8065    8065       0       0
TST.b               8065    8065       0       0
TST.l               8065    4963       0    3102
TST.w               8065    4928       0    3137
UNLINK              8065    8065       0       0

ASL.b (#209): 2 failing cases
  case "e502 [ASL.b Q, D2] 1583": d2: expected 0x2E5E4304, got 0xCDFB7FF8; sr: expected 0x2713, got 0x270A
  case "e502 [ASL.b Q, D2] 1761": d2: expected 0x6461D390, got 0x417C7EF4

124 files: 821971/1000060 cases passed, 178087 cases skipped as exceptions, 2 cases failing in 1 files
sweep took 3.7s
```

## Regenerating this page

Run the gate, replace the block above with its table, and update the four
numbers in [The run](#the-run). `ASL.b`'s row in
[Known failures](#known-failures-1-file-2-cases) comes from the same run.
The gate refuses to pass while `KNOWN_FAILURES` and this page disagree about
*which* files fail, so the only thing a re-run should ever change here is the
numbers — and when a file does move, it moves out of this page and out of
`KNOWN_FAILURES` in the same commit, with the issue that closed it linked.

If the pin moves, re-check the commit and tarball hash against
`scripts/fetch-test-data.sh` (the only place they are written down) and say so
in the commit message; `docs/TESTING.md` explains how a pin gets bumped.
