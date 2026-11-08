import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { callChatTool } from "../dist/mcp.js";

test("chat MCP uses a fixed invocation without inherited agent credentials", async () => {
  process.env.MAST_AGENT_TOKEN = "fixture";
  process.env.MAST_IPC_PORT = "1234";
  try {
    const result = await callChatTool(fileURLToPath(new URL("fake-mcp.mjs", import.meta.url)), "read_chat", { chat: "fixture", include_tools: false });
    assert.equal(result.tool, "read_chat");
    assert.equal(result.hasToken, false);
    assert.equal(result.hasPort, false);
    assert.equal(result.args.chat, "fixture");
  } finally { delete process.env.MAST_AGENT_TOKEN; delete process.env.MAST_IPC_PORT; }
});
