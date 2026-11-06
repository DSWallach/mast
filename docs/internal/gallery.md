# Mast gallery

From the Mast repository, run `cargo run -p mast-gallery`. The gallery starts
with default settings in a temporary directory and does not bootstrap an engine.
It does not load the installed app's settings or data. The temporary directory
is removed when the process exits.

Choose a style in the toolbar or press 1–9; press L or D for light or dark.
The left rail selects the story. Controls uses Mast button, toggle, badge and
menu primitives, the composer input, and a message badge. Chat mounts the real
transcript and composer on an in-memory app state with fixture messages.
Settings → Appearance mounts the real Appearance page.

The chat sidebar, Work outline with item details, and PR walkthrough are static
fixture stubs: their real views are coupled to shell state and RPC-backed review
loading. Their controls do not implement the real views' behavior. The icon button, tabs, status dots, sidebar rows and outline rows are
illustrative fixtures rather than the actual production controls.

The temporary style list reflects the three styles on the integration branch;
`gallery_styles()` must return `UiStyleId::ALL` when the shared registry lands.
Side-by-side comparison is deferred: Mast's style is a process-global value, so
it needs a scoped style override to render multiple styles at once.

Agents must not run the gallery. Build with `cargo build -p mast-gallery` and
run `cargo test -p mast-gallery -- --test-threads=1` for headless story rendering.
