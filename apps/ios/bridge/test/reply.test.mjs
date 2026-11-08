import { test } from "node:test";
import assert from "node:assert/strict";
import { ReplyCoordinator, decisionMessage, deriveId } from "../dist/reply.js";
const key = "fixture-operation-key";
function fixtures(overrides = {}) {
  let intent = null, row = { id: "request", text: "Ship?", kind: "ask", state: "open", chatId: "chat", system: false, chatAvailable: true, options: [{ id: "a", label: "Alpha" }], ...overrides.row };
  const calls = [];
  const store = { load: async () => intent && structuredClone(intent), save: async value => { intent = structuredClone(value); } };
  const engine = { openRow: async () => row, isBusy: async () => Boolean(overrides.busy), sendIdle: async value => { calls.push(["idle", value.message, value.operationId]); if (overrides.idleError) throw Error(); }, sendQueued: async value => { calls.push(["queue", value.message]); if (overrides.queueError) throw Error(); }, resolve: async (value, operationId) => { calls.push(["resolve", value.outcome, value.note, value.optionId, operationId]); if (overrides.resolveError) throw Error(); row = { ...row, state: "resolved", resolution: { outcome: value.outcome, note: value.note, values: { action: value.outcome, ...(value.optionId ? { optionId: value.optionId } : {}) }, by: { kind: "user" } } }; } };
  return { store, engine, calls, setRow: value => { row = value; } };
}
test("formats desktop decisions and derives the resolve operation", () => {
  assert.equal(decisionMessage(" Ship? \n please ", "r1", "approved", ""), 'Re: "Ship? please": Approved in Mast by you (request r1)');
  assert.equal(decisionMessage("Ship?", "r1", "chosen", "Alpha"), 'Re: "Ship?": Chosen in Mast by you (request r1): Alpha');
  assert.equal(deriveId("op", "reply-resolve"), "9a066c61eed60ce69331165bb13fd34e");
});
test("duplicate submits and restart after partial resolution never resend", async () => {
  const f = fixtures({ resolveError: true }); const input = { outcome: "answered", text: "yes" };
  assert.equal((await new ReplyCoordinator(f.store, f.engine).answer("request", key, input)).phase, "resolving");
  f.engine.resolve = async (...args) => { const value = args[0]; f.calls.push(["resolve-retry", args[1]]); f.setRow({ id: "request", text: "Ship?", kind: "ask", state: "resolved", chatId: "chat", system: false, chatAvailable: true, options: [], resolution: { outcome: value.outcome, note: value.note, values: { action: value.outcome }, by: { kind: "user" } } }); };
  assert.equal((await new ReplyCoordinator(f.store, f.engine).answer("request", key, input)).phase, "resolved");
  assert.equal(f.calls.filter(call => call[0] === "idle").length, 1);
  assert.equal((await new ReplyCoordinator(f.store, f.engine).answer("request", key, input)).phase, "resolved");
});
test("concurrent duplicates serialize and reuse one send", async () => {
  const f = fixtures(); const input = { outcome: "answered", text: "yes" };
  const coordinator = new ReplyCoordinator(f.store, f.engine);
  const results = await Promise.all([coordinator.answer("request", key, input), coordinator.answer("request", key, input)]);
  assert.deepEqual(results.map(result => result.phase), ["resolved", "resolved"]);
  assert.equal(f.calls.filter(call => call[0] === "idle").length, 1);
});
test("a lost busy-queue acknowledgement is never retried", async () => {
  const f = fixtures({ busy: true, queueError: true }); const input = { outcome: "answered", text: "yes" };
  assert.equal((await new ReplyCoordinator(f.store, f.engine).answer("request", key, input)).phase, "unknown");
  assert.equal((await new ReplyCoordinator(f.store, f.engine).answer("request", key, input)).phase, "unknown");
  assert.equal(f.calls.filter(call => call[0] === "queue").length, 1);
});
test("unavailable origins resolve only; option and approval fields match desktop", async () => {
  for (const row of [{ kind: "approve", system: true }, { kind: "approve", chatAvailable: false }]) {
    const f = fixtures({ row }); const result = await new ReplyCoordinator(f.store, f.engine).answer("request", key, { outcome: "approved" });
    assert.equal(result.phase, "resolved"); assert.deepEqual(f.calls.map(call => call[0]), ["resolve"]);
  }
  const f = fixtures({ row: { kind: "choose" } });
  const result = await new ReplyCoordinator(f.store, f.engine).answer("request", key, { outcome: "chosen", optionId: "a" });
  assert.equal(result.outcome, "done"); assert.equal(result.note, "Alpha"); assert.equal(result.optionId, "a");
});
test("concurrently resolved request is rejected before send", async () => {
  const f = fixtures(); let checks = 0;
  f.engine.openRow = async () => ++checks === 1 ? { id: "request", text: "Ship?", kind: "ask", state: "open", chatId: "chat", system: false, chatAvailable: true, options: [] } : null;
  assert.equal((await new ReplyCoordinator(f.store, f.engine).answer("request", key, { outcome: "answered", text: "yes" })).phase, "conflict");
  assert.equal(f.calls.length, 0);
});
test("a request resolved elsewhere after send is reported as conflict", async () => {
  const f = fixtures(); let checks = 0;
  f.engine.openRow = async () => ++checks < 3 ? { id: "request", text: "Ship?", kind: "ask", state: "open", chatId: "chat", system: false, chatAvailable: true, options: [] } : null;
  const result = await new ReplyCoordinator(f.store, f.engine).answer("request", key, { outcome: "answered", text: "yes" });
  assert.equal(result.phase, "conflict");
  assert.deepEqual(f.calls.map(call => call[0]), ["idle"]);
});

test('revocation guard prevents dispatch after preparation', async () => {
 const f=fixtures(); let checks=0; const guard=async()=>{checks++;throw Error('revoked')};
 await assert.rejects(()=>new ReplyCoordinator(f.store,f.engine).answer('request',key,{outcome:'answered',text:'yes'},false,guard));
 assert.equal(checks,1); assert.deepEqual(f.calls,[]);
});
