import { test } from "node:test";
import assert from "node:assert/strict";
import { WebSocketServer } from "ws";
import { ipcReplyEngine } from "../dist/reply-ipc.js";
import { ReplyCoordinator } from "../dist/reply.js";

test("desktop reply and resolution use the supported IPC shapes", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise(resolve => server.once("listening", resolve));
  const calls = [];
  server.on("connection", socket => socket.on("message", bytes => {
    const frame = JSON.parse(bytes.toString()); calls.push(frame);
    const item = frame.method === "WatchSpaces" ? [{ id: "s" }] : frame.method === "WatchWorkOutline" ? { available: true, synced: true, userActionRequests: [{ id: "r", text: "Ship?", kind: "approve", state: "open", chatId: "c", originChatAvailable: true }] } : frame.method === "WatchSessions" ? [] : null;
    if (item) socket.send(JSON.stringify({ id: frame.id, item }));
    else socket.send(JSON.stringify({ id: frame.id, ok: frame.method === "ContinueChatForWorkItem" ? { state: "started" } : {} }));
  }));
  const records = new Map();
  const store = { load: async id => records.get(id) ?? null, save: async value => { records.set(value.requestId, structuredClone(value)); } };
  try {
    const coordinator = new ReplyCoordinator(store, ipcReplyEngine(`ws://127.0.0.1:${server.address().port}`));
    const result = await coordinator.answer("r", "fixture-operation-key", { outcome: "approved" });
    assert.equal(result.phase, "resolved");
    const send = calls.find(frame => frame.method === "ContinueChatForWorkItem");
    assert.equal(send.params.prompt.text, 'Re: "Ship?": Approved in Mast by you (request r)');
    assert.equal(send.params.operationId, result.operationId);
    const resolve = calls.find(frame => frame.method === "WorkOutlineMutate");
    assert.equal(resolve.params.op, "resolveUserActionRequest");
    assert.deepEqual(resolve.params.values, { action: "approved" });
    assert.equal(resolve.params.via, "card");
  } finally { await new Promise(resolve => server.close(resolve)); }
});
