import { appendFile, mkdir, readFile, readdir, rename, stat } from "node:fs/promises";
import { join } from "node:path";
import { createHash, randomBytes } from "node:crypto";
import { withStateLock } from "./state-lock.js";
export type AuditRecord = { timestamp: string; deviceId: string; action: string; targetId?: string; spaceId?: string; tailscaleLogin?: string; outcome: string };
export interface AuditLog { append(record: AuditRecord): Promise<void>; }
const LIMIT = 10 * 1024 * 1024;
const digest = (value: string) => createHash("sha256").update(value).digest("hex");
export class FileAuditLog implements AuditLog {
  constructor(private readonly directory: string, private readonly rotationLimit = LIMIT) {}
  async append(record: AuditRecord): Promise<void> {
    await mkdir(this.directory, { recursive: true, mode: 0o700 });
    const safe = { timestamp: record.timestamp, deviceId: record.deviceId, action: record.action, ...(record.targetId ? { targetId: record.targetId } : {}), ...(record.spaceId ? { spaceId: record.spaceId } : {}), ...(record.tailscaleLogin ? { tailscaleLogin: record.tailscaleLogin } : {}), outcome: record.outcome };
    await withStateLock(this.directory, "audit", async () => {
      const path = join(this.directory, "audit.jsonl"); let lines: string[] = [], size = 0;
      try { const bytes = await readFile(path, "utf8"); size = Buffer.byteLength(bytes); lines = bytes.split("\n").filter(Boolean); } catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
      let last = lines.at(-1); let previous = "0".repeat(64);
      if (!last) {
        const rotated = (await readdir(this.directory)).filter(name => /^audit-[0-9]+-[a-f0-9]{8}\.jsonl$/.test(name)).sort().at(-1);
        if (rotated) last = (await readFile(join(this.directory, rotated), "utf8")).split("\n").filter(Boolean).at(-1);
      }
      if (last) { const parsed = JSON.parse(last); previous = typeof parsed.hash === "string" && /^[a-f0-9]{64}$/.test(parsed.hash) ? parsed.hash : digest(last); }
      const payload = { ...safe, prevHash: previous }, hash = digest(JSON.stringify(payload)), line = JSON.stringify({...payload, hash}) + "\n";
      if (size && size + Buffer.byteLength(line) > this.rotationLimit) await rename(path, join(this.directory, `audit-${Date.now()}-${randomBytes(4).toString("hex")}.jsonl`));
      await appendFile(path, line, { mode: 0o600 });
    });
  }
  async tail(limit: number): Promise<string[]> {
    try {
      const names = (await readdir(this.directory)).filter(name => /^audit-[0-9]+-[a-f0-9]{8}\.jsonl$/.test(name)).sort(); names.push("audit.jsonl"); const output: string[] = [];
      for (const name of names.reverse()) { try { const lines = (await readFile(join(this.directory,name), "utf8")).split("\n").filter(Boolean); output.unshift(...lines.slice(-limit)); if (output.length >= limit) break; } catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; } }
      return output.slice(-limit);
    } catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return []; throw error; }
  }
}
