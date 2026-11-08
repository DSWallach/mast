import { mkdir, rmdir, unlink, writeFile, readFile } from "node:fs/promises";
import { join } from "node:path";

/** Cross-process serialization for local state writes; stale locks fail closed. */
export async function withStateLock<T>(directory: string, name: string, operation: () => Promise<T>): Promise<T> {
  await mkdir(directory, { recursive: true, mode: 0o700 });
  const lock = join(directory, `.${name}.lock`), owner = join(lock, "owner");
  let acquired = false;
  for (let attempt = 0; attempt < 60; attempt++) {
    try { await mkdir(lock, { mode: 0o700 }); acquired = true; break; }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error; }
    // Serialize recovery so another contender cannot remove a newly acquired lock.
    const recovery = join(directory, `.${name}.recovery`);
    let recovering = false;
    try {
      await mkdir(recovery, { mode: 0o700 }); recovering = true;
      const text = await readFile(owner, "utf8"), pid = Number(text);
      if (/^[1-9][0-9]*$/.test(text) && Number.isSafeInteger(pid) && pid > 1) {
        try { process.kill(pid, 0); }
        catch (error) { if ((error as NodeJS.ErrnoException).code === "ESRCH") { try { await unlink(owner); await rmdir(lock); } catch {} } }
      }
    } catch (error) { if (!["ENOENT", "EEXIST"].includes((error as NodeJS.ErrnoException).code ?? "")) throw error; }
    finally { if (recovering) await rmdir(recovery); }
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  if (!acquired) throw new Error("state lock unavailable");
  try { await writeFile(owner, String(process.pid), { mode: 0o600, flag: "wx" }); return await operation(); }
  finally { try { await unlink(owner); } finally { await rmdir(lock); } }
}
