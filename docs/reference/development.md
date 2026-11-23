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

`MAST_PREVIEW_WIDTH` changes the width. `MAST_PREVIEW_CODE_REF=1` and `MAST_SNAPSHOT_POPULATED_CHANGES=1` populate the code reference and Changes fixtures. `--serve <request-directory>` starts the persistent preview server. Previews require macOS and a Metal device and run a Cargo test; outputs are under `target/ui-preview/` unless `CARGO_TARGET_DIR` is set.

## Pruning a shared target directory

Cargo keeps every superseded build of a workspace crate. On macOS each debug build also leaves its object files in `deps/` for debug info, so a target shared across branches grows by gigabytes a day. `scripts/prune-cargo-target.py <target-dir>` keeps the newest three builds of each workspace target in `deps/` and `examples/`, counting check-only builds separately, and deletes older ones and the profile's incremental cache. A build counts as workspace code only when its dep-info lists a crate root inside the repository, so third-party dependencies stay in place even when one shares a workspace target's name. It refuses to run while a build holds the target's Cargo lock. Run it only after the target's user has finished, because Cargo releases the lock while tests run. `--dry-run` reports what it would remove; `--keep N` changes how many hashes stay. `python3 -I scripts/tests/prune_cargo_target.py` tests it.
