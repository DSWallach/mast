# Offscreen UI snapshots

On macOS with a Metal device, run outside the Codex sandbox (the sandbox blocks macOS service initialization):

```sh
ulimit -n 4096; CARGO_TARGET_DIR="$PWD/target-clean" cargo test -p mast-ui --features snapshot --test offscreen_snapshot -- --ignored --test-threads=1
```

Run from the Mast repository root. The test uses GPUI's `TestPlatform`, an in-memory window, and Metal texture readback. It does not create a native window or capture the screen. Images and visual diff maps are written to `target-clean/snapshots/`, with `.diff.png` suffixes. Baselines live in `crates/ui/tests/snapshots/`. To intentionally refresh them, add `MAST_UPDATE_SNAPSHOTS=1` to the command, inspect the changed PNGs, then rerun without the variable.

The feature-gated `mast_ui::snapshot::render_to_png(name, size, style, appearance, build_view)` installs a theme and writes a PNG under `$CARGO_TARGET_DIR/snapshots`. The builder sets up fixture-specific state. It waits 800 ms for GPUI's wall-clock entrance animations, then draws again. It is macOS-only and requires Metal and access to macOS services. The test's image-compare 0.5 dependency is MIT licensed; its hybrid similarity threshold is 0.9999. A high average score can hide a very small localized change; inspect the diff map. Size changes are errors. Baselines may vary across OS versions, fonts, scale factors and GPUs; use a consistent host and inspect intentional updates.
