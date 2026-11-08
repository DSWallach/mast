import { test } from 'node:test'; import assert from 'node:assert/strict';
import { createBridge } from '../dist/server.js'; import { PairingChallenge } from '../dist/pairing.js'; import { fakeTokens, fixtureAccess } from './support.mjs';
test('write rate limit is per device and audited', async () => {
 const records=[], bridge=createBridge({send:async()=>({delivery:'accepted'})},fakeTokens(),new PairingChallenge('fixture',Date.now()),undefined,fixtureAccess,{append:async e=>records.push(e)});
 await new Promise(r=>bridge.listen(0,'127.0.0.1',r)); const base=`http://127.0.0.1:${bridge.address().port}`;
 try { const pair=await fetch(base+'/pair',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({code:'fixture',label:'test'})}), {token}=await pair.json(), headers={authorization:`Bearer ${token}`,'content-type':'application/json'};
 for(let i=0;i<10;i++) assert.equal((await fetch(base+'/chats/a/messages',{method:'POST',headers,body:'{"text":"x"}'})).status,200);
 assert.equal((await fetch(base+'/chats/a/messages',{method:'POST',headers,body:'{"text":"x"}'})).status,429); assert.ok(records.some(e=>e.outcome==='rate-limited'));
 } finally {bridge.closeAllConnections();await new Promise(r=>bridge.close(r));}
});
