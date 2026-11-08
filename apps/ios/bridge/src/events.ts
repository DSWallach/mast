import WebSocket from "ws";
import type { ServerResponse } from "node:http";
import type { SpaceAccess } from "./shared-spaces.js";

/** Streams only invalidations, never raw engine frames or unshared identifiers. */
export async function streamEvents(response: ServerResponse, url: string, access: SpaceAccess, chatId?: string, authorized: () => Promise<boolean> = async () => true): Promise<void> {
  const spaces = await access.spaces();
  const ids = new Set(spaces.map(space => space.id));
  if (chatId && !(await access.chatAllowed(chatId))) { response.writeHead(403); response.end(); return; }
  response.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-store", "Connection": "keep-alive", "X-Accel-Buffering": "no" });
  response.write(": connected\n\n");
  let closed = false;
  const sockets: WebSocket[] = [];
  const close = () => { if (closed) return; closed = true; clearInterval(heartbeat); for (const ws of sockets) ws.terminate(); response.end(); };
  response.on("close", close);
  const heartbeat = setInterval(() => { void (async () => {
    if (!(await authorized())) return close();
    const current = await access.spaces();
    if (current.length !== ids.size || current.some(space => !ids.has(space.id)) || (chatId && !(await access.chatAllowed(chatId)))) return close();
    response.write(": heartbeat\n\n");
  })().catch(close); }, 5000);
  function watch(method: string, params: object, filter: (item: any) => Promise<unknown>, event: string) {
    const ws = new WebSocket(url, { maxPayload: 8 * 1024 * 1024 }); sockets.push(ws);
    let previous: string | undefined;
    let pending = Promise.resolve();
    ws.on("open", () => ws.send(JSON.stringify({ id: 1, method, params })));
    ws.on("message", bytes => { pending = pending.then(async () => {
      if (!(await authorized())) return close();
      const frame = JSON.parse(bytes.toString());
      if (frame.err || frame.done) return close();
      if (!("item" in frame)) return;
      const visible = await filter(frame.item);
      if (visible === undefined) return;
      const next = JSON.stringify(visible);
      if (previous !== undefined && previous !== next && !closed) response.write(`event: ${event}\ndata: {}\n\n`);
      previous = next;
    }).catch(close); });
    ws.on("error", close); ws.on("close", close);
  }
  watch("WatchChats", {}, async item => {
    if (!Array.isArray(item)) throw Error("invalid chats");
    const visible = []; for (const chat of item) if (typeof chat.id === "string" && ids.has(chat.spaceId) && await access.chatAllowed(chat.id)) visible.push(chat);
    return visible;
  }, "refresh");
  watch("WatchSessions", {}, async item => {
    if (!Array.isArray(item)) throw Error("invalid sessions");
    const visible = []; for (const session of item) if (typeof session.chatId === "string" && await access.chatAllowed(session.chatId)) visible.push(session);
    return visible;
  }, "refresh");
  for (const space of spaces) watch("WatchWorkOutline", { spaceId: space.id }, async item => (await access.spaces()).some(row => row.id === space.id) ? item : undefined, "refresh");
  if (chatId) watch("WatchDocMessages", { chatId }, async item => await access.chatAllowed(chatId) ? item : undefined, "transcript");
}
