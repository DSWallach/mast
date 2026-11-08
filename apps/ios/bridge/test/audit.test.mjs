import { test } from "node:test";
import { createHash } from "node:crypto";
import assert from "node:assert/strict";
import { mkdtemp, readdir, stat, unlink, rmdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { FileAuditLog } from "../dist/audit.js";
test("audit persists only metadata, with private permissions and bounded tail", async () => {
  const directory = await mkdtemp(join(tmpdir(), "mast-audit-test-"));
  try {
    const audit = new FileAuditLog(directory);
    await audit.append({ timestamp: "2026-01-01T00:00:00Z", deviceId: "device", action: "send", targetId: "chat", spaceId: "space", outcome: "accepted", text: "secret content", token: "secret token" });
    await audit.append({ timestamp: "2026-01-01T00:00:01Z", deviceId: "device", action: "revoke", outcome: "revoked" });
    const tail = await audit.tail(2), text = tail.join("\n");
    assert.equal(tail.length, 2); assert.equal(text.includes("secret"), false);
    assert.equal(JSON.parse(tail[0]).spaceId, "space");
    assert.equal((await stat(join(directory, "audit.jsonl"))).mode & 0o777, 0o600);
    assert.equal((await audit.tail(1)).length, 1);
  } finally { for (const name of await readdir(directory)) await unlink(join(directory, name)); await rmdir(directory); }
});

test('audit records form a verifiable chain', async () => {
 const dir=await mkdtemp(join(tmpdir(),'mast-audit-chain-')); try { const log=new FileAuditLog(dir, 1); await log.append({timestamp:'2026-01-01',deviceId:'a',action:'send',outcome:'started'}); await log.append({timestamp:'2026-01-02',deviceId:'a',action:'send',outcome:'accepted'}); const lines=(await log.tail(2)).map(JSON.parse); assert.ok((await readdir(dir)).some(name=>name.startsWith('audit-'))); assert.equal(lines[0].prevHash,'0'.repeat(64)); assert.equal(lines[1].prevHash,lines[0].hash); const {hash,...payload}=lines[1]; assert.equal(hash,createHash('sha256').update(JSON.stringify(payload)).digest('hex')); }
 finally {for(const name of await readdir(dir))await unlink(join(dir,name));await rmdir(dir)}
});
