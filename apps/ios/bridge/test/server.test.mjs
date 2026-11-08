import { test } from "node:test";
import { request } from "node:http";
import assert from "node:assert/strict";
import { createBridge } from "../dist/server.js";
import { fakeTokens, fixtureAccess, fixtureAudit } from "./support.mjs";
import { PairingChallenge } from "../dist/pairing.js";

test("allowlisted endpoints require a paired token and reject browser origins", async () => {
  const calls = [];
  const store = fakeTokens();
  const backend = { requests: async () => [{ id: "r", spaceId: "space" }], chats: async () => [], transcript: async id => [id], send: async (id, text) => { calls.push([id, text]); return { delivery: "queued" }; } };
  const server = createBridge(backend, store, new PairingChallenge("fixture-code", Date.now()), undefined, fixtureAccess, fixtureAudit);
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  try {
    const base = `http://127.0.0.1:${server.address().port}`;
    assert.equal(await new Promise((resolve, reject) => { const req = request(base + "/spaces", {headers: {host: "evil.example"}}, response => { response.resume(); resolve(response.statusCode); }); req.on("error", reject); req.end(); }), 403);
    assert.equal((await fetch(base + "/requests")).status, 401);
    const pair = await fetch(base + "/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "fixture-code", label: "Test device" }) });
    const { token } = await pair.json();
    const headers = { authorization: `Bearer ${token}` };
    assert.deepEqual(await (await fetch(base + "/requests", { headers })).json(), { items: [{ id: "r", spaceId: "space" }], noSpacesShared: false });
    assert.equal((await fetch(base + "/requests", { headers: { ...headers, origin: "https://example.com" } })).status, 403);
    assert.equal((await fetch(base + "/rpc", { method: "POST", headers })).status, 404);
    const sent = await fetch(base + "/chats/abc/messages", { method: "POST", headers: { ...headers, "content-type": "application/json" }, body: JSON.stringify({ text: "hello" }) });
    assert.equal((await sent.json()).delivery, "queued");
    assert.deepEqual(calls, [["abc", "hello"]]);
    assert.equal((await fetch(base + "/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "fixture-code", label: "Test device" }) })).status, 401);
  } finally { await new Promise(resolve => server.close(resolve)); }
});

test('pairing stays closed unless explicitly enabled', async () => {
  const server=createBridge({},fakeTokens(),new PairingChallenge('fixture',Date.now()),undefined,fixtureAccess,fixtureAudit,undefined,undefined,undefined,10,undefined,undefined,false);
  await new Promise(r=>server.listen(0,'127.0.0.1',r));
  try { const result=await fetch(`http://127.0.0.1:${server.address().port}/pair`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({code:'fixture',label:'device'})}); assert.equal(result.status,403); }
  finally {server.closeAllConnections();await new Promise(r=>server.close(r));}
});

test('pair denials are aggregated rather than individually audited', async () => {
 const records=[],server=createBridge({},fakeTokens(),new PairingChallenge('good',Date.now()),undefined,fixtureAccess,{append:async record=>records.push(record)});await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const base=`http://127.0.0.1:${server.address().port}`;
 for(let i=0;i<2;i++)assert.equal((await fetch(base+'/pair',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({code:'bad',label:'device'})})).status,401);
 assert.equal(records.length,0);server.closeAllConnections();await new Promise(r=>server.close(r));assert.equal(records.length,1);assert.equal(records[0].outcome,'count:2');
});
