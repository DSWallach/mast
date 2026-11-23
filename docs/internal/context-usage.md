# Context usage

Every selected conversation has a context ring beneath its composer, including
project-less and remote conversations. Hovering shows measured tokens and
remaining capacity. The ring turns amber at 75% and red at 90%; its drawing clamps
at a full circle while the label preserves over-capacity measurements. A dash
means the harness has not reported enough data to calculate a percentage. Measured
zero is displayed as 0%.

The host normalizes context occupancy separately from billing `Usage` events:

| Harness | Measurement |
| --- | --- |
| Claude Code | Latest parent assistant message's input plus cache-read/cache-creation tokens; capacity from that model's result metadata. Aggregate result billing and child agents are excluded. |
| Codex | Latest model call (`tokenUsage.last`), with `modelContextWindow`; never the cumulative thread total. |
| OpenCode | Latest parent assistant message's total, or input/output/cache counts; capacity from the advertised provider/model catalog. Empty in-progress placeholders do not clear a measurement. |
| ACP (Devin, Grok, Hermes, pi) | `usage_update.used` and advertised capacity when the agent reports them. |
| Cursor | The pinned SDK exposes billed per-turn counts, not context occupancy. The shared control shows unavailable. |
| Mock / older hosts | Unavailable until a context snapshot is supplied. |

For Claude chats the Context Window selector names the window the CLI reported
when no choice is stored and the reporting model is the selected one; the CLI can
give a model 1M on its own. An explicit pick is always stored and wins: 1M adds the
`[1m]` model suffix, and 200K sets `CLAUDE_CODE_DISABLE_1M_CONTEXT=1` so the meter
and the automatic Manager handoff, which both use the reported window, see 200K.
The Claude catalog's default choice is "Default (set by Claude)", not 200K: New-chat
and scheduled-task controls cannot know the window yet, and a chat that has not
reported one shows the same. The harness passes neither the `[1m]` suffix nor
`CLAUDE_CODE_DISABLE_1M_CONTEXT` for it, so the CLI decides. An older host's
catalog has no such choice and keeps the 200K default.

`AgentEvent::ContextUsage` updates one atomic `meta.contextUsage` value in the
session document. Partial measurements preserve known fields; a zero capacity is
ignored. New non-resumed runs clear old usage. Post-turn updates can refresh the
snapshot without reopening a completed turn, and subagent events cannot change
the parent meter.

The existing document sync carries the value through the relay and local storage.
`WatchDocMessages` includes a typed `TranscriptUpdate` envelope with the snapshot,
including context-only commits and the opening reset. Old readers ignore the
additive field; old hosts decode as unavailable. Thin-document rebuilding also
preserves the snapshot. UI rendering reads the in-memory state, with no polling,
provider calls, filesystem access, or new remote endpoint.

Validation covers adapter normalization, compaction to zero, cache accounting,
subagent isolation, snapshot and incremental import, thin rebuilds, context-only
watch updates, reconnects, and old/new watch compatibility. Screenshots use the
native desktop attached to a viewing engine; a separate host sends deterministic
Claude protocol fixtures through a local relay. The measurements are test data.
