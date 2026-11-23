# Developer tooling

## Install performance gate

`scripts/run_release_tests.sh` records release and budget test results. Run a real-data comparison against the installed binary with:

```sh
scripts/perf-soak.sh <candidate> --baseline <installed> --real
```

The soak uses a read-only backup of local data in a sandbox and records its result. `scripts/perf-gate.py check --binary <candidate>` checks the stamps in `target/perf-gate/`. `scripts/install-macos-app.sh <candidate> <app-path> <bundle-id>` refuses installation without passing release and budget tests and a passing real-data baseline soak for that exact binary. A qualifying ancestor stamp is accepted when intervening changes are limited to docs or crate tests. The install script supports `--skip-perf-gate "<reason>"`, which records an override.

## Headless UI driver

`scripts/ui-drive` runs a test-only driver in a seeded scratch sandbox. `start` begins a warm session; `tree`, `click`, `type`, and `key` inspect and operate it; `stop` ends it. Locators include `#selector`, `role[name="Name"]`, and `text="..."`. `start --render` enables `png`. `scripts/ui-drive run <file.yaml>` runs a scenario in a separate sandbox; `--render` includes a PNG on failure and `--out <directory>` selects the output directory. See `crates/ui-driver/scenarios/` for examples. The driver builds test binaries and is not part of the installed app.

## UI preview

`scripts/ui-preview.sh` renders offscreen previews. Use comma-separated selections with `--themes`, `--screens`, `--styles`, and `--layouts`, for example:

```sh
scripts/ui-preview.sh --screens chat,details --styles default --layouts minimal
```

`MAST_PREVIEW_WIDTH` changes the width. `MAST_PREVIEW_CODE_REF=1` and `MAST_SNAPSHOT_POPULATED_CHANGES=1` populate the code reference and Changes fixtures. `--serve <request-directory>` starts the persistent preview server. Previews run a Cargo test on macOS with a Metal device, or on Linux with the lavapipe Vulkan driver and no window system (see `docs/internal/ui-preview.md`); outputs are under `target/ui-preview/` unless `CARGO_TARGET_DIR` is set.

## Pruning a shared target directory

Cargo keeps every superseded build of a workspace crate. On macOS each debug build also leaves its object files in `deps/` for debug info, so a target shared across branches grows by gigabytes a day. `scripts/prune-cargo-target.py <target-dir>` keeps the newest three builds of each workspace target in `deps/` and `examples/`, counting check-only builds separately, and deletes older ones and the profile's incremental cache. A build counts as workspace code only when its dep-info lists a crate root inside the repository, so third-party dependencies stay in place even when one shares a workspace target's name. It refuses to run while a build holds the target's Cargo lock. Run it only after the target's user has finished, because Cargo releases the lock while tests run. `--dry-run` reports what it would remove; `--keep N` changes how many hashes stay. `python3 -I scripts/tests/prune_cargo_target.py` tests it.

## Remote Cargo builds

`scripts/remote-cargo.sh` runs a command detached on the Fedora desktop (`david@100.105.54.55`, slots `a` and `b`) or the laptop (`david@100.76.3.65`, slot `b`) and waits for it without matching process command lines.

- `start <host> <remote worktree> <slot> <name> -- <command...>` runs the command in the worktree (an absolute path on the host, or relative to its home) under `setsid nohup`, logs to `/tmp/<name>.log` and writes the exit code to `/tmp/<name>.exit` when it ends. The slot's environment comes from `~/Work/mast-slot-<slot>.env` when that file exists (laptop; it is sourced by plain `sh`, so keep it POSIX), otherwise `CARGO_TARGET_DIR=~/Work/mast-target-<slot>` (desktop). It refuses a name already in use and a slot that holds a running job started through the script. Jobs started by hand are not seen, so check the slot first. Names are single-use, so use a new one for a rerun. The files live in the host's `/tmp` (tmpfs on both hosts, so logs use memory and a reboot makes the job `unknown`).
- `wait <host> <name> <minutes|Ns>` blocks until the exit marker appears and prints the exit code, up to 26 failure lines (`error` at line start, nextest `FAIL`, `FAILED`, `panicked`), the last few `test result` or nextest `Summary` lines, and a five-line tail. A bare number is minutes. Exit codes: 0 job succeeded, 1 job failed, 2 unknown job, 124 timeout, 125 the job died without a marker, 255 ssh failed. A timeout leaves the job running; wait again.
- `status <host> <name>` prints `running`, `exit N`, `died` or `unknown`; it exits 0 for the first two whatever N is and 1 for the others.

Each call is a single plain command, for example `scripts/remote-cargo.sh start david@100.105.54.55 /home/david/Work/mast-wt-x a my-test -- cargo nextest run -p mast-engine --test engine`.
