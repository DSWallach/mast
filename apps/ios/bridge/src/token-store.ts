import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { randomBytes } from "node:crypto";
import type { TokenStore } from "./server.js";

/** Credential hashes are stored separately from Mast's profile. */
export class FileTokenStore implements TokenStore {
  private hashes = new Set<string>();
  private constructor(private readonly directory: string) {}

  static async open(directory: string): Promise<FileTokenStore> {
    await mkdir(directory, { recursive: true, mode: 0o700 });
    const store = new FileTokenStore(directory);
    try {
      const parsed: unknown = JSON.parse(await readFile(join(directory, "tokens.json"), "utf8"));
      if (!Array.isArray(parsed) || !parsed.every(value => typeof value === "string" && /^[a-f0-9]{64}$/.test(value))) throw new Error("invalid token store");
      store.hashes = new Set(parsed);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
    return store;
  }

  async add(hash: string): Promise<void> {
    this.hashes.add(hash);
    try { await this.save(); } catch (error) { this.hashes.delete(hash); throw error; }
  }

  async contains(hash: string): Promise<boolean> {
    // Reload so revocation by a separate CLI process takes effect immediately.
    const current = await FileTokenStore.open(this.directory);
    return current.hashes.has(hash);
  }

  async revokeAll(): Promise<void> {
    const previous = this.hashes;
    this.hashes = new Set();
    try { await this.save(); } catch (error) { this.hashes = previous; throw error; }
  }

  private async save(): Promise<void> {
    const temporary = join(this.directory, `.tokens-${randomBytes(12).toString("hex")}`);
    await writeFile(temporary, JSON.stringify([...this.hashes]), { mode: 0o600, flag: "wx" });
    await rename(temporary, join(this.directory, "tokens.json"));
  }
}
