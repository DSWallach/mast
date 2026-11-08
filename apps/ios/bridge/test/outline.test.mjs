import { test } from "node:test";
import assert from "node:assert/strict";
import { WebSocketServer } from "ws";
import { effectiveKind, readRequests } from "../dist/outline.js";

test("projects only open nonduplicate requests from fixture IPC snapshots", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise(resolve => server.once("listening", resolve));
  server.on("connection", socket => socket.on("message", bytes => {
    const frame = JSON.parse(bytes.toString());
    const item = frame.method === "WatchSpaces" ? [{ id: "space", path: "/work/Mast" }] : { available: true, synced: true, userActionRequests: [{ id: "open", text: "Review", chatId: "chat", options: [{ id: "a", label: "Alpha", detail: "Details" }] }, { id: "resolved", state: "resolved" }, { id: "duplicate", deduped: true }] };
    socket.send(JSON.stringify({ id: frame.id, item }));
  }));
  try {
    const rows = await readRequests(`ws://127.0.0.1:${server.address().port}`);
    assert.equal(rows.length, 1);
    assert.equal(rows[0].id, "open");
    assert.equal(rows[0].space, "Mast");
    assert.equal(rows[0].origin, "chat");
    assert.deepEqual(rows[0].optionChoices, [{ id: "a", label: "Alpha", detail: "Details" }]);
  } finally { await new Promise(resolve => server.close(resolve)); }
});
test("legacy request kind matches desktop classification", () => {
  assert.equal(effectiveKind({ text: "Choose", dedupeKey: "artopt:1" }), "choose");
  assert.equal(effectiveKind({ text: "Approve", dedupeKey: "admin-bulk:1" }), "approve");
  assert.equal(effectiveKind({ text: "Ready to merge branch" }), "handoff");
});
