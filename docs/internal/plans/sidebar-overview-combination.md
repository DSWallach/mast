# Combine Overview and Sidebar

Status: implementation complete; final review and the requested Mast Dev QA build are pending.
Work item: Sidebar and Overview combination (`22ac3419111b39808a8565dbc57c6498`).
Branch: `david/sidebar-overview-combination`, based on local `david-rolling` at `4cc809e2` (three commits ahead of fetched `origin/david-rolling` on 2026-11-16).

## Goal and layout

Make Overview the persistent navigation surface in the left column where the sidebar currently appears. Preserve its work-management functionality and bring across every sidebar-only capability. Replace the two overlapping navigation surfaces with one.

Proposed layout:

- **Left column — Overview:** assistant/current-chat access, Space scope, New chat, New item, search, attention counts, view controls, pins/custom sections, and the Work Outline. Keep account, connection, update, and error controls at the bottom.
- **Main workspace:** the selected chat, work-item details, Required Actions, subagent transcript, Inbox/Linear/GitHub content, or another existing destination. The current transcript/composer remain the single live chat surface.
- **Existing right workspace panes:** files, diffs, browser, artifacts, and other chat tools continue to open beside the active content. Where space is insufficient, use the existing takeover/overlay pattern rather than squeezing the outline and chat beyond usable widths.

The column stays available when opening a chat or another destination. Collapse/expand and resize continue to work. On a narrow window, it becomes a dismissible overlay; Escape, selecting content, or clicking outside closes that overlay while preserving its navigation state. Settings may keep its current section navigation while open, with Back returning to the unified Overview.

## Verified baseline and preservation map

These observations come from reading the implementation at the base SHA, rather than running the app. Each source marked `measured` was inspected directly. Proposed behavior below is a design decision, not a claim about current behavior.

| Existing capability | Destination in the combined experience |
| --- | --- |
| Work Outline tree, item/chat selection, hierarchy, keyboard navigation, mutations, drag/drop, templates, and optimistic updates | Primary left-column body; reuse the current Outline implementation and command handlers. |
| New item/child, rename/move/associate chat, effective instructions, instruction creation, start chat for item, shortcuts | Compact toolbar, row menus, and existing keyboard/palette commands; no command removal. |
| Item details, plan/artifact access, instructions/proposals, Required Actions and decisions | Open in the main workspace; preserve return to the previous chat/details target. |
| Search, repository filter, hide stale, archived, needs-interaction/stalled filters, hidden Spaces, completed visibility | Search and a consolidated View/Filter menu, with active-filter indicators and a clear/reset action. |
| Chat preview and live composer, subagent read-only peek, PR/ticket metadata and link editor | Chat or peek in the main workspace; compact metadata remains on navigation rows, full controls in details/menus. |
| Inbox/Linear and GitHub integrations, including associated outline actions and drops | Main-workspace destinations with working selection/association actions and the same access conditions. |
| Sidebar Space selector and Space management | Scope control above the tree; preserve All Spaces, projectless sessions, creation/rename and existing Space menus. |
| Personal assistant and current session outside the active filter | Small fixed access area above the scoped body, visibly identified when outside scope. |
| Pinned sessions, manual pin ordering, custom sections, section menus and archive-all | Overview highlights pinned chats in place. The separate Pinned collection and manual pin ordering appear only in Sessions. Session-list collections appear only in Sessions, backed by the existing synced records. |
| Archived accordion, restore/archive, scheduled-task runs grouped by task and task-history entry points | Archived and Scheduled collections in the same column; retain existing actions and counts. |
| Organization by project/device/one list, created/updated sort, compact rows, project icon/label, branch/PR/harness visibility | View menu for a Sessions presentation within this same Overview column. Work Outline remains the default. |
| Session context menu and conditional actions | Shared session menu on every chat occurrence, including rename, pin, archive/restore, imported-session sync, PR/ticket links, review, handoff, side questions, admin/assistant actions, worktree closeout, copy/IDs/provider links, and confirmed deletion where currently available. |
| User/account menu, local/synced/development identity, settings, connection state, update strip, mutation failures | Fixed shell-owned footer around the Overview body; keep all existing menu destinations and recovery controls. |
| Jump shortcuts, command palette, history, active-chat highlight, collapse/resize and theme behavior | Retain commands, retarget their handlers and reveal the selected row without resetting scroll/expansion. |

