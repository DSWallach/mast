import { snapshot } from './outline.js';
import { unary } from './reply-ipc.js';
import type { SpaceAccess } from './shared-spaces.js';
const MAX_BYTES = 4 * 1024 * 1024;
type Row = { id: string; spaceId: string; deviceId: string; title: string; kind: string; currentRevision: number; archivedAt?: number; revisions?: {id: number; sizeBytes: number}[] };
export class ArtifactReader {
  constructor(private readonly url: string, private readonly access: SpaceAccess) {}
  async find(id: string): Promise<Row | null> {
    for (const space of await this.access.spaces()) {
      const frame = await snapshot<{spaceId: string; artifacts: Row[]}>(this.url, 'WatchArtifacts', {spaceId: space.id});
      if (frame.spaceId !== space.id || !Array.isArray(frame.artifacts)) throw Error('invalid artifacts');
      const row = frame.artifacts.find(row => row.id === id && row.spaceId === space.id && !row.archivedAt);
      if (row && (await this.access.spaces()).some(s => s.id === space.id)) return row;
    }
    return null;
  }
  async content(row: Row): Promise<{ bytes: Buffer; mime: string }> {
    const revision = row.revisions?.find(r => r.id === row.currentRevision);
    if (revision && revision.sizeBytes > MAX_BYTES) throw Error('artifact too large');
    const result = await unary(this.url, 'ArtifactContent', {artifactId: row.id, targetDeviceId: row.deviceId});
    if (result.artifactId !== row.id || typeof result.content !== 'string' || !['base64','utf8'].includes(result.encoding) || typeof result.sizeBytes !== 'number' || result.sizeBytes > MAX_BYTES) throw Error('invalid artifact content');
    const bytes = Buffer.from(result.content, result.encoding === 'base64' ? 'base64' : 'utf8');
    if (bytes.length > MAX_BYTES || bytes.length !== result.sizeBytes) throw Error('invalid artifact size');
    const current = await this.find(row.id);
    if (!current || current.spaceId !== row.spaceId || current.deviceId !== row.deviceId) throw Error('artifact no longer shared');
    return {bytes, mime: typeof result.mimeType === 'string' ? result.mimeType : 'application/octet-stream'};
  }
}
