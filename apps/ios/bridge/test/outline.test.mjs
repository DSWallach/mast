import { test } from "node:test";
import assert from "node:assert/strict";
import { WebSocketServer } from "ws";
import { readRequests } from "../dist/outline.js";

test("projects only open nonduplicate requests from fixture IPC snapshots", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise(resolve => server.once("listening", resolve));
  server.on("connection", socket => socket.on("message", bytes => {
    const frame = JSON.parse(bytes.toString());
    const item = frame.method === "WatchSpaces" ? [{ id: "space", path: "/work/Mast" }] : { available: true, synced: true, userActionRequests: [{ id: "open", text: "Review", chatId: "chat" }, { id: "resolved", state: "resolved" }, { id: "duplicate", deduped: true }] };
    socket.send(JSON.stringify({ id: frame.id, item }));
  }));
  try {
    const rows = await readRequests(`ws://127.0.0.1:${server.address().port}`);
    assert.equal(rows.length, 1);
    assert.equal(rows[0].id, "open");
    assert.equal(rows[0].space, "Mast");
    assert.equal(rows[0].origin, "chat");
  } finally { await new Promise(resolve => server.close(resolve)); }
});
