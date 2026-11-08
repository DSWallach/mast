import { homedir } from "node:os";
import { join } from "node:path";
import { PairingChallenge, generatePairingCode } from "./pairing.js";
import { createBridge, type BridgeBackend } from "./server.js";
import { FileTokenStore } from "./token-store.js";
import { engineBackend } from "./backend.js";
import { ReplyCoordinator } from "./reply.js";
import { FileIntentStore } from "./reply-store.js";
import { ipcReplyEngine } from "./reply-ipc.js";
import { SharedSpaces, engineSpaceAccess, spaceName, type Space } from "./shared-spaces.js";
import { FileAuditLog } from "./audit.js";
import { snapshot } from "./outline.js";

const directory = join(homedir(), "Library", "Application Support", "MastCompanionBridge");
const url = "ws://127.0.0.1:27654";
const command = process.argv[2];
const shared = new SharedSpaces(directory), audit = new FileAuditLog(directory);
if (command === "spaces") {
  const spaces = await snapshot<Space[]>(url, "WatchSpaces", {}), ids = await shared.ids();
  if (!Array.isArray(spaces)) throw new Error("invalid spaces");
  const action = process.argv[3], selector = process.argv[4];
  if (!action) for (const space of spaces) console.log(`${ids.has(space.id) ? "shared" : "not shared"}\t${space.id}\t${spaceName(space)}\t${space.path}`);
  else if ((action === "share" || action === "unshare") && selector) {
    const matches = spaces.filter(space => space.id === selector || spaceName(space) === selector);
    if (matches.length !== 1) throw new Error("space not found or ambiguous");
    await audit.append({ timestamp: new Date().toISOString(), deviceId: "mac", action: `spaces-${action}`, spaceId: matches[0].id, outcome: "started" });
    await shared.set(matches[0].id, action === "share");
    await audit.append({ timestamp: new Date().toISOString(), deviceId: "mac", action: `spaces-${action}`, spaceId: matches[0].id, outcome: "completed" });
    console.log(`${spaceName(matches[0])}: ${action === "share" ? "shared" : "not shared"}`);
  } else throw new Error("Usage: spaces [share|unshare <id|name>]");
} else if (command === "devices") {
  for (const device of await (await FileTokenStore.open(directory)).devices()) console.log(`${device.id}\t${device.label}`);
} else if (command === "revoke") {
  const selector = process.argv[3]; if (!selector) throw new Error("Usage: revoke <device-id|label|--all>");
  const store = await FileTokenStore.open(directory);
  await audit.append({ timestamp: new Date().toISOString(), deviceId: "mac", action: "revoke", outcome: "started" });
  const targets = selector === "--all" ? await store.revokeAll() : [await store.revoke(selector)];
  if (!targets.length) await audit.append({ timestamp: new Date().toISOString(), deviceId: "mac", action: "revoke-all", outcome: "completed" });
  for (const device of targets) await audit.append({ timestamp: new Date().toISOString(), deviceId: device.id, action: "revoke", outcome: "revoked" });
  console.log(`Revoked ${targets.length} device(s).`);
} else if (command === "audit") {
  const limit = process.argv[3] === undefined ? 20 : Number(process.argv[3]);
  if (!Number.isInteger(limit) || limit < 1 || limit > 1000) throw new Error("audit count must be 1–1000");
  for (const line of await audit.tail(limit)) console.log(line);
} else if (command === "fixture" || command === "start") {
  const store = await FileTokenStore.open(directory), code = generatePairingCode();
  const fixture: BridgeBackend = {
    requests: async () => [{ id: "fixture-request", spaceId: "fixture-space", space: "Mast", kind: "review", text: "Review the companion layout", detail: "Fixture request", options: [], facts: [], links: [], origin: "Fixture chat" }],
    chats: async () => [{ id: "fixture-chat", spaceId: "fixture-space", title: "Fixture chat", space: "Mast", status: "working" }],
    transcript: async () => ({ messages: [{ id: "fixture-message", role: "assistant", author: "agent", text: "Fixture transcript" }], olderRemaining: 0, nextOffset: 1 }),
    send: async () => ({ delivery: "simulated" }),
  };
  const access = command === "fixture" ? {
    spaces: async () => (await shared.ids()).has("fixture-space") ? [{ id: "fixture-space", name: "Fixture" }] : [],
    chatSpace: async (id: string) => id === "fixture-chat" && (await shared.ids()).has("fixture-space") ? "fixture-space" : null,
    requestSpace: async (id: string) => id === "fixture-request" && (await shared.ids()).has("fixture-space") ? "fixture-space" : null,
    chatAllowed: async (id: string) => id === "fixture-chat" && (await shared.ids()).has("fixture-space"),
    requestAllowed: async (id: string) => id === "fixture-request" && (await shared.ids()).has("fixture-space"),
  } : engineSpaceAccess(url, shared);
  const backend = command === "fixture" ? fixture : engineBackend("/Applications/Mast.app/Contents/MacOS/mast", url, undefined, undefined, access);
  const replies = command === "start" ? new ReplyCoordinator(new FileIntentStore(join(directory, "replies")), ipcReplyEngine(url, access)) : undefined;
  const server = createBridge(backend, store, new PairingChallenge(code, Date.now()), replies, access, audit, command === "start" ? url : undefined);
  server.listen(27655, "127.0.0.1", () => { console.log(`${command === "fixture" ? "Fixture" : "Mast"} bridge listening on 127.0.0.1:27655. Pairing code expires in five minutes:`); console.log(code); });
} else { console.error("Usage: node dist/cli.js start | fixture | spaces [share|unshare <id|name>] | devices | revoke <device|--all> | audit [count]"); process.exitCode = 1; }
