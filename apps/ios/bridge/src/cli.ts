import { homedir } from "node:os";
import { join } from "node:path";
import { PairingChallenge, generatePairingCode } from "./pairing.js";
import { createBridge, type BridgeBackend } from "./server.js";
import { FileTokenStore } from "./token-store.js";
import { engineBackend } from "./backend.js";

const directory = join(homedir(), "Library", "Application Support", "MastCompanionBridge");
const command = process.argv[2];
if (command === "revoke") {
  await (await FileTokenStore.open(directory)).revokeAll();
  console.log("All companion device credentials revoked.");
} else if (command === "fixture" || command === "start") {
  const store = await FileTokenStore.open(directory);
  const code = generatePairingCode();
  const fixture: BridgeBackend = {
    requests: async () => [{ id: "fixture-request", space: "Mast", kind: "review", text: "Review the companion layout", detail: "Fixture request", options: [], facts: [], links: [], origin: "Fixture chat" }],
    chats: async () => [{ id: "fixture-chat", title: "Fixture chat", space: "Mast", status: "working" }],
    transcript: async () => [{ id: "fixture-message", role: "assistant", text: "Fixture transcript" }],
    send: async () => ({ delivery: "simulated" }),
  };
  const backend = command === "fixture" ? fixture : engineBackend("/Applications/Mast.app/Contents/MacOS/mast", "ws://127.0.0.1:27654");
  const server = createBridge(backend, store, new PairingChallenge(code, Date.now()));
  server.listen(27655, "127.0.0.1", () => {
    console.log(`${command === "fixture" ? "Fixture" : "Mast"} bridge listening on 127.0.0.1:27655. Pairing code expires in five minutes:`);
    console.log(code);
  });
} else {
  console.error("Usage: node dist/cli.js start | fixture | revoke");
  process.exitCode = 1;
}
