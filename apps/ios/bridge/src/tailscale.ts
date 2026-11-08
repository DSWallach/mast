import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
const executable = '/Applications/Tailscale.app/Contents/MacOS/Tailscale';
/** Tailscale ServeConfig's AllowFunnel maps host:port to booleans. */
export function funnelEnabled(value: unknown): boolean {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw Error('invalid Serve status');
  const inspect = (node: unknown): boolean => {
    if (!node || typeof node !== 'object') return false;
    if (Array.isArray(node)) return node.some(inspect);
    const object = node as Record<string, unknown>, map = object.AllowFunnel;
    if (map !== undefined && map !== null) {
      if (typeof map !== 'object' || Array.isArray(map) || Object.values(map).some(value => typeof value !== 'boolean')) throw Error('invalid Funnel status');
      if (Object.values(map).some(value => value === true)) return true;
    }
    return Object.entries(object).some(([key, child]) => key !== 'AllowFunnel' && inspect(child));
  };
  return inspect(value);
}
export async function ensureNoFunnel(): Promise<void> {
  const {stdout} = await promisify(execFile)(executable, ['serve', 'status', '--json'], {timeout: 5000, maxBuffer: 1024 * 1024});
  if (funnelEnabled(JSON.parse(stdout))) throw Error('Tailscale Funnel is enabled');
}
