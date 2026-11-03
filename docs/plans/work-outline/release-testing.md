# Release testing

Replaces the Tests notes under "How we work" in `HANDOFF.md` for non-UI crates
(the coordinator folds this in). Written 2026-11-03 on branch `faster-releases`.

## Quick start

```
cargo install cargo-nextest --locked          # once, user-level
scripts/run_release_tests.sh --build          # affected suites + installed build in parallel
scripts/run_release_tests.sh --all --build    # everything
scripts/run_release_tests.sh --dry-run        # show the selection only
scripts/run_release_tests.sh --quarantine     # also run the quarantined tests (not fatal)
```

The base ref defaults to `origin/work-outline-implementation`; pass another as the
positional argument. The script maps changed paths (committed since the base, plus
uncommitted and untracked) to crates, widens to every crate that depends on them
(`cargo metadata`), and runs only those. A change to `Cargo.toml`, `Cargo.lock`,
`rust-toolchain.toml` or `.config/` selects everything; a change to docs or scripts
selects nothing. `--build` builds `-p mast --profile installed` into
`target/release-build` (its own target directory, so it never contends with the test
build); the binary is `target/release-build/installed/zeron`. Install from there.

## Runners

- **Non-UI crates (engine, mcp, harness, proto, ...): cargo-nextest**, config in
  `.config/nextest.toml`. Each test is its own process, test binaries run in parallel
  (8 threads), and a test running 3 minutes (`slow-timeout` 60 s x 3) is terminated and
  reported as TIMEOUT instead of hanging the run.
- **mast-ui stays on `cargo test -p mast-ui --lib -- --test-threads=1`.** gpui tests
  rely on a single thread inside one process (shared platform/global state). nextest
  would give every test its own process, and a `max-threads = 1` group would still pay
  per-test process start-up for several thousand tests with no benefit. Use
  `perl -e 'alarm N; exec @ARGV' ...` for a time limit.

## Profiles

| Profile | Use | Behaviour |
| --- | --- | --- |
| `default` | local development | everything runs |
| `release` | release gate | quarantined tests excluded; any failure is new |
| `quarantine` | tracking the known-bad set | only quarantined tests, 2 threads; failures expected |

`cargo nextest run --profile release -p mast-engine` (add `-p` per crate, or
`--workspace --exclude mast-ui --exclude mast`).

### Quarantine

The list is the "Known test failures" section of `HANDOFF.md`, written as `test(~name)`
filters in `profile.release.default-filter` (exclude) and
`profile.quarantine.default-filter` (include). They are duplicated because nextest has no
named filter sets; edit both. When a quarantined test is fixed, delete it from both.
When a new test fails in the `release` profile, it is a regression (or a new flake:
re-run it alone with `cargo nextest run -p <crate> <name>`; if it passes alone under
load only, add it to the quarantine and to HANDOFF).

### Terminal-heavy tests

`managed_accounts`, `m5_repos_diffs_terminals` and `pr_ticket_cache_login_shell` open
PTYs and login shells. At 8 concurrent tests, every `m5_repos_diffs_terminals` test hit
the 3 minute limit and three `managed_accounts` tests failed. They are in the
`terminal-heavy` test group (max 2 at a time); with that, both binaries pass (38 tests,
69 s).

## Process rules

- Only kill processes inside your own worktree. Never `pkill -f target/debug/deps`.
- Do not run long runs under `nice` (it cannot be raised again without root).
- At most two children run full suites at once.
- Seed a worktree cache with `cp -c -R` and wait for it to finish before running cargo.
  A cargo build started while the copy is still running leaves mixed artifacts and fails
  with "found crates with colliding StableCrateId values"; `rm -rf target/debug` and
  re-seed. A long-running command can be detached with `nohup ... &` plus `disown`
  (macOS has no `setsid`); background tool commands may be stopped after about 20 minutes.

## sccache: not adopted

- `installed` sets `incremental = true`, and sccache cannot cache incremental
  compilations. The workspace crates (the ones that change) would never hit; only
  third-party dependencies would.
- Third-party dependencies are already shared: seeding a worktree with APFS clones of
  `target/debug/{deps,build,.fingerprint}` costs seconds of disk time and no disk
  space, and registry crates are not rebuilt. The remaining cost is workspace-crate
  compilation, which sccache does not address.
- Workspace paths differ per worktree, so keys for path dependencies and anything that
  embeds absolute paths would also miss unless `--remap-path-prefix` is configured
  everywhere, which changes the backtraces in the installed binary.
- Adding a compiler wrapper to every build is a risk to the release path for no
  measured gain. Revisit only if clean builds of new dependency versions become a
  recurring cost.
