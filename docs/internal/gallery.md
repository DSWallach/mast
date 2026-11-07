# Mast gallery

From the Mast repository, start the watcher with:

```sh
./scripts/gallery-watch.sh
```

It builds `mast-gallery`, opens it, and watches `crates/ui/src` and
`apps/mast-gallery`. After a successful incremental rebuild it restarts the
window. A failed build leaves the previous window running and retries on the
next edit. Press Ctrl-C in the terminal to stop the watcher and gallery. Python
3 and the Rust toolchain are required. `CARGO_TARGET_DIR` is respected.

The watcher keeps the selected story, style, layout, light/dark mode and theme
in a temporary file for the lifetime of the watcher. The gallery uses temporary
settings and fixture data; it does not connect to the engine. It reads the
installed app's style-overrides.json, if present, and refreshes the comparison
columns when that file changes. Save edits in Mast's Design panel to update it;
unsaved drafts stay in Mast. It never writes that file. Running `cargo run -p mast-gallery` directly also works,
but does not retain selections between launches unless `MAST_GALLERY_STATE`
points to a writable state file.

Select a component in the left rail to compare its states in Default,
Industrial and Area columns. Layout buttons (or Shift-1 through Shift-3)
select Minimal, Compact and Guided. L and D choose light and dark, and T cycles
through built-in themes. The 1–3 keys select the style for the full-screen
fixtures. The chat story mounts the real transcript and composer with in-memory
messages; the work outline and walkthrough remain fixture compositions.
Hover the examples to see their interactive hover treatment. The disabled row
is a dimmed fixture: the shared primitives do not all expose a disabled state.
The Board story is a gallery-only composition of outline rows and cards.

Agents must not launch the gallery. Run `cargo test -p mast-gallery --
--test-threads=1` to exercise the stories headlessly.
