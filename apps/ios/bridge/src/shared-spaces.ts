import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { randomBytes } from "node:crypto";
import { snapshot } from "./outline.js";
import { withStateLock } from "./state-lock.js";

export type Space = { id: string; name?: string; path: string };
export function spaceName(space: Space): string { return space.name?.trim() || space.path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || space.path; }
export type Capability = "view" | "answer" | "send";
export class SharedSpaces {
  constructor(private readonly directory: string) {}
  private async readIds(): Promise<Map<string, Capability>> {
    try {
      const value: unknown = JSON.parse(await readFile(join(this.directory, "shared-spaces.json"), "utf8"));
      if (!Array.isArray(value)) throw new Error("invalid shared spaces");
      const entries = new Map<string, Capability>();
      for (const entry of value) {
        const id = typeof entry === "string" ? entry : entry?.id, allow = typeof entry === "string" ? "view" : entry?.allow;
        if (typeof id !== "string" || !id.length || id.length > 256 || !["view", "answer", "send"].includes(allow) || entries.has(id)) throw new Error("invalid shared spaces");
        entries.set(id, allow);
      }
      return entries;
    } catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return new Map(); throw error; }
  }
  async ids(): Promise<Set<string>> { return new Set((await withStateLock(this.directory, "spaces", () => this.readIds())).keys()); }
  async allows(id: string, level: Capability): Promise<boolean> { const entries = await withStateLock(this.directory, "spaces", () => this.readIds()); const rank = {view: 0, answer: 1, send: 2}; const allow = entries.get(id); return allow !== undefined && rank[allow] >= rank[level]; }
  async set(id: string, share: boolean, allow: Capability = "view"): Promise<void> {
    await withStateLock(this.directory, "spaces", async () => {
      const ids = await this.readIds(); if (share) ids.set(id, allow); else ids.delete(id);
      await mkdir(this.directory, { recursive: true, mode: 0o700 });
      const temporary = join(this.directory, `.spaces-${randomBytes(12).toString("hex")}`);
      await writeFile(temporary, JSON.stringify([...ids].map(([id, allow]) => ({id, allow}))), { mode: 0o600, flag: "wx" });
      await rename(temporary, join(this.directory, "shared-spaces.json"));
    });
  }
}
export interface SpaceAccess {
  allows?(id: string, level: Capability): Promise<boolean>;
  spaces(): Promise<{ id: string; name: string }[]>;
  chatSpace(id: string): Promise<string | null>;
  requestSpace(id: string): Promise<string | null>;
  chatAllowed(id: string): Promise<boolean>;
  requestAllowed(id: string): Promise<boolean>;
}
export function engineSpaceAccess(url: string, shared: SharedSpaces): SpaceAccess {
  async function sharedSpaces() {
    const [spaces, ids] = await Promise.all([snapshot<Space[]>(url, "WatchSpaces", {}), shared.ids()]);
    if (!Array.isArray(spaces)) throw new Error("invalid spaces");
    return spaces.filter(space => ids.has(space.id));
  }
  async function chatSpace(id: string): Promise<string | null> {
      const spaces = await sharedSpaces(); if (!spaces.length) return null;
      const chats = await snapshot<{ id: string; spaceId?: string; archived?: boolean }[]>(url, "WatchChats", {});
      if (!Array.isArray(chats)) throw new Error("invalid chats");
      const chat = chats.find(value => value.id === id && !value.archived);
      return chat?.spaceId && spaces.some(space => space.id === chat.spaceId) && (await shared.ids()).has(chat.spaceId) ? chat.spaceId : null;
  }
  async function requestSpace(id: string): Promise<string | null> {
      const spaces = await sharedSpaces();
      for (const space of spaces) {
        const outline = await snapshot<{ available: boolean; synced: boolean; userActionRequests: { id: string }[] }>(url, "WatchWorkOutline", { spaceId: space.id });
        if (!outline.available || !outline.synced || !Array.isArray(outline.userActionRequests)) throw new Error("outline unavailable");
        if (outline.userActionRequests.some(row => row.id === id)) return (await shared.ids()).has(space.id) ? space.id : null;
      }
      return null;
  }
  return {
    async spaces() { return (await sharedSpaces()).map(space => ({ id: space.id, name: spaceName(space) })); },
    chatSpace, requestSpace,
    allows: (id, level) => shared.allows(id, level),
    async chatAllowed(id) { return Boolean(await chatSpace(id)); },
    async requestAllowed(id) { return Boolean(await requestSpace(id)); },
  };
}
