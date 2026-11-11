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

Claim it right away so other agents skip it. Several agents may share one
GitHub account, so sign your claim with your own agent ID. Use `$AGENT_ID` if
it's set. Otherwise make one at the start of your session and reuse the same
value for the whole session:

```sh
echo "${AGENT_ID:-agent-$(head -c4 /dev/urandom | od -An -tx1 | tr -d ' \n')}"
```

```sh
gh issue edit <N> --add-label in-progress
gh issue comment <N> --body "Claimed by <your-agent-id>"
```

Then check that you actually won the claim. Two agents can grab the same
ticket at the same moment, and the earliest claim comment wins:

```sh
gh issue view <N> --json comments \
  --jq '[.comments[] | select(.body | startswith("Claimed by "))][0].body'
```

- If it prints `Claimed by <your-agent-id>`, the ticket is yours. Carry on.
- If it prints a different ID, you lost. Don't remove the label (the winner
  needs it). Go back to the start of step 2 and run `next-ticket.sh` again.

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

Other agents are merging at the same time, so rebase on the latest main right
before you push, and re-run the full check after the rebase:

```sh
git fetch origin
git rebase origin/main        # fix conflicts if any (Cargo.toml and mod lists are common)
nix develop -c just check     # must pass AFTER the rebase, not just before
git push -u origin HEAD --force-with-lease
gh pr create --fill --body "Closes #<N>

<what you did, what tests you added, anything surprising>"
```

Then wait for CI. Every push to `main` and every PR runs the `CI` workflow,
which is `just check` on a runner. There is no "this repo has no CI yet" state.

The gate is a reported `pass`. `pending`, `queued`, `no checks reported` and an
empty check list are not passes and none of them authorise a merge: a run that
has not registered yet and a run still waiting for a runner both read as
nothing-to-do, and a local `just check` after the rebase is not a substitute for
seeing the CI run go green.

Don't read CI with `gh pr checks --watch`. It prints `no checks reported` for
the first few seconds of every PR, before the workflow has registered, and its
coloured output breaks parsing. Read the run itself instead:

```sh
sha=$(gh pr view <N> --json headRefOid -q .headRefOid)
gh run list --commit "$sha" --json status,conclusion
```

Merge only when the newest `CI` run on your PR head SHA reports
`status: completed` and `conclusion: success`. To block on a run, bound the
wait, because a queued run can outlast your whole session:

```sh
timeout 300 gh run watch <run-id> --exit-status
```

If CI is still `queued` or `pending` when your time budget runs out, stop.
Report "PR <N> up, CI pending" and let whoever is watching it merge. One bounded
look, no retry loop.

If a run fails, fix it on the same branch and push. Do not merge red.

Merge by PR number. Another agent may be working in this same checkout, so do
not `git checkout` or `git pull` here just to merge; `gh pr merge` takes the
number and does not need your branch checked out.

```sh
gh pr merge --squash --delete-branch <N>
```

If the merge fails because main moved (conflict or "not up to date"), go back
to the rebase step above, then try the merge again.

Then update the tracking issue. Run the `git` half only when this checkout is
yours alone:

```sh
git checkout main && git pull
nix develop -c scripts/tick-tracking.sh
```

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
