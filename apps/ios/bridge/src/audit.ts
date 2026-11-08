import { appendFile, mkdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { withStateLock } from "./state-lock.js";
export type AuditRecord = { timestamp: string; deviceId: string; action: string; targetId?: string; spaceId?: string; outcome: string };
export interface AuditLog { append(record: AuditRecord): Promise<void>; }
export class FileAuditLog implements AuditLog {
  constructor(private readonly directory: string) {}
  async append(record: AuditRecord): Promise<void> {
    await mkdir(this.directory, { recursive: true, mode: 0o700 });
    const safe = { timestamp: record.timestamp, deviceId: record.deviceId, action: record.action, ...(record.targetId ? { targetId: record.targetId } : {}), ...(record.spaceId ? { spaceId: record.spaceId } : {}), outcome: record.outcome };
    await withStateLock(this.directory, "audit", async () => { await appendFile(join(this.directory, "audit.jsonl"), JSON.stringify(safe) + "\n", { mode: 0o600 }); });
  }
  async tail(limit: number): Promise<string[]> {
    try { return (await readFile(join(this.directory, "audit.jsonl"), "utf8")).split("\n").filter(Boolean).slice(-limit); }
    catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return []; throw error; }
  }
}
