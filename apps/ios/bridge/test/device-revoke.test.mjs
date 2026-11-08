import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readdir, unlink, rmdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomBytes } from "node:crypto";
import { FileTokenStore } from "../dist/token-store.js";
import { tokenDigest, PairingChallenge } from "../dist/pairing.js";
import { createBridge } from "../dist/server.js";
import { fixtureAccess, fixtureAudit } from "./support.mjs";

test("revoking one device immediately denies that token but preserves the other", async () => {
  const dir = await mkdtemp(join(tmpdir(), "mast-devices-"));
  const store = await FileTokenStore.open(dir), first = randomBytes(32).toString("base64url"), second = randomBytes(32).toString("base64url");
  const device = await store.add(tokenDigest(first), "Phone"), other = await store.add(tokenDigest(second), "Tablet");
  const backend = { requests: async () => [], chats: async () => [], transcript: async () => ({}), send: async () => ({}) };
  const bridge = createBridge(backend, store, new PairingChallenge("unused", Date.now()), undefined, fixtureAccess, fixtureAudit);
  await new Promise(resolve => bridge.listen(0, "127.0.0.1", resolve));
  try {
    const base = `http://127.0.0.1:${bridge.address().port}`, fetchSpaces = token => fetch(base + "/spaces", { headers: { authorization: `Bearer ${token}` } });
    assert.equal((await fetchSpaces(first)).status, 200);
    await (await FileTokenStore.open(dir)).revoke(device.id);
    assert.equal((await fetchSpaces(first)).status, 401);
    assert.equal((await fetchSpaces(second)).status, 200);
    assert.deepEqual(await store.devices(), [other]);
  } finally { bridge.closeAllConnections(); await new Promise(resolve => bridge.close(resolve)); for (const name of await readdir(dir)) await unlink(join(dir, name)); await rmdir(dir); }
});
