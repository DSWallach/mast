import { chmod, lstat, mkdir, readdir } from 'node:fs/promises';
import { join } from 'node:path';
/** Refuse unsafe ownership or writable state; tighten otherwise permissive modes. */
export async function ensureStatePermissions(directory: string): Promise<void> {
  await mkdir(directory, {recursive: true, mode: 0o700});
  const uid = process.getuid?.(); if (uid === undefined) throw Error('cannot verify state ownership');
  async function inspect(path: string): Promise<void> {
    const info = await lstat(path);
    if (info.isSymbolicLink() || info.uid !== uid || (info.mode & 0o022) !== 0 || (!info.isDirectory() && !info.isFile())) throw Error('unsafe bridge state ownership or permissions');
    if (info.isDirectory()) { await chmod(path, 0o700); for (const name of await readdir(path)) await inspect(join(path, name)); }
    else await chmod(path, 0o600);
  }
  await inspect(directory);
}
