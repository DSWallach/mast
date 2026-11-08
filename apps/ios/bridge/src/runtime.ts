export function requireSupportedNode(version: string): void {
  const match = /^(\d+)\./.exec(version);
  if (!match || Number(match[1]) < 22) throw new Error('Node 22 or newer is required');
}
