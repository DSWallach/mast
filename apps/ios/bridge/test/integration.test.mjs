import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { WebSocketServer } from "ws";
import { engineBackend } from "../dist/backend.js";
import { createBridge } from "../dist/server.js";
import { fakeTokens, fixtureAccess, fixtureAudit } from "./support.mjs";
import { PairingChallenge } from "../dist/pairing.js";

test("HTTP inbox uses an isolated IPC snapshot", async () => {
  const ipc = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise(resolve => ipc.once("listening", resolve));
  ipc.on("connection", socket => socket.on("message", bytes => {
    const frame = JSON.parse(bytes.toString());
    const item = frame.method === "WatchSpaces" ? [{ id: "space", path: "/fixture/Space" }] : { available: true, synced: true, userActionRequests: [{ id: "request", text: "Review", options: [{ label: "Yes" }], facts: [{ label: "State", value: "Ready" }], links: [{ label: "Plan", target: { type: "url", url: "https://example.com" } }] }] };
    socket.send(JSON.stringify({ id: frame.id, item }));
  }));
  const store = fakeTokens();
  const backend = engineBackend(fileURLToPath(new URL("fake-mcp.mjs", import.meta.url)), `ws://127.0.0.1:${ipc.address().port}`);
  const bridge = createBridge(backend, store, new PairingChallenge("fixture", Date.now()), undefined, fixtureAccess, fixtureAudit);
  await new Promise(resolve => bridge.listen(0, "127.0.0.1", resolve));
  try {
    const base = `http://127.0.0.1:${bridge.address().port}`;
    const pair = await fetch(base + "/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "fixture", label: "Test device" }) });
    const { token } = await pair.json();
    const response = await fetch(base + "/requests", { headers: { authorization: `Bearer ${token}` } });
    const { items: rows } = await response.json();
    assert.equal(rows[0].space, "Space");
    assert.deepEqual(rows[0].options, ["Yes"]);
    assert.deepEqual(rows[0].facts, ["State: Ready"]);
    assert.match(rows[0].links[0], /https:\/\/example.com/);
    const { items: chats } = await (await fetch(base + "/chats", { headers: { authorization: `Bearer ${token}` } })).json();
    assert.equal(chats[0].id, "fixture-chat");
    assert.equal(chats[0].space, "Space");
  } finally {
    bridge.closeAllConnections();
    await new Promise(resolve => bridge.close(resolve));
    await new Promise(resolve => ipc.close(resolve));
  }
});
