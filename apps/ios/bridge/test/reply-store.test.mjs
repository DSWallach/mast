import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readdir, stat, unlink, rmdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { FileIntentStore } from "../dist/reply-store.js";
test("reply journal survives reopening and stores private files", async () => {
  const dir = await mkdtemp(join(tmpdir(), "mast-reply-test-"));
  try {
    const first = new FileIntentStore(dir);
    const intent = { requestId: "r", phase: "sent", key: "fixture-operation-key", note: "private" };
    await first.save(intent);
    assert.deepEqual(await new FileIntentStore(dir).load("r"), intent);
    assert.equal((await stat(join(dir, (await readdir(dir))[0]))).mode & 0o777, 0o600);
    assert.equal(await first.load("missing"), null);
  } finally { for (const name of await readdir(dir)) await unlink(join(dir, name)); await rmdir(dir); }
});
