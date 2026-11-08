import { test } from "node:test";
import assert from "node:assert/strict";
import { engineBackend, messageAuthor } from "../dist/backend.js";

test("normalizes actual relay, system, reply and user messages", () => {
  const author = text => messageAuthor({ id: "1", role: "user", text });
  assert.equal(author("[Message from Mast chat Coordinator (123abc)]\nPlease review"), "relay");
  assert.equal(author("[Mast system message, not from the user: auto-continue 1/3]\nContinue"), "system");
  assert.equal(author("Worker updates:\nA worker finished"), "system");
  assert.equal(author("auto-continue 2/3"), "system");
  assert.equal(author('Re: "Approve?": yes'), "system");
  assert.equal(author("Please review this"), "user");
  assert.equal(messageAuthor({ id: "1", role: "assistant", text: "Done" }), "agent");
});

test("classifies user participation across pages, caches revisions, and keeps request origins", async () => {
  let reads = 0;
  const call = async (_binary, tool, args) => {
    if (tool === "list_chats") return { chats: [{ id: "parent", title: "Parent", status: "working", lastMessageAt: "v1" }, { id: "worker", title: "Worker", status: "idle", parentChatId: "parent" }, { id: "request", title: "Request", status: "idle" }] };
    reads++;
    return args.offset === 0 ? { total: 41, olderRemaining: 1, messages: [{ id: "x", role: "user", text: "Worker updates: done" }] } : { total: 41, olderRemaining: 0, messages: [{ id: "old", role: "user", text: "My question" }] };
  };
  const backend = engineBackend("unused", "unused", call, async () => [{ originChatId: "request" }]);
  const rows = await backend.chats();
  assert.equal(rows[0].hasUserMessage, true);
  assert.equal(rows[1].isWorker, true);
  assert.equal(rows[2].hasOpenRequest, true);
  assert.equal(reads, 2);
  await backend.chats();
  assert.equal(reads, 2);
});
