import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { PairingChallenge, tokenDigest } from "./pairing.js";
import type { ReplyCoordinator, AnswerInput } from "./reply.js";
import type { SpaceAccess } from "./shared-spaces.js";
import type { AuditLog } from "./audit.js";

export interface BridgeBackend {
  requests(ids: ReadonlySet<string>): Promise<unknown>;
  chats(ids: ReadonlySet<string>): Promise<unknown>;
  transcript(chatId: string, offset: number): Promise<unknown>;
  send(chatId: string, text: string, spaceId?: string): Promise<unknown>;
}

export type DeviceIdentity = { id: string; label: string };
export interface TokenStore { add(hash: string, label: string): Promise<DeviceIdentity>; authenticate(hash: string): Promise<DeviceIdentity | null>; }

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
export function createBridge(backend: BridgeBackend, tokens: TokenStore, challenge: PairingChallenge, replies?: ReplyCoordinator, access: SpaceAccess = denySpaces, audit: AuditLog = denyAudit) {
  return createServer(async (request, response) => {
    try {
      // Browsers cannot invoke the bridge from an unrelated origin.
      if (request.headers.origin) return reply(response, 403, { error: "origin forbidden" });
      const path = request.url ?? "";
      if (request.method === "POST" && path === "/pair") {
        if (request.headers["content-type"] !== "application/json") return reply(response, 415, { error: "json required" });
        const body = await readBody(request);
        if (typeof body.label !== "string" || !body.label.trim() || body.label.trim().length > 80 || /[\x00-\x1f\x7f]/.test(body.label)) return reply(response, 400, { error: "device label required" });
        const token = challenge.exchange(typeof body.code === "string" ? body.code : "", Date.now());
        if (!token) { await audit.append({ timestamp: new Date().toISOString(), deviceId: "unpaired", action: "pair", outcome: "denied" }); return reply(response, 401, { error: "pairing failed" }); }
        const device = await tokens.add(tokenDigest(token), body.label.trim());
        await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "pair", outcome: "paired" });
        return reply(response, 200, { token });
      }
      const authorization = request.headers.authorization ?? "";
      const match = /^Bearer ([A-Za-z0-9_-]{43})$/.exec(authorization);
      const device = match ? await tokens.authenticate(tokenDigest(match[1])) : null;
      if (!device) return reply(response, 401, { error: "unauthorized" });
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
}
