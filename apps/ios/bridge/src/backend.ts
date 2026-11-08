import { callChatTool } from "./mcp.js";
import { readRequests } from "./outline.js";
import type { BridgeBackend } from "./server.js";

type ChatTool = typeof callChatTool;

/** Only projected companion fields leave the bridge. */
export function engineBackend(binary: string, ipcUrl: string, call: ChatTool = callChatTool): BridgeBackend {
  return {
    requests: () => readRequests(ipcUrl),
    chats: async () => {
      const result = await call(binary, "list_chats", { limit: 500 });
      if (!Array.isArray(result.chats)) throw new Error("invalid chats");
      return result.chats.filter(chat => typeof chat.status === "string" && (chat.status.startsWith("working") || chat.status === "awaitingInput")).map(chat => ({
        id: chat.id, title: chat.title, space: chat.project?.name ?? "", status: chat.status,
      }));
    },
    transcript: async chatId => {
      const result = await call(binary, "read_chat", { chat: chatId, limit: 40, include_tools: false, include_reasoning: false });
      if (!Array.isArray(result.messages)) throw new Error("invalid transcript");
      return result.messages.filter(message => typeof message.text === "string" && message.text.length > 0).map(message => ({ id: message.id, role: message.role, text: message.text }));
    },
    send: async (chatId, text) => {
      const result = await call(binary, "send_message", { chat: chatId, text, mode: "auto", wait: false });
      const sent = result.sent as { delivery?: string } | undefined;
      if (!sent || !["run", "steer", "queue"].includes(sent.delivery ?? "")) throw new Error("unknown delivery");
      return { delivery: sent.delivery === "queue" ? "queued" : "accepted" };
    },
  };
}
