import WebSocket from "ws";
import { effectiveKind, snapshot } from "./outline.js";
import type { AnswerRow, Intent, ReplyEngine } from "./reply.js";
import type { SpaceAccess } from "./shared-spaces.js";

type UnaryMethod = "ArtifactContent" | "EngineInfo" | "ContinueChatForWorkItem" | "QueueMessage" | "WorkOutlineMutate";
export async function unary(url: string, method: UnaryMethod, params: object): Promise<any> {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url, { maxPayload: 8 * 1024 * 1024 });
    let finished = false;
    const finish = (error?: Error, value?: unknown) => {
      if (finished) return; finished = true; clearTimeout(timer);
      if (socket.readyState === WebSocket.CONNECTING) socket.terminate(); else socket.close();
      if (error) reject(error); else resolve(value);
    };
    const timer = setTimeout(() => finish(new Error("IPC timeout")), 10_000);
    socket.on("open", () => socket.send(JSON.stringify({ id: 1, method, params })));
    socket.on("message", bytes => {
      try { const frame = JSON.parse(bytes.toString()); if (frame.id !== 1) return; if (frame.err || frame.done) return finish(new Error("IPC operation failed")); if ("ok" in frame) finish(undefined, frame.ok); }
      catch { finish(new Error("invalid IPC frame")); }
    });
    socket.on("error", () => finish(new Error("IPC unavailable")));
    socket.on("close", () => finish(new Error("IPC closed")));
  });
}

export function ipcReplyEngine(url: string, access?: SpaceAccess): ReplyEngine {
  async function requireRequest(id: string) { if (access && !(await access.requestAllowed(id))) throw new Error("space not shared"); }
  return {
    async openRow(id): Promise<AnswerRow | null> {
      await requireRequest(id);
      const spaces = await snapshot<any[]>(url, "WatchSpaces", {});
      if (!Array.isArray(spaces)) throw new Error("invalid spaces");
      const allowed = access ? new Set((await access.spaces()).map(space => space.id)) : null;
      const snapshots = await Promise.all(spaces.filter(space => !allowed || allowed.has(space.id)).map(space => snapshot<any>(url, "WatchWorkOutline", { spaceId: space.id })));
      for (const outline of snapshots) {
        if (!outline.available || !outline.synced || !Array.isArray(outline.userActionRequests)) throw new Error("outline unavailable");
        const row = outline.userActionRequests.find((value: any) => value.id === id && !value.deduped);
        if (row) {
          const options = row.options ?? [];
          if (typeof row.text !== "string" || !Array.isArray(options) || !options.every((option: any) => typeof option.id === "string" && typeof option.label === "string")) throw new Error("invalid request snapshot");
          return { id: row.id, text: row.text, kind: effectiveKind(row), state: row.state ?? "open", chatId: row.chatId ?? "", system: row.originKind === "system", chatAvailable: row.originChatAvailable === true, options, resolution: row.resolution };
        }
      }
      return null;
    },
    async isBusy(chatId) {
      const sessions = await snapshot<any[]>(url, "WatchSessions", {});
      if (!Array.isArray(sessions)) throw new Error("invalid sessions");
      const session = sessions.find(value => value.chatId === chatId);
      if (session && ["working", "awaitingInput"].includes(session.status) && !Number.isFinite(Date.parse(session.updatedAt))) throw new Error("invalid session timestamp");
      const busy = session && ["working", "awaitingInput"].includes(session.status) && Date.now() - Date.parse(session.updatedAt) <= 45_000;
      if (!busy) return false;
      const [info, chats, devices] = await Promise.all([unary(url, "EngineInfo", {}), snapshot<any[]>(url, "WatchChats", {}), snapshot<any[]>(url, "WatchDevices", {})]);
      const chat = chats.find(value => value.id === chatId);
      const host = chat?.deviceId === info.deviceId ? info : devices.find(value => value.id === chat?.deviceId);
      if (!info.capabilities?.includes("message-queue-v1") || !host?.capabilities?.includes("message-queue-v1")) throw new Error("message queue unsupported");
      return true;
    },
    async sendIdle(intent, guard) {
      await requireRequest(intent.requestId);
      if (access && !(await access.chatAllowed(intent.chatId))) throw new Error("space not shared");
      if (guard) await guard();
      const params = { chatId: intent.chatId, prompt: { text: intent.message, attachments: [] }, operationId: intent.operationId };
      let reply = await unary(url, "ContinueChatForWorkItem", params);
      for (let i = 0; i < 6 && reply.state === "queued"; i++) { await new Promise(resolve => setTimeout(resolve, 500)); if (guard) await guard(); reply = await unary(url, "ContinueChatForWorkItem", params); }
      if (!reply || reply.state === "rejected" || !["queued", "started"].includes(reply.state)) throw new Error("reply not accepted");
    },
    async sendQueued(intent, guard) {
      await requireRequest(intent.requestId);
      if (access && !(await access.chatAllowed(intent.chatId))) throw new Error("space not shared");
      if (guard) await guard();
      const reply = await unary(url, "QueueMessage", { chatId: intent.chatId, text: intent.message, attachments: [], holdForTurnEnd: true });
      if (!reply?.id || typeof reply.id !== "string") throw new Error("missing queue acknowledgement");
    },
    async resolve(intent: Intent, operationId: string, guard) {
      await requireRequest(intent.requestId);
      if (guard) await guard();
      await unary(url, "WorkOutlineMutate", { op: "resolveUserActionRequest", userActionRequestId: intent.requestId, operationId, outcome: intent.outcome, values: { action: intent.outcome, ...(intent.optionId ? { optionId: intent.optionId } : {}) }, via: "card", ...(intent.note ? { note: intent.note } : {}) });
    },
  };
}
