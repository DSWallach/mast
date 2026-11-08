import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import type { TokenStore, DeviceIdentity } from "./server.js";
import { withStateLock } from "./state-lock.js";

type DeviceRecord = DeviceIdentity & { hash: string };
export class FileTokenStore implements TokenStore {
  private constructor(private readonly directory: string) {}
  static async open(directory: string): Promise<FileTokenStore> { await mkdir(directory, { recursive: true, mode: 0o700 }); return new FileTokenStore(directory); }
  private async records(): Promise<DeviceRecord[]> {
    try {
      const values: unknown = JSON.parse(await readFile(join(this.directory, "tokens.json"), "utf8"));
      if (!Array.isArray(values)) throw new Error("invalid token store");
      return values.map(value => {
        if (typeof value === "string" && /^[a-f0-9]{64}$/.test(value)) return { hash: value, id: createHash("sha256").update(value).digest("hex").slice(0, 24), label: "Existing device" };
        if (!value || typeof value !== "object" || !/^[a-f0-9]{64}$/.test(value.hash) || !/^[a-f0-9]{24}$/.test(value.id) || typeof value.label !== "string" || !value.label || value.label.length > 80) throw new Error("invalid token store");
        return value as DeviceRecord;
      });
    } catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return []; throw error; }
  }
  async add(hash: string, label: string): Promise<DeviceIdentity> {
    if (!/^[a-f0-9]{64}$/.test(hash) || typeof label !== "string" || !label.trim() || label.length > 80 || /[\x00-\x1f\x7f]/.test(label)) throw new Error("invalid device");
    return withStateLock(this.directory, "tokens", async () => {
      const records = await this.records();
      const record = { hash, id: randomBytes(12).toString("hex"), label };
      records.push(record); await this.save(records); return { id: record.id, label: record.label };
    });
  }
  async authenticate(hash: string): Promise<DeviceIdentity | null> {
    const candidate = Buffer.from(hash, "hex"); if (candidate.length !== 32) return null;
    return withStateLock(this.directory, "tokens", async () => {
      let matched: DeviceIdentity | null = null;
      for (const record of await this.records()) if (timingSafeEqual(candidate, Buffer.from(record.hash, "hex"))) matched = { id: record.id, label: record.label };
      return matched;
    });
  }
  async contains(hash: string): Promise<boolean> { return Boolean(await this.authenticate(hash)); }
  async devices(): Promise<DeviceIdentity[]> { return (await this.records()).map(({ id, label }) => ({ id, label })); }
  async revoke(selector: string): Promise<DeviceIdentity> {
    return withStateLock(this.directory, "tokens", async () => {
      const records = await this.records(), matches = records.filter(record => record.id === selector || record.label === selector);
      if (matches.length !== 1) throw new Error("device not found or ambiguous");
      await this.save(records.filter(record => record.id !== matches[0].id));
      return { id: matches[0].id, label: matches[0].label };
    });
  }
  async revokeAll(): Promise<DeviceIdentity[]> {
    return withStateLock(this.directory, "tokens", async () => {
      const records = await this.records(); await this.save([]);
      return records.map(({ id, label }) => ({ id, label }));
    });
  }
  private async save(records: DeviceRecord[]): Promise<void> {
    const temporary = join(this.directory, `.tokens-${randomBytes(12).toString("hex")}`);
    await writeFile(temporary, JSON.stringify(records), { mode: 0o600, flag: "wx" });
    await rename(temporary, join(this.directory, "tokens.json"));
  }
}
