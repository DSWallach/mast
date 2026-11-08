import { homedir } from "node:os";
import { join } from "node:path";
import { PairingChallenge, generatePairingCode } from "./pairing.js";
import { createBridge, type BridgeBackend } from "./server.js";
import { FileTokenStore } from "./token-store.js";

const directory = join(homedir(), "Library", "Application Support", "MastCompanionBridge");
const command = process.argv[2];
if (command === "revoke") {
  await (await FileTokenStore.open(directory)).revokeAll();
  console.log("All companion device credentials revoked.");
} else if (command === "fixture") {
  const store = await FileTokenStore.open(directory);
  const code = generatePairingCode();
  const backend: BridgeBackend = {
    requests: async () => [{ id: "fixture-request", space: "Mast", kind: "review", text: "Review the companion layout", detail: "Fixture request", options: [], facts: [], links: [], origin: "Fixture chat" }],
    chats: async () => [{ id: "fixture-chat", title: "Fixture chat", space: "Mast", status: "working" }],
    transcript: async () => [{ role: "assistant", text: "Fixture transcript" }],
    send: async () => ({ delivery: "simulated" }),
  };
  const server = createBridge(backend, store, new PairingChallenge(code, Date.now()));
  server.listen(27655, "127.0.0.1", () => {
    console.log("Fixture bridge listening on 127.0.0.1:27655. Pairing code expires in five minutes:");
    console.log(code);
  });
} else {
  console.error("Usage: node dist/cli.js fixture | revoke");
  console.error("Live engine integration is not implemented.");
  process.exitCode = 1;
}
