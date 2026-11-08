import { test } from "node:test";
import assert from "node:assert/strict";
import { engineBackend } from "../dist/backend.js";

test("chat projection excludes extra fields and preserves send delivery", async () => {
  const calls = [];
  const call = async (_binary, tool, args) => {
    calls.push([tool, args]);
    if (tool === "list_chats") return { chats: [{ id: "one", title: "Working", status: "working", project: { name: "Space", path: "/private" }, cwd: "/private" }, { id: "two", status: "idle" }] };
    if (tool === "read_chat") return { total: 1, olderRemaining: 0, messages: [{ id: "m", role: "user", text: "hello", tools: ["private"] }] };
    return { sent: { delivery: "queue", id: "command" } };
  };
  const backend = engineBackend("unused", "unused", call, async () => []);
  const chats = await backend.chats();
  assert.equal(chats[0].id, "one");
  assert.equal(chats[0].space, "Space");
  assert.equal("cwd" in chats[0], false);
  assert.equal(chats.length, 2);
  assert.deepEqual((await backend.transcript("one", 0)).messages, [{ id: "m", role: "user", text: "hello", author: "user" }]);
  assert.deepEqual(await backend.send("one", "hello"), { delivery: "queued" });
  assert.equal(calls.find(call => call[0] === "read_chat")[1].include_tools, false);
});