Sources: [Overview composition and panels](crates/ui/src/overview.rs:4654 "measured"), [Overview commands and details](crates/ui/src/overview.rs:5974 "measured"), [Outline commands](crates/ui/src/work_outline/command.rs:14 "measured"), [Outline interaction implementation](crates/ui/src/work_outline/outline_view.rs:148 "measured"), [sidebar body/footer](crates/ui/src/shell.rs:8023 "measured"), [session menus](crates/ui/src/shell.rs:9354 "measured"), [sidebar organization and scope](crates/ui/src/shell/spaces.rs:3889 "measured"), [assistant/current session](crates/ui/src/shell/spaces.rs:5964 "measured"), [pins](crates/ui/src/shell/sidebar_pins.rs:1 "measured"), [custom sections](crates/ui/src/shell/sidebar_sections.rs:15 "measured"), [GitHub integration](crates/ui/src/overview/github_host.rs:17 "measured").

Important baseline detail: production Overview restores Outline mode; List is retained for legacy rendering/tests. The proposed Sessions presentation preserves sidebar organization options without reviving a second full-page Overview. Preserve still-used List-only actions through shared row/details components before retiring legacy rendering. [Mode restoration](crates/ui/src/overview.rs:1342 "measured")

## Interaction decisions

1. **One navigation surface, two presentations.** Default to Work Outline. Offer Sessions in its View menu for flat/project/device organization and sorting. Overview highlights pinned chats in place; the Pinned collection appears only in Sessions. Custom sections, archive, and scheduled-run collections appear in Sessions; Overview shows the Work Outline. Switching presentation preserves each presentation's scroll, selection, and expansion.
2. **Separate shortcuts from hierarchy.** Pins and custom sections reference chats; moving a shortcut never reparents a work item. Outline drag/drop retains its existing work-item/chat semantics. Label drop targets explicitly, retain remote-sync readiness checks, and show mutation errors with rollback. A chat may have a shortcut and an outline occurrence; it occurs only once within each collection and highlights consistently by ID.
3. **Separate navigation selection from the open workspace.** Arrow keys move the tree selection; Enter/click opens the target. Chat selection opens the normal live chat. Item selection opens details. Existing focus-toggle/full-chat commands move focus between column and workspace. Returning from details, Inbox or a subagent restores the prior target without losing a draft.
4. **Search and scope remain predictable.** Space scope filters navigation, not the selected chat's execution target or an existing draft. Show the current session outside scope in the fixed access area. Search and attention/repository filters operate consistently across collections, retaining matching ancestors in the outline. Display hidden-result counts and a clear-filter recovery action.
5. **Fit controls to the column.** Use a full-width search row, a compact New menu/button group, and overflow menus for secondary filters/actions. Wrap or progressively disclose metadata; do not scale down the full-page header. Proposed starting width: 360 px, adjustable using the existing sidebar sizing constraints. Final minimums should be established with long titles and the existing themes before shipping.

## Implementation sequence

### 1. Extract navigation and workspace ownership

Split `Overview` into a persistent navigation controller/body and workspace panel hosts. Keep `OutlineView`, data loading, action dispatch, and persisted state. Let `Shell` own the live transcript/composer and select the workspace target. Adapt work-item details, Required Actions, subagent, and integration panels to the workspace host.

This split is required before mounting the column alongside Chat: today `ensure_overview_page` explicitly shares Shell's transcript/composer only because the routes render exclusively. Rendering the existing Overview wholesale beside Chat would break that assumption. [Shared-entity constraint](crates/ui/src/shell.rs:5014 "measured")

### 2. Mount Overview in the sidebar slot

Replace the chat sidebar body with compact Overview navigation inside the existing resize/collapse/footer shell. Route existing Open Overview/Open Outline actions to reveal and focus this column. Map saved Overview history entries to that intent. Keep new-chat, tabs, back/forward, full-chat, and reveal-in-outline behavior coherent through one selection/open-target model.

### 3. Bring over sidebar collections and actions

Reuse pins, sections, assistant/current-chat, archived and scheduled-task implementations. Extract a common session action builder used by outline and session rows. Add Sessions presentation and view preferences. Reuse durable commands instead of implementing alternate mutations. Preserve group archive actions and confirmations.

### 4. Preserve settings and complete the cutover

Keep synced pin/section storage unchanged. Read existing sidebar width/collapse, density, metadata, sort and organization settings; retain Overview filters and per-Space collapse state. A fresh install defaults to Work Outline; previous sidebar organization is available in Sessions. Make any necessary settings migration versioned and idempotent, with old data left readable.

Remove the separate Overview page and redundant sidebar renderer only after every row in the preservation map has a reachable replacement. Audit all route checks, command/palette entries, deep links, screenshot fixtures, and integrations that currently assume Overview is a page.

### 5. Verify parity and review

