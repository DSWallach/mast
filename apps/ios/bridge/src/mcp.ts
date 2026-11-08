import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

type Tool = "list_chats" | "read_chat" | "send_message";

/** A fixed set of local MCP tools, always without an agent capability token. */
export async function callChatTool(binary: string, tool: Tool, args: Record<string, unknown>): Promise<Record<string, unknown>> {
  const environment = { ...process.env };
  delete environment.MAST_AGENT_TOKEN;
  delete environment.ZERON_AGENT_TOKEN;
  delete environment.MAST_IPC_PORT;
  delete environment.ZERON_IPC_PORT;
  return new Promise((resolve, reject) => {
    const child = spawn(binary, ["mcp"], { env: environment, stdio: ["pipe", "pipe", "ignore"] });
    let settled = false;
    const finish = (error?: Error, value?: Record<string, unknown>) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.kill();
      if (error) reject(error); else resolve(value!);
    };
    const timer = setTimeout(() => finish(new Error("MCP response timed out; delivery may be unknown")), 15_000);
    child.on("error", () => finish(new Error("MCP process failed")));
    child.on("exit", () => finish(new Error("MCP process exited")));
    const lines = createInterface({ input: child.stdout });
    lines.on("line", line => {
      if (line.length > 1_000_000) return finish(new Error("MCP response too large"));
      try {
        const response = JSON.parse(line);
        if (response.id !== 2) return;
        if (response.error || response.result?.isError || !response.result?.structuredContent) return finish(new Error("MCP operation failed"));
        finish(undefined, response.result.structuredContent);
      } catch { finish(new Error("invalid MCP response")); }
    });
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18" } }) + "\n");
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: tool, arguments: args } }) + "\n");
  });
}
