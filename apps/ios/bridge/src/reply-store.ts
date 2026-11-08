import { createHash, randomBytes } from "node:crypto";
import { mkdir, open, readFile, rename } from "node:fs/promises";
import { join } from "node:path";
import type { Intent, IntentStore } from "./reply.js";

/** One private, atomically replaced journal file per request. */
export class FileIntentStore implements IntentStore {
  constructor(private readonly directory: string) {}
  private path(id: string): string { return join(this.directory, createHash("sha256").update(id).digest("hex") + ".json"); }
  async load(id: string): Promise<Intent | null> {
    try {
      const parsed: unknown = JSON.parse(await readFile(this.path(id), "utf8"));
      if (!parsed || typeof parsed !== "object" || (parsed as Intent).requestId !== id || !["prepared", "idleDispatch", "queueDispatch", "sent", "resolved", "unknown"].includes((parsed as Intent).phase)) throw new Error("invalid reply journal");
      return parsed as Intent;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
      throw error;
    }
  }
  async save(intent: Intent): Promise<void> {
    await mkdir(this.directory, { recursive: true, mode: 0o700 });
    const temporary = join(this.directory, `.reply-${randomBytes(16).toString("hex")}`);
    const file = await open(temporary, "wx", 0o600);
    try { await file.writeFile(JSON.stringify(intent)); await file.sync(); } finally { await file.close(); }
    await rename(temporary, this.path(intent.requestId));
    const directory = await open(this.directory, "r");
    try { await directory.sync(); } finally { await directory.close(); }
  }
}
