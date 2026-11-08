import { callChatTool } from "./mcp.js";
import { readRequests } from "./outline.js";
import type { BridgeBackend } from "./server.js";
import type { SpaceAccess } from "./shared-spaces.js";

type ChatTool = typeof callChatTool;
type Message = { id: string; role: string; text: string; tools?: string[] };
export function messageAuthor(message: Message): "user" | "agent" | "system" | "relay" {
  const text = message.text.trimStart();
  if (/^\[Message from Mast chat\b/i.test(text)) return "relay";
  if (/^(\[Mast system message\b|Worker updates:|\[?auto[- ]continue\b)/i.test(text)) return "system";
  if (/^Re:\s*["“]/i.test(text)) return "system";
  return message.role.toLowerCase() === "user" ? "user" : message.role.toLowerCase() === "assistant" ? "agent" : "system";
}

/** Only projected companion fields leave the bridge. */
export function engineBackend(binary: string, ipcUrl: string, call: ChatTool = callChatTool, requests: typeof readRequests = readRequests, access?: SpaceAccess): BridgeBackend {
  const cache = new Map<string, { revision: string; spoke: boolean }>();
  async function page(chatId: string, offset: number) {
    const result = await call(binary, "read_chat", { chat: chatId, limit: 40, offset, include_tools: true, include_reasoning: false });
    if (!Array.isArray(result.messages) || typeof result.olderRemaining !== "number" || typeof result.total !== "number") throw new Error("invalid transcript");
    const messages = result.messages.flatMap((message: Message) => {
      const rows = [];
      if (typeof message.text === "string" && message.text.length > 0) rows.push({ id: message.id, role: message.role, text: message.text, author: messageAuthor(message) });
      const tools = (message.tools ?? []).filter(line => typeof line === "string" && !line.startsWith("image:")).map(line => line.split(" → ")[0]);
      if (message.role === "assistant" && tools.length) rows.push({ id: `${message.id}:tools`, role: "tool", text: tools.join("\n"), author: "tool" });
      return rows;
    });
    return { messages, olderRemaining: result.olderRemaining, nextOffset: result.total - result.olderRemaining };
  }
  async function spoke(chat: { id: string; lastMessageAt?: string | null }): Promise<boolean> {
    const revision = chat.lastMessageAt ?? "";
    const prior = cache.get(chat.id);
    if (revision && prior && prior.revision === revision) return prior.spoke;
    let offset = 0;
    let found = false;
    for (;;) {
      const result = await page(chat.id, offset);
      if (result.messages.some((message: { author: string }) => message.author === "user")) { found = true; break; }
      if (result.olderRemaining <= 0) break;
      if (result.nextOffset <= offset) throw new Error("transcript pagination did not advance");
      offset = result.nextOffset;
    }
    if (cache.size >= 500) cache.delete(cache.keys().next().value!);
    if (revision) cache.set(chat.id, { revision, spoke: found });
    return found;
  }
  return {
    requests: ids => requests(ipcUrl, ids),
    chats: async ids => {
      const [result, openRequests] = await Promise.all([call(binary, "list_chats", { limit: 500 }), requests(ipcUrl, ids)]);
      if (!Array.isArray(result.chats)) throw new Error("invalid chats");
      const chats = result.chats.filter(chat => typeof chat.project?.id === "string" && ids.has(chat.project.id));
      const requestChats = new Set(openRequests.map((row: any) => row.originChatId).filter(Boolean));
      const output = new Array(chats.length);
      let index = 0;
      await Promise.all(Array.from({ length: Math.min(4, chats.length) }, async () => {
        while (index < chats.length) {
          const current = index++; const chat = chats[current];
          const hasRequest = requestChats.has(chat.id);
          const isWorker = Boolean(chat.parentChatId);
          const hasUserMessage = isWorker || hasRequest ? false : await spoke(chat);
          output[current] = { id: chat.id, title: chat.title, spaceId: chat.project.id, space: chat.project?.name ?? "", status: chat.status, harness: chat.harness ?? null, model: chat.model ?? null, lastMessageAt: chat.lastMessageAt ?? null, isWorker, hasUserMessage, hasOpenRequest: hasRequest };
        }
      }));
      return output;
    },
    transcript: page,
    send: async (chatId, text, spaceId) => {
      if (access && (!spaceId || (await access.chatSpace(chatId)) !== spaceId)) throw new Error("space not shared");
      const result = await call(binary, "send_message", { chat: chatId, text, mode: "auto", wait: false });
      const sent = result.sent as { delivery?: string } | undefined;
      if (!sent || !["run", "steer", "queue"].includes(sent.delivery ?? "")) throw new Error("unknown delivery");
      return { delivery: sent.delivery === "queue" ? "queued" : "accepted" };
    },
  };
}
