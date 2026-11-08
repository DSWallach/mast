import WebSocket from "ws";
import { snapshot } from "./outline.js";
import type { AnswerRow, Intent, ReplyEngine } from "./reply.js";

type UnaryMethod = "EngineInfo" | "ContinueChatForWorkItem" | "QueueMessage" | "WorkOutlineMutate";
async function unary(url: string, method: UnaryMethod, params: object): Promise<any> {
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

export function ipcReplyEngine(url: string): ReplyEngine {
  return {
    async openRow(id): Promise<AnswerRow | null> {
      const spaces = await snapshot<any[]>(url, "WatchSpaces", {});
      if (!Array.isArray(spaces)) throw new Error("invalid spaces");
      const snapshots = await Promise.all(spaces.map(space => snapshot<any>(url, "WatchWorkOutline", { spaceId: space.id })));
      for (const outline of snapshots) {
        if (!outline.available || !outline.synced || !Array.isArray(outline.userActionRequests)) throw new Error("outline unavailable");
        const row = outline.userActionRequests.find((value: any) => value.id === id && !value.deduped);
        if (row) return { id: row.id, text: row.text, kind: row.kind ?? "ask", state: row.state ?? "open", chatId: row.chatId ?? "", system: row.originKind === "system", chatAvailable: row.originChatAvailable === true, options: row.options ?? [] };
      }
      return null;
    },
    async isBusy(chatId) {
      const sessions = await snapshot<any[]>(url, "WatchSessions", {});
      if (!Array.isArray(sessions)) throw new Error("invalid sessions");
      const session = sessions.find(value => value.chatId === chatId);
      const busy = session && ["working", "awaitingInput"].includes(session.status) && Date.now() - Date.parse(session.updatedAt) <= 45_000;
      if (!busy) return false;
      const [info, chats, devices] = await Promise.all([unary(url, "EngineInfo", {}), snapshot<any[]>(url, "WatchChats", {}), snapshot<any[]>(url, "WatchDevices", {})]);
      const chat = chats.find(value => value.id === chatId);
      const host = chat?.deviceId === info.deviceId ? info : devices.find(value => value.id === chat?.deviceId);
      if (!info.capabilities?.includes("message-queue-v1") || !host?.capabilities?.includes("message-queue-v1")) throw new Error("message queue unsupported");
      return true;
    },
    async sendIdle(intent) {
      const params = { chatId: intent.chatId, prompt: { text: intent.message, attachments: [] }, operationId: intent.operationId };
      let reply = await unary(url, "ContinueChatForWorkItem", params);
      for (let i = 0; i < 6 && reply.state === "queued"; i++) { await new Promise(resolve => setTimeout(resolve, 500)); reply = await unary(url, "ContinueChatForWorkItem", params); }
      if (!reply || reply.state === "rejected" || !["queued", "started"].includes(reply.state)) throw new Error("reply not accepted");
    },
    async sendQueued(intent) {
      const reply = await unary(url, "QueueMessage", { chatId: intent.chatId, text: intent.message, attachments: [], holdForTurnEnd: true });
      if (!reply?.id || typeof reply.id !== "string") throw new Error("missing queue acknowledgement");
    },
    async resolve(intent: Intent, operationId: string) {
      await unary(url, "WorkOutlineMutate", { op: "resolveUserActionRequest", userActionRequestId: intent.requestId, operationId, outcome: intent.outcome, values: { action: intent.outcome, ...(intent.optionId ? { optionId: intent.optionId } : {}) }, via: "card", ...(intent.note ? { note: intent.note } : {}) });
    },
  };
}
