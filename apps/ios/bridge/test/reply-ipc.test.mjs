import { test } from "node:test";
import assert from "node:assert/strict";
import { WebSocketServer } from "ws";
import { ipcReplyEngine } from "../dist/reply-ipc.js";
import { ReplyCoordinator } from "../dist/reply.js";

test("desktop reply and resolution use the supported IPC shapes", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise(resolve => server.once("listening", resolve));
  const calls = []; let resolved = false;
  server.on("connection", socket => socket.on("message", bytes => {
    const frame = JSON.parse(bytes.toString()); calls.push(frame);
    if (frame.method === "WorkOutlineMutate") resolved = true;
    const item = frame.method === "WatchSpaces" ? [{ id: "s" }] : frame.method === "WatchWorkOutline" ? { available: true, synced: true, userActionRequests: [{ id: "r", text: "Ship?", kind: "approve", state: resolved ? "resolved" : "open", chatId: "c", originChatAvailable: true, ...(resolved ? { resolution: { outcome: "approved", values: { action: "approved" }, by: { kind: "user" } } } : {}) }] } : frame.method === "WatchSessions" ? [] : null;
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

test("busy queue lost acknowledgement stays unknown across bridge restart", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise(resolve => server.once("listening", resolve));
  let sends = 0;
  server.on("connection", socket => socket.on("message", bytes => {
    const frame = JSON.parse(bytes.toString());
    if (frame.method === "QueueMessage") { sends++; assert.equal(frame.params.holdForTurnEnd, true); socket.terminate(); return; }
    const items = { WatchSpaces: [{ id: "s" }], WatchWorkOutline: { available: true, synced: true, userActionRequests: [{ id: "r", text: "Ship?", kind: "ask", state: "open", chatId: "c", originChatAvailable: true }] }, WatchSessions: [{ chatId: "c", status: "working", updatedAt: new Date().toISOString() }], WatchChats: [{ id: "c", deviceId: "d" }], WatchDevices: [] };
    if (frame.method in items) socket.send(JSON.stringify({ id: frame.id, item: items[frame.method] }));
    else socket.send(JSON.stringify({ id: frame.id, ok: { deviceId: "d", capabilities: ["message-queue-v1"] } }));
  }));
  const records = new Map();
  const store = { load: async id => records.get(id) ?? null, save: async value => { records.set(value.requestId, structuredClone(value)); } };
  try {
    const url = `ws://127.0.0.1:${server.address().port}`, input = { outcome: "answered", text: "yes" };
    assert.equal((await new ReplyCoordinator(store, ipcReplyEngine(url)).answer("r", "fixture-operation-key", input)).phase, "unknown");
    assert.equal((await new ReplyCoordinator(store, ipcReplyEngine(url)).answer("r", "fixture-operation-key", input)).phase, "unknown");
    assert.equal(sends, 1);
  } finally { await new Promise(resolve => server.close(resolve)); }
});

test("idle and resolve lost acknowledgements retry the same operation without another delivery", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise(resolve => server.once("listening", resolve));
  let sendCalls = 0, deliveries = 0, resolveCalls = 0, applied = false, sentOperation;
  server.on("connection", socket => socket.on("message", bytes => {
    const frame = JSON.parse(bytes.toString());
    if (frame.method === "ContinueChatForWorkItem") {
      sendCalls++; if (!sentOperation) { sentOperation = frame.params.operationId; deliveries++; socket.terminate(); return; }
      assert.equal(frame.params.operationId, sentOperation); socket.send(JSON.stringify({ id: frame.id, ok: { state: "started" } })); return;
    }
    if (frame.method === "WorkOutlineMutate") {
      resolveCalls++; applied = true;
      if (resolveCalls === 1) { socket.terminate(); return; }
      socket.send(JSON.stringify({ id: frame.id, ok: {} })); return;
    }
    const item = frame.method === "WatchSpaces" ? [{ id: "s" }] : frame.method === "WatchSessions" ? [] : { available: true, synced: true, userActionRequests: [{ id: "r", text: "Ship?", kind: "approve", state: applied ? "resolved" : "open", chatId: "c", originChatAvailable: true, ...(applied ? { resolution: { outcome: "declined", values: { action: "declined" }, by: { kind: "user" } } } : {}) }] };
    socket.send(JSON.stringify({ id: frame.id, item }));
  }));
  const records = new Map();
  const store = { load: async id => records.get(id) ?? null, save: async value => { records.set(value.requestId, structuredClone(value)); } };
  try {
    const url = `ws://127.0.0.1:${server.address().port}`, input = { outcome: "declined" };
    await assert.rejects(new ReplyCoordinator(store, ipcReplyEngine(url)).answer("r", "fixture-operation-key", input));
    assert.equal((await new ReplyCoordinator(store, ipcReplyEngine(url)).answer("r", "fixture-operation-key", input)).phase, "resolving");
    assert.equal((await new ReplyCoordinator(store, ipcReplyEngine(url)).retryResolve("r", "fixture-operation-key")).phase, "resolved");
    assert.equal(sendCalls, 2); assert.equal(deliveries, 1); assert.equal(resolveCalls, 2);
  } finally { await new Promise(resolve => server.close(resolve)); }
});
