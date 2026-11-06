# Mast gallery (work in progress)

From the Mast repository, run `cargo run -p mast-gallery`. The gallery starts
with default settings in a temporary directory and does not bootstrap an engine.
It does not load the installed app's settings or data. The temporary directory
is removed when the process exits.

Choose a style in the toolbar or press 1–3; press L or D for light or dark.
Choose Controls or Chat in the left rail. The controls story currently contains
Mast button, toggle, badge and menu primitives, the composer input, and a message badge. The chat story mounts the real
transcript and composer on an in-memory app state with fixture messages.

The style list currently reflects the three styles on the integration branch;
it must be switched to the shared style registry when that registry lands.
Additional controls, sidebar, outline, review and Appearance stories are still
needed. Side-by-side comparison is deferred: Mast's style is a process-global
value, so it needs a scoped style override to render multiple styles at once.

Agents must not run the gallery. Check it with `cargo build -p mast-gallery`
and its headless tests instead.
