import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, unlink, rmdir, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { FileTokenStore } from "../dist/token-store.js";
import { tokenDigest } from "../dist/pairing.js";

test("stores hashes privately and revokes them across restarts", async () => {
  const directory = await mkdtemp(join(tmpdir(), "mast-bridge-test-"));
  try {
    const token = "fixture-token";
    const hash = tokenDigest(token);
    const store = await FileTokenStore.open(directory);
    const device = await store.add(hash, "Test device");
    const otherHash = tokenDigest("other-fixture-token");
    await store.add(otherHash, "Other device");
    assert.equal((await stat(join(directory, "tokens.json"))).mode & 0o777, 0o600);
    assert.equal((await readFile(join(directory, "tokens.json"), "utf8")).includes(token), false);
    const reopened = await FileTokenStore.open(directory);
    assert.equal(await reopened.contains(hash), true);
    const thirdHash = tokenDigest("third-fixture-token");
    await Promise.all([reopened.revoke(device.id), store.add(thirdHash, "Third device")]);
    assert.equal(await store.contains(hash), false);
    assert.equal(await store.contains(otherHash), true);
    assert.equal(await store.contains(thirdHash), true);
    await reopened.revokeAll();
    assert.equal(await store.contains(hash), false);
    assert.equal(await (await FileTokenStore.open(directory)).contains(hash), false);
    assert.equal(await store.contains(otherHash), false);
    assert.equal(await store.contains(thirdHash), false);
  } finally { await unlink(join(directory, "tokens.json")); await rmdir(directory); }
});
