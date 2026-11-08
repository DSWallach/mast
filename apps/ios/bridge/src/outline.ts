import WebSocket from "ws";

type Space = { id: string; name?: string; path: string };
type Request = { id: string; state?: string; deduped?: boolean; text: string; info?: string; kind?: string; dedupeKey?: string; priority?: string; workItemId?: string; createdAt?: number; options?: { id: string; label: string; detail?: string }[]; facts?: { label: string; value: string }[]; links?: { label: string; target: Record<string, unknown> }[]; chatId?: string; originKind?: string };
type Snapshot = { available: boolean; synced: boolean; items?: { id: string; title: string }[]; userActionRequests: Request[] };
export function effectiveKind(row: { kind?: string; dedupeKey?: string; text: string }): string {
  if (row.kind) return ["ask", "choose", "approve", "review", "handoff", "notice"].includes(row.kind) ? row.kind : "ask";
  const key = row.dedupeKey ?? "";
  if (key.startsWith("admin-bulk:")) return "approve";
  if (key.startsWith("pr-review:")) return "review";
  if (key.startsWith("artopt:")) return "choose";
  if (key.startsWith("artq:")) return "ask";
  return row.text.trimStart().startsWith("Ready to merge") ? "handoff" : "ask";
}

/** One opening frame per subscription. The caller supplies a fixture URL in tests. */
export async function snapshot<T>(url: string, method: "WatchSpaces" | "WatchWorkOutline" | "WatchSessions" | "WatchChats" | "WatchDevices", params: object): Promise<T> {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url, { maxPayload: 8 * 1024 * 1024 });
    let complete = false;
    const finish = (error?: Error, item?: T) => {
      if (complete) return;
      complete = true;
      clearTimeout(timer);
      socket.close();
      if (error) reject(error); else resolve(item!);
    };
    const timer = setTimeout(() => { socket.terminate(); finish(new Error("IPC timeout")); }, 10_000);
    socket.on("open", () => socket.send(JSON.stringify({ id: 1, method, params })));
    socket.on("message", bytes => {
      try {
        const frame = JSON.parse(bytes.toString());
        if (frame.id !== 1) return;
        if (frame.err || frame.done) return finish(new Error("IPC snapshot unavailable"));
        if ("item" in frame) finish(undefined, frame.item as T);
      } catch { finish(new Error("invalid IPC frame")); }
    });
    socket.on("error", () => finish(new Error("IPC unavailable")));
    socket.on("close", () => finish(new Error("IPC closed")));
  });
}

export async function readRequests(url: string): Promise<unknown[]> {
  const spaces = await snapshot<Space[]>(url, "WatchSpaces", {});
  if (!Array.isArray(spaces)) throw new Error("invalid spaces");
  const results = await Promise.all(spaces.map(async space => {
    const outline = await snapshot<Snapshot>(url, "WatchWorkOutline", { spaceId: space.id });
    if (!outline.available || !outline.synced || !Array.isArray(outline.userActionRequests)) throw new Error("outline unavailable");
    const name = space.name?.trim() || space.path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || space.path;
    return outline.userActionRequests.filter(row => row.state !== "resolved" && !row.deduped).map(row => ({
      id: row.id, space: name, kind: effectiveKind(row), text: row.text, detail: row.info ?? "", priority: row.priority ?? null, item: outline.items?.find(item => item.id === row.workItemId)?.title ?? null, createdAt: row.createdAt ?? null,
      options: (row.options ?? []).map(option => option.detail ? `${option.label}: ${option.detail}` : option.label), optionChoices: row.options ?? [],
      facts: (row.facts ?? []).map(fact => `${fact.label}: ${fact.value}`),
      links: (row.links ?? []).map(link => `${link.label}: ${JSON.stringify(link.target)}`),
      origin: row.originKind === "system" ? "System" : (row.chatId ?? ""), originChatId: row.chatId ?? null,
    }));
  }));
  return results.flat();
}
