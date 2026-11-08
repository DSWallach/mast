import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readdir, stat, unlink, rmdir, readFile, utimes } from "node:fs/promises";
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

test('terminal decision text is removed while retry fingerprint remains', async () => {
 const dir=await mkdtemp(join(tmpdir(),'mast-redact-')); try { const store=new FileIntentStore(dir), intent={requestId:'r',phase:'resolved',key:'operation-key-123',input:{outcome:'answered',text:'private secret'},message:'private secret',note:'private secret'}; await store.save(intent); const raw=await readFile(join(dir,(await readdir(dir))[0]),'utf8'); assert.equal(raw.includes('private secret'),false); assert.equal((await store.load('r')).inputHash.length,64); }
 finally {for(const name of await readdir(dir))await unlink(join(dir,name));await rmdir(dir)}
});

test('old resolved journals are pruned but ambiguous delivery remains protected', async () => {
 const dir=await mkdtemp(join(tmpdir(),'mast-prune-')); try { const store=new FileIntentStore(dir); for(const [requestId,phase] of [['done','resolved'],['ambiguous','unknown']]) await store.save({requestId,phase,key:'fixture-key-12345',input:{outcome:'answered',text:'secret'},message:'secret',note:'secret'}); const old=new Date(Date.now()-8*86400000); for(const name of await readdir(dir)) await utimes(join(dir,name),old,old); await store.prune(); assert.equal(await store.load('done'),null); assert.equal((await store.load('ambiguous')).phase,'unknown'); }
 finally { for(const name of await readdir(dir)) await unlink(join(dir,name));await rmdir(dir); }
});
