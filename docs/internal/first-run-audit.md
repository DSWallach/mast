# First-run audit

Audit of `origin/work-outline-implementation` at `0892ac0d`, 2026-11-05. This is a headless audit; no GUI was opened. The clean build and runtime checks are in progress. Findings below distinguish source inspection from observed execution.

## Findings

1. **High — fresh settings migration is not called at app startup.** `crates/ui/src/lib.rs:165` calls `UiSettings::load`, not `load_for_startup` (`crates/ui/src/settings.rs:1688`). Consequently a new profile takes the legacy defaults in `settings.rs:825` and `:844`–`:851`: compact rows, sound, live Dock icon and menu bar item on, contrary to the Neutral defaults 1B table in `docs/internal/plans/work-outline/HANDOFF.md:674`. The migration tests call the unused entry point directly. **Proposed fix:** invoke the migration before any startup code creates the data directory, and add a test covering the actual startup wiring and an existing profile.

2. **Medium — packaging has undeclared Python dependency and repeats the build.** README.md:44–58 says to build the installed profile and then run `scripts/package-macos.sh`; the script builds a separate release profile (`scripts/package-macos.sh:31`) and installs `dmgbuild` through pip (`:115`–`:117`). A fresh macOS may not have pip or may prevent user installation. **Proposed fix:** document Python 3, pip and dmgbuild, the second build, and provide explicit dependency setup; longer term make packaging reuse a selected build.

3. **Medium — packaging ignores a custom Cargo target directory.** `scripts/package-macos.sh:25` and `:35` hard-code `target/`, whereas Cargo honours `CARGO_TARGET_DIR`. With that environment variable the release build and the binary copied into the bundle differ, or packaging fails. **Proposed fix:** derive both build and package paths from the selected Cargo target directory.

4. **Low — PR error mentions a specific employer.** `crates/ui/src/pr_review_launch.rs:128` tells every user to add Cofactr's workspace superproject. **Proposed fix:** use a generic repository-folder instruction.

5. **Low — getting-started describes GitHub authentication as mandatory.** `docs/guide/getting-started.md:7` lists `gh` with prerequisites for a first chat, whereas README.md:41 restricts it to PR features. **Proposed fix:** say it is optional and requires sign-in for GitHub features.

## Coverage and limitations

- The outline's empty-state copy and create action have a headless gpui test in `crates/ui/src/work_outline/guidance_tests.rs:19`; the no-Space onboarding copy is in `crates/ui/src/shell.rs:9990`. These are source observations until the tests run.
- GitHub's typed no-auth message tells the user to run `gh auth login` (`crates/ui/src/github/client.rs:105`); Linear requests a personal API key (`crates/engine/src/linear.rs:340`). PR dashboard and Start Chat behavior remain to be exercised.
- The clean build, isolated engine, and headless UI test results will be recorded here. The package script and dev script were not run because they perform destructive cleanup and launch a GUI respectively.
