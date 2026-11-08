import { mkdir, rmdir, unlink, writeFile } from "node:fs/promises";
import { join } from "node:path";

/** Cross-process serialization for local state writes; stale locks fail closed. */
export async function withStateLock<T>(directory: string, name: string, operation: () => Promise<T>): Promise<T> {
  await mkdir(directory, { recursive: true, mode: 0o700 });
  const lock = join(directory, `.${name}.lock`), owner = join(lock, "owner");
  let acquired = false;
  for (let attempt = 0; attempt < 60; attempt++) {
    try { await mkdir(lock, { mode: 0o700 }); acquired = true; break; }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error; }
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  if (!acquired) throw new Error("state lock unavailable");
  try { await writeFile(owner, String(process.pid), { mode: 0o600, flag: "wx" }); return await operation(); }
  finally { try { await unlink(owner); } finally { await rmdir(lock); } }
}
