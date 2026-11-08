import { requireSupportedNode } from "./runtime.js";
import { ensureNoFunnel } from "./tailscale.js";
import { ArtifactReader } from "./artifacts.js";
import { readAppearance } from "./appearance.js";
import qr from "qrcode-terminal";
import { homedir } from "node:os";
import { join } from "node:path";
import { PairingChallenge, generatePairingCode } from "./pairing.js";
import { createBridge, type BridgeBackend } from "./server.js";
import { FileTokenStore } from "./token-store.js";
import { engineBackend } from "./backend.js";
import { ReplyCoordinator } from "./reply.js";
import { FileIntentStore } from "./reply-store.js";
import { ipcReplyEngine } from "./reply-ipc.js";
import { SharedSpaces, engineSpaceAccess, spaceName, type Space, type Capability } from "./shared-spaces.js";
import { FileAuditLog } from "./audit.js";
import { snapshot } from "./outline.js";

const directory = join(homedir(), "Library", "Application Support", "MastCompanionBridge");
const url = "ws://127.0.0.1:27654";
const command = process.argv[2];
if (command === "start") requireSupportedNode(process.versions.node);
const shared = new SharedSpaces(directory), audit = new FileAuditLog(directory);
if (command === "spaces") {
  const spaces = await snapshot<Space[]>(url, "WatchSpaces", {}), ids = await shared.ids();
  if (!Array.isArray(spaces)) throw new Error("invalid spaces");
  const action = process.argv[3], selector = process.argv[4];
  const allow = process.argv[5] === "--allow" ? process.argv[6] : "view";
  if (!["view", "answer", "send"].includes(allow) || (process.argv[5] && process.argv[5] !== "--allow") || (process.argv[5] && !process.argv[6]) || process.argv[7]) throw new Error("invalid capability");
  if (!action) for (const space of spaces) { const capability = !ids.has(space.id) ? "not shared" : await shared.allows(space.id, "send") ? "shared:send" : await shared.allows(space.id, "answer") ? "shared:answer" : "shared:view"; console.log(`${capability}\t${space.id}\t${spaceName(space)}\t${space.path}`); }
  else if ((action === "share" || action === "unshare") && selector) {
    const matches = spaces.filter(space => space.id === selector || spaceName(space) === selector);
    if (matches.length !== 1) throw new Error("space not found or ambiguous");
    await audit.append({ timestamp: new Date().toISOString(), deviceId: "mac", action: `spaces-${action}`, spaceId: matches[0].id, outcome: "started" });
    await shared.set(matches[0].id, action === "share", allow as Capability);
    await audit.append({ timestamp: new Date().toISOString(), deviceId: "mac", action: `spaces-${action}`, spaceId: matches[0].id, outcome: "completed" });
    console.log(`${spaceName(matches[0])}: ${action === "share" ? "shared" : "not shared"}`);
  } else throw new Error("Usage: spaces [share|unshare <id|name>]");
} else if (command === "devices") {
  for (const device of await (await FileTokenStore.open(directory)).devices()) console.log(`${device.id}\t${device.label}\tcreated=${new Date(device.createdAt).toISOString()}\tlastUsed=${new Date(device.lastUsedAt).toISOString()}\texpires=${new Date(device.expiresAt).toISOString()}`);
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
  if (command === "start") await ensureNoFunnel();
  const userIndex = process.argv.indexOf("--tailscale-user"), tailscaleUser = userIndex < 0 ? undefined : process.argv[userIndex + 1];
  if (userIndex >= 0 && (!tailscaleUser || tailscaleUser.startsWith("--") || /[\x00-\x20\x7f]/.test(tailscaleUser))) throw new Error("--tailscale-user requires a login");
  const pairingEnabled = command === "fixture" || process.argv.includes("--pair");
  const store = await FileTokenStore.open(directory), code = pairingEnabled ? generatePairingCode() : "";
  const urlIndex = process.argv.indexOf("--url"), publicUrl = urlIndex < 0 ? "http://127.0.0.1:27655" : process.argv[urlIndex + 1];
  if (!publicUrl) throw new Error("--url requires a bridge URL");
  const endpoint = new URL(publicUrl);
  if (!(endpoint.protocol === "https:" || (endpoint.protocol === "http:" && ["127.0.0.1", "localhost"].includes(endpoint.hostname))) || endpoint.username || endpoint.password || endpoint.search || endpoint.hash || (endpoint.pathname !== "/" && endpoint.pathname !== "")) throw new Error("URL must be HTTPS or loopback HTTP");
  const pairing = new URL("mast-companion://pair"); pairing.searchParams.set("url", endpoint.origin); pairing.searchParams.set("code", code);
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
  const server = createBridge(backend, store, new PairingChallenge(code, Date.now()), replies, access, audit, command === "start" ? url : undefined, command === "start" ? () => readAppearance(join(process.env.MAST_DATA_DIR || process.env.ZERON_DATA_DIR || join(homedir(), ".zeron"), "ui-settings.json")) : undefined, command === "start" ? new ArtifactReader(url, access) : undefined, 10, tailscaleUser, endpoint.host, pairingEnabled);
  server.listen(27655, "127.0.0.1", () => { console.log(`${command === "fixture" ? "Fixture" : "Mast"} bridge listening on 127.0.0.1:27655.`); if (pairingEnabled) { console.log("Pairing code expires in five minutes:"); console.log(code); qr.generate(pairing.toString(), { small: true }); } });
} else { console.error("Usage: node dist/cli.js start | fixture | spaces [share|unshare <id|name>] | devices | revoke <device|--all> | audit [count]"); process.exitCode = 1; }
