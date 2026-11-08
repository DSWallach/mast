import { test } from "node:test";
import assert from "node:assert/strict";
import { createBridge } from "../dist/server.js";
import { PairingChallenge } from "../dist/pairing.js";
import { ReplyCoordinator } from "../dist/reply.js";

test("allowlisted answer endpoint pairs, sends and retries only resolution", async () => {
  let saved = null, sends = 0, resolves = 0;
  const row = { id: "r", text: "Ship?", kind: "ask", state: "open", chatId: "c", system: false, chatAvailable: true, options: [] };
  const store = { load: async () => saved, save: async value => { saved = structuredClone(value); } };
  const engine = { openRow: async () => row, isBusy: async () => false, sendIdle: async () => { sends++; }, sendQueued: async () => { throw Error(); }, resolve: async () => { if (++resolves === 1) throw Error(); } };
  const tokenSet = new Set();
  const tokenStore = { add: async token => tokenSet.add(token), contains: async token => tokenSet.has(token) };
  const backend = { requests: async () => [], chats: async () => [], transcript: async () => [], send: async () => ({}) };
  const server = createBridge(backend, tokenStore, new PairingChallenge("fixture", Date.now()), new ReplyCoordinator(store, engine));
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  try {
    const base = `http://127.0.0.1:${server.address().port}`;
    const pair = await fetch(base + "/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "fixture" }) });
    const { token } = await pair.json();
    const headers = { "content-type": "application/json", authorization: `Bearer ${token}`, "idempotency-key": "fixture-operation-key" };
    const sent = await fetch(base + "/requests/r/answer", { method: "POST", headers, body: JSON.stringify({ outcome: "answered", text: "yes" }) });
    assert.equal((await sent.json()).state, "sent");
    const retry = await fetch(base + "/requests/r/resolve", { method: "POST", headers, body: "{}" });
    assert.equal((await retry.json()).state, "resolved");
    assert.equal(sends, 1); assert.equal(resolves, 2);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});
