import { test } from "node:test";
import assert from "node:assert/strict";
import { createBridge } from "../dist/server.js";
import { fakeTokens, fixtureAccess, fixtureAudit } from "./support.mjs";
import { PairingChallenge } from "../dist/pairing.js";
import { ReplyCoordinator } from "../dist/reply.js";

test("allowlisted answer endpoint pairs, sends and retries only resolution", async () => {
  let saved = null, sends = 0, resolves = 0;
  let row = { id: "r", text: "Ship?", kind: "ask", state: "open", chatId: "c", system: false, chatAvailable: true, options: [] };
  const store = { load: async () => saved, save: async value => { saved = structuredClone(value); } };
  const engine = { openRow: async () => row, isBusy: async () => false, sendIdle: async () => { sends++; }, sendQueued: async () => { throw Error(); }, resolve: async () => { if (++resolves === 1) throw Error(); row = { ...row, state: "resolved", resolution: { outcome: "answered", note: "yes", values: { action: "answered" }, by: { kind: "user" } } }; } };
  const tokenStore = fakeTokens();
  const backend = { requests: async () => [], chats: async () => [], transcript: async () => [], send: async () => ({}) };
  const server = createBridge(backend, tokenStore, new PairingChallenge("fixture", Date.now()), new ReplyCoordinator(store, engine), fixtureAccess, fixtureAudit);
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  try {
    const base = `http://127.0.0.1:${server.address().port}`;
    const pair = await fetch(base + "/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "fixture", label: "Test device" }) });
    const { token } = await pair.json();
    const headers = { "content-type": "application/json", authorization: `Bearer ${token}`, "idempotency-key": "fixture-operation-key" };
    const sent = await fetch(base + "/requests/r/answer", { method: "POST", headers, body: JSON.stringify({ outcome: "answered", text: "yes" }) });
    assert.equal((await sent.json()).state, "sent");
    const firstResolve = await fetch(base + "/requests/r/resolve", { method: "POST", headers, body: "{}" });
    assert.equal((await firstResolve.json()).state, "resolving");
    const retry = await fetch(base + "/requests/r/resolve", { method: "POST", headers, body: "{}" });
    assert.equal((await retry.json()).state, "resolved");
    assert.equal(sends, 1); assert.equal(resolves, 2);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});
