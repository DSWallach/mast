import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { PairingChallenge, tokenDigest } from "./pairing.js";

export interface BridgeBackend {
  requests(): Promise<unknown>;
  chats(): Promise<unknown>;
  transcript(chatId: string): Promise<unknown>;
  send(chatId: string, text: string): Promise<unknown>;
}

export interface TokenStore {
  add(hash: string): Promise<void>;
  contains(hash: string): Promise<boolean>;
}

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
export function createBridge(backend: BridgeBackend, tokens: TokenStore, challenge: PairingChallenge) {
  return createServer(async (request, response) => {
    try {
      // Browsers cannot invoke the bridge from an unrelated origin.
      if (request.headers.origin) return reply(response, 403, { error: "origin forbidden" });
      const path = request.url ?? "";
      if (request.method === "POST" && path === "/pair") {
        if (request.headers["content-type"] !== "application/json") return reply(response, 415, { error: "json required" });
        const body = await readBody(request);
        const token = challenge.exchange(typeof body.code === "string" ? body.code : "", Date.now());
        if (!token) return reply(response, 401, { error: "pairing failed" });
        await tokens.add(tokenDigest(token));
        return reply(response, 200, { token });
      }
      const authorization = request.headers.authorization ?? "";
      const match = /^Bearer ([A-Za-z0-9_-]{43})$/.exec(authorization);
      if (!match || !(await tokens.contains(tokenDigest(match[1])))) return reply(response, 401, { error: "unauthorized" });
      if (request.method === "GET" && path === "/requests") return reply(response, 200, await backend.requests());
      if (request.method === "GET" && path === "/chats") return reply(response, 200, await backend.chats());
      const transcript = /^\/chats\/([A-Za-z0-9_-]{1,128})\/transcript$/.exec(path);
      if (request.method === "GET" && transcript) return reply(response, 200, await backend.transcript(transcript[1]));
      const message = /^\/chats\/([A-Za-z0-9_-]{1,128})\/messages$/.exec(path);
      if (request.method === "POST" && message) {
        if (request.headers["content-type"] !== "application/json") return reply(response, 415, { error: "json required" });
        const body = await readBody(request);
        if (typeof body.text !== "string" || !body.text.trim() || body.text.length > 12000) return reply(response, 400, { error: "invalid text" });
        return reply(response, 200, await backend.send(message[1], body.text));
      }
      return reply(response, 404, { error: "not found" });
    } catch {
      // Never log request contents, credentials, or upstream errors.
      return reply(response, 503, { error: "bridge operation failed" });
    }
  });
}
