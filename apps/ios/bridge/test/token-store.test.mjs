import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
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
    await store.add(hash);
    assert.equal((await stat(join(directory, "tokens.json"))).mode & 0o777, 0o600);
    assert.equal((await readFile(join(directory, "tokens.json"), "utf8")).includes(token), false);
    const reopened = await FileTokenStore.open(directory);
    assert.equal(await reopened.contains(hash), true);
    await reopened.revokeAll();
    assert.equal(await store.contains(hash), false);
    assert.equal(await (await FileTokenStore.open(directory)).contains(hash), false);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
