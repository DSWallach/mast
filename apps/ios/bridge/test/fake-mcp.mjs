#!/usr/bin/env node
import { createInterface } from "node:readline";
createInterface({ input: process.stdin }).on("line", line => {
  const request = JSON.parse(line);
  if (request.id !== 2) return;
  if (request.params.name === "list_chats") {
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: 2, result: { structuredContent: { chats: [{ id: "fixture-chat", title: "Fixture chat", project: { name: "Space" }, status: "working" }] } } }) + "\n");
    return;
  }
  const value = { tool: request.params.name, args: request.params.arguments, hasToken: Boolean(process.env.MAST_AGENT_TOKEN || process.env.ZERON_AGENT_TOKEN), hasPort: Boolean(process.env.MAST_IPC_PORT || process.env.ZERON_IPC_PORT) };
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: 2, result: { structuredContent: value } }) + "\n");
});