- Navigation tests: open chat/item/peek/integration, back/forward, focus toggle, jump shortcuts, deep link, selected row hidden by filters, collapse/resize, and return to an unsent draft. Assert the transcript/composer are mounted exactly once.
- Data/action tests: pin ordering and cross-collection moves, no unintended hierarchy changes, section archive-all, archived restore, scheduled grouping, Space/profile switch, remote preference readiness, optimistic rejection/rollback, and persisted-state migration.
- Preserve Outline command, instruction/template, Required Action, plan/artifact, Linear/GitHub association, PR/ticket editing and subagent workflows through existing tests plus focused host-integration regressions.
- Manual UI matrix: narrow/normal/wide windows, light/dark themes and current style variants, long/deep outlines, empty/offline/error/loading states, large chat lists, drag autoscroll and reduced motion. Compare responsiveness and idle fetching against the current views; avoid duplicate metadata polling.
- Run formatting, relevant `mast-ui` tests and targeted build checks in this worktree using the shared Cargo target directory. No builds or test results are claimed for this planning-only change.
- Before merging implementation, run the repository's `pr-review` gate, address every finding and rerun until CLEAR at the final SHA. Do not merge as part of planning.

## Done when

Overview occupies the existing sidebar position while a chat or other workspace content is open. Every existing Overview capability and sidebar capability has a reachable home in the combined experience. Saved pins/sections/preferences survive restart and profile changes, keyboard navigation remains useful, and chat drafts and panel-return behavior survive navigation. There is one authoritative navigation surface and one live transcript/composer host.

No blocking product questions are needed to begin implementation. The 360 px starting width and narrow-window overlay are proposed defaults to validate visually; the implementation should record any changes to those defaults in this plan.


## Implementation progress

### First checkpoint: rendering boundaries

- Extracted navigation rendering and workspace-panel composition into `crates/ui/src/overview/layout.rs`. The current Overview still composes both, so its visible layout and route behavior are unchanged at this checkpoint.
- Added a UI regression that mounts navigation alone, then restores/removes the chat panel repeatedly with the same Overview controller. It checks that navigation does not mount or close the live chat panel and that the selected chat survives.
- This establishes the rendering boundary for step 1. Moving panel ownership/routing into Shell, compact navigation, the left-column mount, sidebar capability migration and final cutover remain to implement.

Validation: `cargo test -p mast-ui --lib overview::logic_tests:: -- --test-threads=1` compiled successfully and ran 318 tests: 317 passed, including the new navigation/workspace regression. The existing `overview_indicates_sessions_needing_interaction_or_stalled` test failed because `overview-row-stalled-c-errored` was not mounted. Running that exact test with the unchanged base `overview.rs` reproduced the identical assertion failure. No change to that unrelated test was made. New layout module formatting and `git diff --check` passed. The repository-wide isolation audit reports existing findings outside these changes.

This checkpoint is not a merge-ready implementation of the feature. The item remains active; next is the compact left-column host and Shell workspace routing.


### Second checkpoint: persistent left-column Overview

- Mounted the Overview navigation in the sidebar position. Chats use Shell's normal main workspace; work-item details, Required Actions, Inbox/Linear and PR/ticket editing use the Overview workspace outlet without a second navigation tree or live composer.
- Added a persisted Overview/Sessions presentation switch. Sessions retains the existing project/device/list grouping, sort, metadata and context-menu controls. Overview keeps assistant/current-session access inside its fixed header; the shell retains the Space selector and account/connection/update/error footer. Sessions keeps its original assistant/current-session section.
- Kept pins, custom sections, scheduled-task groups and archived sessions above the outline in an independently scrolling collection region. Added an explicit height and geometry regression so populated collections cannot collapse to zero height.
- Space scope filters navigation while leaving the open chat unchanged. Opening Overview reveals/focuses the column; arrow navigation leaves the workspace alone until Enter/click. Chat drafts survive focus toggles and opening/closing Required Actions or Inbox.
- Fresh settings use a 360 px column. Saved widths remain intact. Added cached-sidebar invalidation for Overview changes and fixed Full Chat's shell re-entry path.
- Updated routing tests to the persistent Outline behavior. The instruction fixture now notifies already-mounted views when seeded, and its drift comparison test exercises the production Outline column.

Validation:

- 37 keyboard/routing tests passed.
- 87 settings tests passed.
- 340 Overview-related tests passed, with two existing ignored tests. The previously baseline-reproduced `overview_indicates_sessions_needing_interaction_or_stalled` failure remains explicitly skipped.
- Native light/dark preview rendered and was inspected. The published [native preview](mast-artifact:f8e561b1409df4a8161ee445670c7d5e) also passed the artifact render check at both artifact viewport widths in both themes. This checks presentation of the preview image, not narrow native-window behavior.
- New module formatting and whitespace checks passed. All Mast builds used the shared Cargo target directory.

Remaining before the full consolidation is merge-ready: finish the capability-by-capability parity audit, check Overview filter behavior and the Sessions presentation, validate narrow native windows and the proposed overlay, exercise the full context-menu and remote-preference workflows, and run the required `pr-review` gate on the final commit. The branch has not been merged; the work item remains active.


