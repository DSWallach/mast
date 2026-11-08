import type { ArtifactReader } from "./artifacts.js";
import { defaultAppearance, type Appearance } from "./appearance.js";
import { streamEvents } from "./events.js";
import { Readable } from "node:stream";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { PairingChallenge, tokenDigest } from "./pairing.js";
import type { ReplyCoordinator, AnswerInput } from "./reply.js";
import type { SpaceAccess } from "./shared-spaces.js";
import type { AuditLog } from "./audit.js";

export interface BridgeBackend {
  requests(ids: ReadonlySet<string>): Promise<unknown>;
  chats(ids: ReadonlySet<string>): Promise<unknown>;
  transcript(chatId: string, offset: number): Promise<unknown>;
  chat?(chatId: string, spaceId: string): Promise<unknown>;
  send(chatId: string, text: string, spaceId?: string): Promise<unknown>;
  interrupt?(chatId: string, spaceId: string): Promise<unknown>;
}

export type DeviceIdentity = { id: string; label: string };
export interface TokenStore { add(hash: string, label: string): Promise<DeviceIdentity>; authenticate(hash: string): Promise<DeviceIdentity | null>; revoke?(selector: string): Promise<DeviceIdentity>; }

async function readBody(request: IncomingMessage): Promise<Record<string, unknown>> {
  let body = "";
  for await (const chunk of request) {
    body += chunk.toString();
    if (Buffer.byteLength(body) > 16384) throw new Error("body too large");
  }
  const value: unknown = JSON.parse(body);
  if (!value || Array.isArray(value) || typeof value !== "object") throw new Error("invalid body");
  return value as Record<string, unknown>;
}

function reply(response: ServerResponse, status: number, body: unknown): void {
  response.writeHead(status, { "Content-Type": "application/json", "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff" });
  response.end(JSON.stringify(body));
}

