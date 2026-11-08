import { test } from "node:test";
import assert from "node:assert/strict";
import { createBridge } from "../dist/server.js";
import { PairingChallenge } from "../dist/pairing.js";

test("allowlisted endpoints require a paired token and reject browser origins", async () => {
  const hashes = new Set();
  const calls = [];
  const store = { add: async hash => hashes.add(hash), contains: async hash => hashes.has(hash) };
  const backend = { requests: async () => ["fixture"], chats: async () => [], transcript: async id => [id], send: async (id, text) => { calls.push([id, text]); return { delivery: "queued" }; } };
  const server = createBridge(backend, store, new PairingChallenge("fixture-code", Date.now()));
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  try {
    const base = `http://127.0.0.1:${server.address().port}`;
    assert.equal((await fetch(base + "/requests")).status, 401);
    const pair = await fetch(base + "/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "fixture-code" }) });
    const { token } = await pair.json();
    const headers = { authorization: `Bearer ${token}` };
    assert.deepEqual(await (await fetch(base + "/requests", { headers })).json(), ["fixture"]);
    assert.equal((await fetch(base + "/requests", { headers: { ...headers, origin: "https://example.com" } })).status, 403);
    assert.equal((await fetch(base + "/rpc", { method: "POST", headers })).status, 404);
    const sent = await fetch(base + "/chats/abc/messages", { method: "POST", headers: { ...headers, "content-type": "application/json" }, body: JSON.stringify({ text: "hello" }) });
    assert.equal((await sent.json()).delivery, "queued");
    assert.deepEqual(calls, [["abc", "hello"]]);
    assert.equal((await fetch(base + "/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "fixture-code" }) })).status, 401);
  } finally { await new Promise(resolve => server.close(resolve)); }
});
