# Sidebar pin integrity

## Storage and scope

Synced profiles store one `sidebarPins/{sessionId}` row per pin in the per-user/org registry. Independent `pinned` and `orderKey` fields use the existing per-field logical clocks. Moving a pin writes only its position; unpinning writes explicit false without deleting its row. A delayed move cannot re-pin an item. Concurrent moves of different pins survive independently; concurrent moves of the same pin resolve by their clocks. Pin operations never rewrite sessions or normal activity ordering.

Local profiles retain profile-keyed device preferences. Remote profiles do not import local pin lists or older whole-list preferences from development versions of this PR. There is no pin migration, compatibility adapter or server cutover. Threads are unaffected; old development pins must be pinned again.

The `preferences/sidebarPins` row records readiness only, never membership or order. Once an authoritative snapshot arrives, even an empty list is initialized so it remains editable offline after restarting. Cleanup runs against the same locked authoritative chat/pin snapshot, never independent UI watch streams. Archived chats keep their pins; deleted chats are unpinned individually.

## Hand-off transfers and mixed versions

Synced workspaces move the pin, its order key and the pinned location in the registry batch that completes the user hand-off or agent standing-role rotation. A successor that is already pinned keeps its position and the transfer removes the source pin. Repeated rotation or completion of the same hand-off leaves later pin edits intact.

In local workspaces, the host reports completed hand-offs with predecessors before successors in a chain. The desktop applies them to its device-local pins in that order. Manual hand-offs use a persisted UI intent and respect pin edits made while the hand-off was in flight. Agent and automatic completions are recorded once even when there was no pin to move. The UI writes the updated pin list and the completion receipts in the same atomic settings replacement. It retains receipts beyond 200 completions because the host can replay the full history.

The optional `automatic` watch field defaults to false and older clients ignore it. Synced pin transfers are registry writes from the completing host and become visible to any client that reads those pin rows. Local automatic transfers require both a host that records and reports agent and automatic Manager completions and a desktop that consumes them. Hand-offs completed on older hosts without that provenance are not inferred or backfilled.

## Ordering keys

Rust and Swift use the same byte-ordered, variable-length hexadecimal algorithm. It allocates a prefix interval strictly between adjacent keys, then appends a fixed-width hexadecimal encoding of the unique operation HLC, padded to 149 bytes, plus a nonzero terminator. Distinct operation clocks cannot generate identical keys. The complete key participates in allocation, so another item can be inserted between keys generated concurrently in the same gap. No floating-point arithmetic or random jitter is used.

Keys are bounded at 8192 ASCII bytes, below the registry operation-size budget. Exhaustion fails explicitly rather than truncating, colliding or rewriting unrelated pins. Edits observe the affected pin's clocks before generating a later local operation, including clocks ahead of local wall time. Uniqueness relies on the registry's existing device identity and persisted monotonic clock assumptions.

The maximum of 200 is an admission limit for new pins, including hidden and archived pins. Simultaneous offline additions can exceed it; all existing pins remain visible, reorderable and removable. Cleanup never truncates overflow. Further additions are rejected until capacity is available.

## Interaction and persistence

Menu and drop paths validate profile identity, readiness, IDs and capacity before changing anything. Unknown preferences are not editable. Rejected drops animate back; successful drops do not replay the activity-sort animation. Moving a visible pin preserves every other pin's relative order, including hidden and archived pins.

Dragging changes only the preview. Each accepted drop queues one pin, unpin or move intent, not a replacement list. Neighbor IDs are resolved against current state by the engine: a surviving right anchor takes precedence, otherwise the left anchor is used; if both disappear, append. Moving an already unpinned item is a no-op.

Pending intents are projected over the latest confirmed state. Unrelated mutations cannot cancel their task. Writes are serialized per engine attachment. Failure removes the affected optimistic entry and reveals current confirmed state plus remaining intents, never a stale full-list backup. Old profile/attachment/queue replies are ignored. Operations use the existing durable pending outbox and snapshot persistence; engine acknowledgement is not proof of remote delivery.

Watch and acknowledgement revisions are monotonic within an engine attachment, not synchronized order keys. Older messages cannot replace a newer confirmed snapshot. A timeout cancels unsent edits and blocks new writes until the original request resolves or its attachment is replaced. Late confirmations are still reconciled.

## Regression commands

```sh
cargo test --locked -p mast-proto --lib
cargo test --locked -p mast-doc --lib
cargo test --locked -p mast-engine --lib
cargo test --locked -p mast-ui --lib -- --test-threads=1
cd edge && npm run typecheck && npm test
```

Run `ZeronTests/PinnedSessionsTests` on an iOS simulator for Swift key parity, dense insertion, membership, persistence and ignored old preferences. Registry tests cover independent concurrent moves, move/unpin races, duplicated delivery, restarts and capacity overflow. Workerd tests verify single-pin writes and unchanged session activity on real SQLite. Native UI and RPC tests cover filtered movement, drag gaps, empty targets, animated returns, successful-drop animation suppression, serialization, failures, timeouts and late acknowledgements.