/** This server exposes fixed operations only; no request can select an RPC method. */
const denySpaces: SpaceAccess = { spaces: async () => [], chatSpace: async () => null, requestSpace: async () => null, chatAllowed: async () => false, requestAllowed: async () => false };
const denyAudit: AuditLog = { append: async () => { throw new Error("audit unavailable"); } };
export function createBridge(backend: BridgeBackend, tokens: TokenStore, challenge: PairingChallenge, replies?: ReplyCoordinator, access: SpaceAccess = denySpaces, auditSink: AuditLog = denyAudit, eventsUrl?: string, appearance: () => Promise<Appearance> = async () => defaultAppearance, artifacts?: ArtifactReader, minuteLimit = 10, tailscaleUser?: string, publicHost?: string, pairingEnabled = true) {
  let pairDenials = 0;
  const flushPairDenials = () => {
    const count = pairDenials; pairDenials = 0;
    if (count) void auditSink.append({timestamp: new Date().toISOString(), deviceId: "unpaired", action: "pair-denials", outcome: `count:${count}`}).catch(() => { pairDenials += count; });
  };
  const denialTimer = setInterval(flushPairDenials, 60000); denialTimer.unref();
  const writes = new Map<string, number[]>();
  const streamCounts = new Map<string, number>(); let totalStreams = 0;
  const server = createServer(async (request, response) => {
    try {
      const host = request.headers.host?.toLowerCase(), port = request.socket.localPort;
      if (!host || ![`127.0.0.1:${port}`, `localhost:${port}`, ...(publicHost ? [publicHost.toLowerCase()] : [])].includes(host)) return reply(response, 403, { error: "host forbidden" });
      const login = request.headers["tailscale-user-login"];
      const audit: AuditLog = { append: record => auditSink.append({ ...record, ...(typeof login === "string" && login.length <= 256 ? { tailscaleLogin: login } : {}) }) };
      if (request.headers["tailscale-funnel-request"] !== undefined) return reply(response, 403, { error: "funnel forbidden" });
      if (tailscaleUser && (typeof login !== "string" || login !== tailscaleUser)) return reply(response, 403, { error: "identity forbidden" });
      // Browsers cannot invoke the bridge from an unrelated origin.
      if (request.headers.origin) return reply(response, 403, { error: "origin forbidden" });
      const path = request.url ?? "";
      if (request.method === "POST" && path === "/pair") {
        if (!pairingEnabled) return reply(response, 403, {error: "pairing closed"});
        if (request.headers["content-type"] !== "application/json") return reply(response, 415, { error: "json required" });
        const body = await readBody(request);
        if (typeof body.label !== "string" || !body.label.trim() || body.label.trim().length > 80 || /[\x00-\x1f\x7f]/.test(body.label)) return reply(response, 400, { error: "device label required" });
        const token = challenge.exchange(typeof body.code === "string" ? body.code : "", Date.now());
        if (!token) { pairDenials++; return reply(response, 401, { error: "pairing failed" }); }
        const device = await tokens.add(tokenDigest(token), body.label.trim());
        await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "pair", outcome: "paired" });
        return reply(response, 200, { token });
      }
      const authorization = request.headers.authorization ?? "";
      const match = /^Bearer ([A-Za-z0-9_-]{43})$/.exec(authorization);
      const device = match ? await tokens.authenticate(tokenDigest(match[1])) : null;
      if (!device) return reply(response, 401, { error: "unauthorized" });
      if (["POST", "DELETE"].includes(request.method ?? "")) {
        const now = Date.now(), recent = (writes.get(device.id) ?? []).filter(time => now - time < 86400000);
        if (recent.length >= 200 || recent.filter(time => now - time < 60000).length >= minuteLimit) { await audit.append({timestamp: new Date().toISOString(), deviceId: device.id, action: "write", outcome: "rate-limited"}); return reply(response, 429, {error: "rate limited"}); }
        recent.push(now); writes.set(device.id, recent);
      }
      if (request.method === "DELETE" && path === "/devices/self" && tokens.revoke) {
        await audit.append({timestamp: new Date().toISOString(), deviceId: device.id, action: "revoke-self", outcome: "started"});
        await tokens.revoke(device.id);
        await audit.append({timestamp: new Date().toISOString(), deviceId: device.id, action: "revoke-self", outcome: "revoked"});
        return reply(response, 200, {status: "revoked"});
      }
      const events = /^\/events(?:\?chat=([A-Za-z0-9_-]{1,128}))?$/.exec(path);
      if (request.method === "GET" && events && eventsUrl) {
        if (totalStreams >= 4 || (streamCounts.get(device.id) ?? 0) >= 2) return reply(response, 429, {error: "too many streams"});
        totalStreams++; streamCounts.set(device.id, (streamCounts.get(device.id) ?? 0) + 1); let released = false;
        const release = () => { if (released) return; released = true; totalStreams--; const count = (streamCounts.get(device.id) ?? 1) - 1; if (count) streamCounts.set(device.id, count); else streamCounts.delete(device.id); };
        try { await streamEvents(response, eventsUrl, access, events[1], async () => !!(await tokens.authenticate(tokenDigest(match![1]))), appearance, release); } catch (error) { release(); throw error; } return;
      }
      if (request.method === "GET" && path === "/appearance") return reply(response, 200, await appearance());
      if (request.method === "GET" && path === "/spaces") { const spaces = await access.spaces(); return reply(response, 200, { spaces, noSpacesShared: spaces.length === 0 }); }
      if (request.method === "GET" && (path === "/requests" || path === "/chats")) {
        const spaces = await access.spaces();
        if (!spaces.length) return reply(response, 200, { items: [], noSpacesShared: true });
        const ids = new Set(spaces.map(space => space.id));
        const items = path === "/requests" ? await backend.requests(ids) : await backend.chats(ids);
        if (!Array.isArray(items)) throw new Error("invalid projection");
        const current = new Set((await access.spaces()).map(space => space.id));
        const visible = [];
        for (const item of items) {
          if (!item || typeof item.id !== "string" || typeof item.spaceId !== "string" || !ids.has(item.spaceId) || !current.has(item.spaceId)) continue;
          if (path === "/requests" ? await access.requestAllowed(item.id) : await access.chatAllowed(item.id)) visible.push(item);
        }
        return reply(response, 200, { items: visible, noSpacesShared: current.size === 0 });
      }
      const artifact = /^\/artifacts\/([A-Za-z0-9_-]{1,128})(\/content)?$/.exec(path);
      if (request.method === "GET" && artifact && artifacts) {
        const row = await artifacts.find(artifact[1]);
        if (!row) { await audit.append({timestamp: new Date().toISOString(), deviceId: device.id, action: "artifact-read", targetId: artifact[1], outcome: "denied"}); return reply(response, 403, {error: "space not shared"}); }
        if (!artifact[2]) { await audit.append({timestamp: new Date().toISOString(), deviceId: device.id, action: "artifact-metadata", targetId: row.id, spaceId: row.spaceId, outcome: "read"}); return reply(response, 200, {id: row.id, title: row.title, kind: row.kind, spaceId: row.spaceId, revision: row.currentRevision}); }
        let result;
        try { result = await artifacts.content(row); }
        catch (error) { await audit.append({timestamp: new Date().toISOString(), deviceId: device.id, action: "artifact-content", targetId: row.id, spaceId: row.spaceId, outcome: "error-or-denied"}); throw error; }
        await audit.append({timestamp: new Date().toISOString(), deviceId: device.id, action: "artifact-content", targetId: row.id, spaceId: row.spaceId, outcome: "read"});
        response.writeHead(200, {"Content-Type": "application/octet-stream", "Content-Length": result.bytes.length, "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff", "X-Artifact-Mime": result.mime});
        const bytes = result.bytes;
        Readable.from((function* () { for (let offset = 0; offset < bytes.length; offset += 65536) yield bytes.subarray(offset, offset + 65536); })()).pipe(response); return;
      }
      const requestDetail = /^\/requests\/([A-Za-z0-9_.:@-]{1,128})$/.exec(path);
      if (request.method === "GET" && requestDetail) {
        const spaceId = await access.requestSpace(requestDetail[1]);
        if (!spaceId) return reply(response, 403, { error: "space not shared", noSpacesShared: (await access.spaces()).length === 0 });
        const items = await backend.requests(new Set([spaceId]));
        if (!Array.isArray(items)) throw new Error("invalid projection");
        const item = items.find(value => value?.id === requestDetail[1] && value.spaceId === spaceId);
        if ((await access.requestSpace(requestDetail[1])) !== spaceId) return reply(response, 403, { error: "space not shared" });
        return item ? reply(response, 200, item) : reply(response, 404, { error: "not found" });
      }
      const answer = /^\/requests\/([A-Za-z0-9_.:@/-]{1,128})\/(answer|resolve)$/.exec(path);
      if (request.method === "POST" && answer && replies) {
        const spaceId = await access.requestSpace(answer[1]);
        if (!spaceId) { await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: answer[2], targetId: answer[1], outcome: "denied" }); return reply(response, 403, { error: "space not shared", noSpacesShared: (await access.spaces()).length === 0 }); }
        if (!access.allows || !(await access.allows(spaceId, "answer"))) { await audit.append({timestamp: new Date().toISOString(), deviceId: device.id, action: answer[2], targetId: answer[1], spaceId, outcome: "denied"}); return reply(response, 403, {error: "capability denied"}); }
        if (request.headers["content-type"] !== "application/json") return reply(response, 415, { error: "json required" });
        const key = request.headers["idempotency-key"];
        if (typeof key !== "string") return reply(response, 400, { error: "idempotency key required" });
        const body = await readBody(request);
        if (answer[2] === "answer") {
          if (typeof body.outcome !== "string" || Object.keys(body).some(key => !["outcome", "text", "optionId"].includes(key)) || (body.text !== undefined && typeof body.text !== "string") || (body.optionId !== undefined && typeof body.optionId !== "string")) return reply(response, 400, { error: "invalid decision" });
          const input: AnswerInput = { outcome: body.outcome, ...(body.text === undefined ? {} : { text: body.text as string }), ...(body.optionId === undefined ? {} : { optionId: body.optionId as string }) };
          await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "answer", targetId: answer[1], spaceId, outcome: "started" });
          let result;
          try { result = await replies.answer(answer[1], key, input, true); } catch (error) { await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "answer", targetId: answer[1], spaceId, outcome: "error-or-unknown" }); throw error; }
          await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "answer", targetId: answer[1], spaceId, outcome: result.phase });
          return reply(response, 200, { state: result.phase, resolveOnly: result.resolveOnly });
        }
        if (Object.keys(body).length) return reply(response, 400, { error: "invalid retry" });
        await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "resolve", targetId: answer[1], spaceId, outcome: "started" });
        let result;
        try { result = await replies.retryResolve(answer[1], key); } catch (error) { await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "resolve", targetId: answer[1], spaceId, outcome: "error-or-unknown" }); throw error; }
        await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "resolve", targetId: answer[1], spaceId, outcome: result.phase });
        return reply(response, 200, { state: result.phase, resolveOnly: result.resolveOnly });
      }
      const stop = /^\/chats\/([A-Za-z0-9_-]{1,128})\/stop$/.exec(path);
      if (request.method === "POST" && stop && backend.interrupt) {
        const spaceId = await access.chatSpace(stop[1]);
        if (!spaceId) { await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "stop", targetId: stop[1], outcome: "denied" }); return reply(response, 403, { error: "space not shared" }); }
        if (!access.allows || !(await access.allows(spaceId, "send"))) { await audit.append({timestamp: new Date().toISOString(), deviceId: device.id, action: "stop", targetId: stop[1], spaceId, outcome: "denied"}); return reply(response, 403, {error: "capability denied"}); }
        if (request.headers["content-type"] !== "application/json" || Object.keys(await readBody(request)).length) return reply(response, 400, { error: "invalid request" });
        await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "stop", targetId: stop[1], spaceId, outcome: "started" });
        try { await backend.interrupt(stop[1], spaceId); } catch (error) { await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "stop", targetId: stop[1], spaceId, outcome: "error-or-unknown" }); throw error; }
        await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "stop", targetId: stop[1], spaceId, outcome: "accepted" });
        return reply(response, 200, { status: "accepted" });
      }
      const chatDetail = /^\/chats\/([A-Za-z0-9_-]{1,128})$/.exec(path);
      if (request.method === "GET" && chatDetail && backend.chat) {
        const spaceId = await access.chatSpace(chatDetail[1]); if (!spaceId) return reply(response, 403, {error: "space not shared"});
        const result = await backend.chat(chatDetail[1], spaceId);
        if ((await access.chatSpace(chatDetail[1])) !== spaceId) return reply(response, 403, {error: "space not shared"});
        return reply(response, 200, result);
      }
      const transcript = /^\/chats\/([A-Za-z0-9_-]{1,128})\/transcript(?:\?offset=(\d{1,7}))?$/.exec(path);
      if (request.method === "GET" && transcript) {
        if (!(await access.chatAllowed(transcript[1]))) return reply(response, 403, { error: "space not shared", noSpacesShared: (await access.spaces()).length === 0 });
        const result = await backend.transcript(transcript[1], Number(transcript[2] ?? 0));
        if (!(await access.chatAllowed(transcript[1]))) return reply(response, 403, { error: "space not shared" });
        return reply(response, 200, result);
      }
      const message = /^\/chats\/([A-Za-z0-9_-]{1,128})\/messages$/.exec(path);
      if (request.method === "POST" && message) {
        const spaceId = await access.chatSpace(message[1]);
        if (!spaceId) { await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "send", targetId: message[1], outcome: "denied" }); return reply(response, 403, { error: "space not shared", noSpacesShared: (await access.spaces()).length === 0 }); }
        if (!access.allows || !(await access.allows(spaceId, "send"))) { await audit.append({timestamp: new Date().toISOString(), deviceId: device.id, action: "send", targetId: message[1], spaceId, outcome: "denied"}); return reply(response, 403, {error: "capability denied"}); }
        if (request.headers["content-type"] !== "application/json") return reply(response, 415, { error: "json required" });
        const body = await readBody(request);
        if (typeof body.text !== "string" || !body.text.trim() || body.text.length > 12000) return reply(response, 400, { error: "invalid text" });
        await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "send", targetId: message[1], spaceId, outcome: "started" });
        if ((await access.chatSpace(message[1])) !== spaceId) { await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "send", targetId: message[1], spaceId, outcome: "denied" }); return reply(response, 403, { error: "space not shared" }); }
        let result;
        try { result = await backend.send(message[1], body.text, spaceId); } catch (error) { await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "send", targetId: message[1], spaceId, outcome: "error-or-unknown" }); throw error; }
        const delivery = (result && typeof result === "object" && "delivery" in result && (result.delivery === "queued" || result.delivery === "accepted")) ? result.delivery : "completed";
        await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "send", targetId: message[1], spaceId, outcome: delivery });
        return reply(response, 200, result);
      }
      return reply(response, 404, { error: "not found" });
    } catch {
      // Never log request contents, credentials, or upstream errors.
      return reply(response, 503, { error: "bridge operation failed" });
    }
  });
  server.once("close", () => { clearInterval(denialTimer); flushPairDenials(); });
  return server;
}
