# Instructions for coding agents

You are working on rustsega, a Sega Genesis emulator in Rust that runs in the
browser via WASM and ships as a single binary. The work is split into small
GitHub issues. Do exactly one issue at a time.

Read this whole file before starting. Then read `docs/ARCHITECTURE.md` and
`docs/TESTING.md`. Do not read anything else until your ticket tells you to.

## 1. Environment: always use nix

This is a NixOS machine. Do not install anything with rustup, cargo install,
pip, npm, or apt. Everything you need is in the flake dev shell.

```sh
nix develop -c <command>        # run one command in the dev shell
nix develop -c just check       # the full check: fmt, clippy, tests, wasm build
```

If you need a tool that is missing, add it to `packages` in `flake.nix` in its
own commit, and say so in the PR.

`wasm-bindgen` (crate) must stay pinned to the exact version of
`wasm-bindgen --version` in the dev shell. Never bump one without the other.

## 2. Find your ticket

```sh
nix develop -c scripts/next-ticket.sh
```

This reads the tracking issue (label `tracking`) and prints the first open
ticket whose dependencies are all closed and that nobody has claimed. If it
prints nothing ready, stop and report that.

Claim it right away so other agents skip it:

```sh
gh issue edit <N> --add-label in-progress
gh issue comment <N> --body "Claimed. Starting work."
```

## 3. Do the work (TDD, always)

```sh
git checkout main && git pull
git checkout -b issue-<N>-<short-slug>
```

1. Read the ticket fully. Read only the files it lists, plus whatever you
   need to find with `rg` (ripgrep) or `grep`.
2. Write the tests from "Tests to write" first. Run them and confirm they
   fail (`nix develop -c cargo nextest run -p <crate> <test_name>`).
3. Implement until they pass.
4. Run `nix develop -c just check`. Fix everything it reports.
5. Commit with a message like `m68k: implement ADDQ/SUBQ (#<N>)`.

Rules:

- Stay inside the ticket's scope. If you notice other problems, open a new
  issue (`gh issue create`) instead of fixing them.
- Never delete, weaken, or `#[ignore]` an existing test to get green. If an
  older test looks wrong, stop and comment on the ticket explaining why.
- Never regenerate a golden hash/snapshot unless the ticket says to.
- Never commit ROMs or downloaded test data. They live in `test-roms/`
  (gitignored) and `$RUSTSEGA_ROM_DIR`.
- No `unsafe`. No new dependencies in core crates unless the ticket allows it.
- Keep functions small and code readable. Add doc comments that cite the
  hardware doc you used (manual name + section) for anything non-obvious.

## 4. Ship it

```sh
git push -u origin HEAD
gh pr create --fill --body "Closes #<N>

<what you did, what tests you added, anything surprising>"
gh pr checks --watch          # wait for CI
gh pr merge --squash --delete-branch
git checkout main && git pull
nix develop -c scripts/tick-tracking.sh
```

If CI fails, fix it on the same branch. Do not merge red.

## 5. When you get stuck

- Ticket too big for one session: commit what works (tests green), push the
  branch, comment a summary of what's left, then
  `gh issue edit <N> --remove-label in-progress --add-label needs-split`.
- Blocked by a missing dependency or unclear spec: comment the exact question,
  then `gh issue edit <N> --remove-label in-progress --add-label blocked`.
- Then go back to step 2 and pick the next ticket.

## 6. Saving context (you have ~128k tokens)

- Never `cat` big files. Use `rg -n`, `sed -n 'START,ENDp' file`, `head`.
- Never print test data files (SingleStepTests JSON is huge, ROMs are binary).
- Run single tests, not the whole suite, while iterating. Run `just check`
  once at the end.
- Pipe noisy commands through `tail -n 40`.
- If the conversation is getting long, commit, push, and write a progress
  comment on the issue so the next session can continue.

## 7. Test data

- `$RUSTSEGA_TEST_ROM_DIR` (default `./test-roms`): public test suites and
  homebrew test ROMs, fetched by `scripts/fetch-test-data.sh`.
- `$RUSTSEGA_ROM_DIR` (default `~/games/roms/genesis`): commercial ROMs, local
  only. Files use the `.md` extension but they are binary ROMs. Never commit
  them, never print them.
- When a ticket says "find a test ROM", search the web, prefer ROMs with a
  clear license that report pass/fail on screen, and record them in
  `docs/test-roms.md` with URL, SHA-256, and license.
