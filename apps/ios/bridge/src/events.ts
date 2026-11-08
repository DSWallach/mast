import WebSocket from "ws";
import type { ServerResponse } from "node:http";
import type { SpaceAccess } from "./shared-spaces.js";

/** Streams only invalidations, never raw engine frames or unshared identifiers. */
export async function streamEvents(response: ServerResponse, url: string, access: SpaceAccess, chatId?: string, authorized: () => Promise<boolean> = async () => true, appearance: () => Promise<unknown> = async () => null, onClose: () => void = () => {}): Promise<void> {
  const spaces = await access.spaces();
  const ids = new Set(spaces.map(space => space.id));
  if (chatId && !(await access.chatAllowed(chatId))) { response.writeHead(403); response.end(); onClose(); return; }
  response.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-store", "Connection": "keep-alive", "X-Accel-Buffering": "no" });
  response.write(": connected\n\n");
  let appearanceValue = JSON.stringify(await appearance());
  let closed = false;
  let chatSpaces = new Map<string, string>();
  const sockets: WebSocket[] = [];
  const close = () => { if (closed) return; closed = true; clearInterval(heartbeat); for (const ws of sockets) ws.terminate(); onClose(); response.end(); };
  response.on("close", close);
  const heartbeat = setInterval(() => { void (async () => {
    if (!(await authorized())) return close();
    const current = await access.spaces();
    if (current.length !== ids.size || current.some(space => !ids.has(space.id)) || (chatId && !(await access.chatAllowed(chatId)))) return close();
    const nextAppearance = JSON.stringify(await appearance());
    if (nextAppearance !== appearanceValue) { appearanceValue = nextAppearance; response.write("event: appearance\ndata: {}\n\n"); }
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
      if (previous !== undefined && (event === "transcript" || previous !== next) && !closed) response.write(`event: ${event}\ndata: {}\n\n`);
      previous = next;
    }).catch(close); });
    ws.on("error", close); ws.on("close", close);
  }
  watch("WatchChats", {}, async item => {
    if (!Array.isArray(item)) throw Error("invalid chats");
    const allowed = new Set((await access.spaces()).map(space => space.id));
    const visible = item.filter(chat => typeof chat.id === "string" && typeof chat.spaceId === "string" && ids.has(chat.spaceId) && allowed.has(chat.spaceId) && !chat.archived);
    chatSpaces = new Map(visible.map(chat => [chat.id, chat.spaceId])); return visible;
  }, "refresh");
  watch("WatchSessions", {}, async item => {
    if (!Array.isArray(item)) throw Error("invalid sessions");
    const allowed = new Set((await access.spaces()).map(space => space.id));
    if (access.chatSpaces) chatSpaces = await access.chatSpaces();
    return item.filter(session => typeof session.chatId === "string" && allowed.has(chatSpaces.get(session.chatId) ?? ""));
  }, "refresh");
  for (const space of spaces) watch("WatchWorkOutline", { spaceId: space.id }, async item => (await access.spaces()).some(row => row.id === space.id) ? item : undefined, "refresh");
  if (chatId) watch("WatchDocMessages", { chatId }, async item => await access.chatAllowed(chatId) ? item : undefined, "transcript");
}
