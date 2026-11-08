import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { randomBytes, timingSafeEqual } from "node:crypto";
import type { TokenStore, DeviceIdentity } from "./server.js";
import { withStateLock } from "./state-lock.js";

const DAY = 86400000, ABSOLUTE = 30 * DAY, IDLE = 7 * DAY;
type DeviceRecord = DeviceIdentity & { hash: string; createdAt: number; lastUsedAt: number; expiresAt: number };
export class FileTokenStore implements TokenStore {
  private constructor(private readonly directory: string, private readonly now: () => number) {}
  static async open(directory: string, now: () => number = Date.now): Promise<FileTokenStore> {
    await mkdir(directory, { recursive: true, mode: 0o700 }); const store = new FileTokenStore(directory, now);
    await withStateLock(directory, "tokens", async () => { const { records, dirty } = await store.records(); if (dirty) await store.save(records); }); return store;
  }
  private async records(): Promise<{records: DeviceRecord[]; dirty: boolean}> {
    try {
      const values: unknown = JSON.parse(await readFile(join(this.directory, "tokens.json"), "utf8"));
      if (!Array.isArray(values)) throw new Error("invalid token store");
      const records: DeviceRecord[] = []; let dirty = false; const now = this.now();
      for (const value of values) {
        if (typeof value === "string" || (value && typeof value === "object" && !("createdAt" in value))) { dirty = true; continue; }
        if (!value || typeof value !== "object" || !/^[a-f0-9]{64}$/.test(value.hash) || !/^[a-f0-9]{24}$/.test(value.id) || typeof value.label !== "string" || !value.label || value.label.length > 80 || !Number.isSafeInteger(value.createdAt) || !Number.isSafeInteger(value.lastUsedAt) || !Number.isSafeInteger(value.expiresAt) || value.expiresAt !== value.createdAt + ABSOLUTE || value.lastUsedAt < value.createdAt || value.createdAt > now || value.lastUsedAt > now) throw new Error("invalid token store");
        if (now >= value.expiresAt || now - value.lastUsedAt >= IDLE) { dirty = true; continue; }
        records.push(value as DeviceRecord);
      }
      return { records, dirty };
    } catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return {records: [], dirty: false}; throw error; }
  }
  async add(hash: string, label: string): Promise<DeviceIdentity> {
    if (!/^[a-f0-9]{64}$/.test(hash) || typeof label !== "string" || !label.trim() || label.length > 80 || /[\x00-\x1f\x7f]/.test(label)) throw new Error("invalid device");
    return withStateLock(this.directory, "tokens", async () => {
      const {records} = await this.records(), now = this.now();
      const record = { hash, id: randomBytes(12).toString("hex"), label, createdAt: now, lastUsedAt: now, expiresAt: now + ABSOLUTE };
      records.push(record); await this.save(records); return { id: record.id, label: record.label };
    });
  }
  async authenticate(hash: string): Promise<DeviceIdentity | null> {
    const candidate = Buffer.from(hash, "hex"); if (candidate.length !== 32) return null;
    return withStateLock(this.directory, "tokens", async () => {
      const {records, dirty} = await this.records(); let matched: DeviceRecord | undefined;
      for (const record of records) if (timingSafeEqual(candidate, Buffer.from(record.hash, "hex"))) matched = record;
      if (matched) matched.lastUsedAt = this.now();
      if (matched || dirty) await this.save(records);
      return matched ? {id: matched.id, label: matched.label} : null;
    });
  }
  async contains(hash: string): Promise<boolean> { return Boolean(await this.authenticate(hash)); }
  async devices(): Promise<Array<DeviceIdentity & {createdAt: number; lastUsedAt: number; expiresAt: number}>> { return (await this.records()).records.map(({ id, label, createdAt, lastUsedAt, expiresAt }) => ({ id, label, createdAt, lastUsedAt, expiresAt })); }
  async revoke(selector: string): Promise<DeviceIdentity> {
    return withStateLock(this.directory, "tokens", async () => {
      const {records} = await this.records(), matches = records.filter(record => record.id === selector || record.label === selector);
      if (matches.length !== 1) throw new Error("device not found or ambiguous");
      await this.save(records.filter(record => record.id !== matches[0].id)); return { id: matches[0].id, label: matches[0].label };
    });
  }
  async revokeAll(): Promise<DeviceIdentity[]> { return withStateLock(this.directory, "tokens", async () => { const {records} = await this.records(); await this.save([]); return records.map(({ id, label }) => ({ id, label })); }); }
  private async save(records: DeviceRecord[]): Promise<void> { const temporary = join(this.directory, `.tokens-${randomBytes(12).toString("hex")}`); await writeFile(temporary, JSON.stringify(records), { mode: 0o600, flag: "wx" }); await rename(temporary, join(this.directory, "tokens.json")); }
}
