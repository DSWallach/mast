# First-run audit

Audit of `origin/work-outline-implementation` at `0892ac0d`, 2026-11-05. This is a headless audit; no GUI was opened. The clean build and runtime checks are in progress. Findings below distinguish source inspection from observed execution.

## Findings

1. **Medium — headless-first startup can select legacy settings.** Headed startup calls the migration before logging creates the directory (`apps/mast/src/main.rs:136`), but headless startup skips it and creates the directory for logs (`:158`). A subsequent first headed launch classifies any existing data directory as an existing profile (`crates/ui/src/settings.rs:1689`), selecting legacy values rather than the Neutral defaults 1B table. This is a source-derived scenario, not yet exercised. **Proposed fix:** record the profile classification before any mode creates the data directory, while preserving existing profiles.

2. **Medium — packaging has undeclared Python dependency and repeats the build.** README.md:44–58 says to build the installed profile and then run `scripts/package-macos.sh`; the script builds a separate release profile (`scripts/package-macos.sh:31`) and installs `dmgbuild` through pip (`:115`–`:117`). A fresh macOS may not have pip or may prevent user installation. **Proposed fix:** document Python 3, pip and dmgbuild, the second build, and provide explicit dependency setup; longer term make packaging reuse a selected build.

3. **Medium — packaging ignores a custom Cargo target directory.** `scripts/package-macos.sh:25` and `:35` hard-code `target/`, whereas Cargo honours `CARGO_TARGET_DIR`. With that environment variable the release build and the binary copied into the bundle differ, or packaging fails. **Proposed fix:** derive both build and package paths from the selected Cargo target directory.

4. **Low — PR error mentions a specific employer.** `crates/ui/src/pr_review_launch.rs:128` tells every user to add Cofactr's workspace superproject. **Fix:** generic repository-folder instruction on this branch.

5. **Low — getting-started describes GitHub authentication as mandatory.** `docs/guide/getting-started.md:7` lists `gh` with prerequisites for a first chat, whereas README.md:41 restricts it to PR features. **Fix:** identify it as optional and require sign-in for GitHub features on this branch.

## Coverage and limitations

Headed startup runs `load_for_startup` before logging (`apps/mast/src/main.rs:136`); its fresh-profile tests cover the 1B table (`crates/ui/src/settings.rs:1939`, `:1967`, `:1995`, `:2041`). The direct UI load occurs after that migration.

- The outline's empty-state copy and create action have a headless gpui test in `crates/ui/src/work_outline/guidance_tests.rs:19`; the no-Space onboarding copy is in `crates/ui/src/shell.rs:9990`. These are source observations until the tests run.
- GitHub's typed no-auth message tells the user to run `gh auth login` (`crates/ui/src/github/client.rs:105`); Linear requests a personal API key (`crates/engine/src/linear.rs:340`). PR dashboard and Start Chat behavior remain to be exercised.
- The clean build, isolated engine, and headless UI test results will be recorded here. The package script and dev script were not run because they perform destructive cleanup and launch a GUI respectively.
