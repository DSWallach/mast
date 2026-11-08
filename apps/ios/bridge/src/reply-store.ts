import { createHash, randomBytes } from "node:crypto";
import { mkdir, open, readFile, rename, readdir, stat, unlink } from "node:fs/promises";
import { join } from "node:path";
import type { Intent, IntentStore } from "./reply.js";

/** One private, atomically replaced journal file per request. */
export class FileIntentStore implements IntentStore {
  constructor(private readonly directory: string) {}
  private path(id: string): string { return join(this.directory, createHash("sha256").update(id).digest("hex") + ".json"); }
  async load(id: string): Promise<Intent | null> {
    try {
      const parsed: unknown = JSON.parse(await readFile(this.path(id), "utf8"));
      if (!parsed || typeof parsed !== "object" || (parsed as Intent).requestId !== id || !["prepared", "idleDispatch", "queueDispatch", "sent", "resolving", "resolved", "unknown", "conflict"].includes((parsed as Intent).phase)) throw new Error("invalid reply journal");
      return parsed as Intent;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
      throw error;
    }
  }
  async prune(now = Date.now()): Promise<void> {
    let names: string[]; try { names = await readdir(this.directory); } catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return; throw error; }
    for (const name of names) {
      if (!/^[a-f0-9]{64}\.json$/.test(name)) continue;
      const path = join(this.directory, name), metadata = await stat(path);
      if (now - metadata.mtimeMs < 7 * 86400000) continue;
      const value = JSON.parse(await readFile(path, "utf8"));
      if (["resolved", "conflict"].includes(value.phase)) await unlink(path);
    }
  }
  async save(intent: Intent): Promise<void> {
    await mkdir(this.directory, { recursive: true, mode: 0o700 });
    const temporary = join(this.directory, `.reply-${randomBytes(16).toString("hex")}`);
    const terminal = ["resolved", "unknown", "conflict"].includes(intent.phase);
    const redacted = terminal && intent.input ? { ...intent, inputHash: intent.inputHash ?? createHash("sha256").update(JSON.stringify(intent.input)).digest("hex"), input: {outcome: intent.input.outcome}, message: "", note: "" } : intent;
    const file = await open(temporary, "wx", 0o600);
    try { await file.writeFile(JSON.stringify(redacted)); await file.sync(); } finally { await file.close(); }
    await rename(temporary, this.path(intent.requestId));
    const directory = await open(this.directory, "r");
    try { await directory.sync(); } finally { await directory.close(); }
  }
}
