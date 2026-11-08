import { test } from "node:test";
import assert from "node:assert/strict";
import { engineBackend } from "../dist/backend.js";

test("chat projection excludes extra fields and preserves send delivery", async () => {
  const calls = [];
  const call = async (_binary, tool, args) => {
    calls.push([tool, args]);
    if (tool === "list_chats") return { chats: [{ id: "one", title: "Working", status: "working", project: { name: "Space", path: "/private" }, cwd: "/private" }, { id: "two", status: "idle" }] };
    if (tool === "read_chat") return { messages: [{ id: "m", role: "assistant", text: "hello", tools: ["private"] }] };
    return { sent: { delivery: "queue", id: "command" } };
  };
  const backend = engineBackend("unused", "unused", call);
  assert.deepEqual(await backend.chats(), [{ id: "one", title: "Working", space: "Space", status: "working" }]);
  assert.deepEqual(await backend.transcript("one"), [{ id: "m", role: "assistant", text: "hello" }]);
  assert.deepEqual(await backend.send("one", "hello"), { delivery: "queued" });
  assert.equal(calls[1][1].include_tools, false);
});