### Final implementation and parity audit

- Narrow windows below 780 px start with Overview closed and use a dismissible overlay when opened. Opening a destination or clicking outside closes it without changing the saved wide-window width or collapse preference. Wide windows restore the normal column.
- Overview's search, repository, stale and attention filters now apply to pinned/custom/scheduled/archived collections as well as the tree. The current-chat access row remains fixed. Sessions keeps its own presentation and the shared Space scope.
- Single-click opens a chat or work-item destination. Clicking a chat keeps navigation focus for continued browsing; Enter focuses its composer. Shift/multi-selection and disclosure controls retain their existing behavior.
- Fixed the Outline archive shortcut to use navigation focus even while a Chat occupies the main workspace. Artifact Find keeps priority inside artifact tabs.
- Audited the preservation map against the shared shell implementations: account/footer controls, assistant/current chat, Space menus, session menu actions, grouping and sort, managed-account badges, pins and section drag/drop, archived restore, scheduled grouping, outline commands, details/instructions/proposals/artifacts, Required Actions, Inbox/Linear/GitHub, and chat/subagent tools all remain reachable. The Sessions presentation retains the complete existing session menu; no new wire or sync schema was introduced.
- Updated the user guide and Work Outline reference to the persistent navigation, focus and workspace behavior.

Final validation before review:

- 342 Overview-related tests passed, two ignored, with the previously confirmed baseline stalled-badge failure skipped.
- The full shell suite has 401 passing tests and five ignored tests. Its four failures were reproduced on the original `4cc809e2` base: the wake-switch settings test, two outline-group sidebar-section fixtures, and the Start Chat auto-approve fixture. These are pre-existing failures, not waivers of findings in this feature.
- Nine unified-column integration tests pass after review corrections. They cover workspace/draft return, presentation switching, pin geometry, filtered pins, full-history/archived filtering and reset, custom membership without outline duplication, session-jump preferences, Escape/New Chat dismissal, and the 600 px overlay restoring its wide layout.
- Settings persistence: 87 tests passed. Native light/dark preview and artifact render checks passed; the final 600 px native light/dark preview was inspected and its artifact render checks passed without warnings.

The requested deliverable is a signed **Mast Dev.app** built from this feature branch after the required review passes. It uses the Dev bundle identity and isolated Dev data/IPC configuration. No merge into `david-rolling` or `main` is part of this request.

### Manual QA in Mast Dev

1. Open a Chat, type an unsent draft, browse with arrows, click another Chat, and use the focus toggle. Confirm drafts survive navigation.
2. Pin/reorder a Chat and move it between custom sections. Exercise search, repository/attention filters, archive and restore. Confirm the current Chat remains reachable outside the scope.
3. Switch to Sessions and try project/device/list organization, sorting, metadata toggles and the session context menu.
4. Open item details, edit/cancel instructions, open Compare from a drift marker, Required Actions and Inbox. Close panels and confirm the selected Chat/draft returns.
5. Resize to about 600 px, open Overview, select a destination and reopen it. Resize wide and confirm the saved width/collapse setting returns.
6. Try artifact Find, the archive shortcut with the tree focused, New Chat, Space switching and the footer's Settings/Back flow.

### Review corrections

The first Sonnet review identified four findings. All are addressed: full-chat filters preserve archived and older scheduled matches, collection filtering has a hidden count and Clear filters action, numbered jumps preserve the saved presentation, narrow overlays support Escape and all primary destinations, with the focused search field retaining first priority to clear its query, and touched Rust files are formatted. Custom collection membership remains separate from outline-derived groups to avoid showing the same groups twice. Regression tests cover these behaviors; final review and build receipts are added to the linked plan after completion.

### Follow-up: assistant/current-session integration and empty-state wrapping

Moved personal assistant and current-session access into the fixed Overview header, independent of its filters. A single card carries both labels when they refer to the same chat; scheduled assistants resolve to the latest run. The chooser and context menus reuse the existing designation calls. Normal and compact outline rows retain their hierarchy and show role badges. Card and full-chat actions pass through the existing unsaved-details confirmation.

Constrained empty-state and first-use text to the navigation width so Needs interaction with no matching chats wraps within the column. Geometry verification covers 240, 300 and 360 px columns. Native light/dark previews cover all three styles and layouts, including the narrow empty-filter state. Final validation and the pinned review gate are recorded in the linked work-item plan.

### Follow-up: highlight Overview pins in place

Overview keeps pin/unpin actions and highlights pinned chats at their normal outline positions. It no longer renders the separate session-list block between the view-switch buttons and the Overview header. Saved pins stay intact and appear in Sessions, including after switching presentations. Pinning never moves or reparents its Outline row. Saved custom-section and scheduled-task collections remain available in Sessions.
