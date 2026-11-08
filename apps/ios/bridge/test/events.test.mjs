import { test } from 'node:test';
import assert from 'node:assert/strict';
import { WebSocketServer } from 'ws';
import { createBridge } from '../dist/server.js';
import { PairingChallenge } from '../dist/pairing.js';
import { fakeTokens, fixtureAccess, fixtureAudit } from './support.mjs';

test('event stream filters hidden chats and permits authorized transcript watch', async () => {
  const ipc = new WebSocketServer({ host: '127.0.0.1', port: 0 }); await new Promise(r => ipc.once('listening', r));
  const watchers = new Map();
  ipc.on('connection', ws => ws.on('message', bytes => { const frame = JSON.parse(bytes); watchers.set(frame.method, ws); ws.send(JSON.stringify({id: 1, item: frame.method === 'WatchChats' ? [{id:'visible',spaceId:'shared'},{id:'hidden',spaceId:'private', title:'secret'}] : frame.method === 'WatchSessions' ? [] : { reset: [] } })); }));
  const access = { spaces: async () => [{id:'shared',name:'Shared'}], chatAllowed: async id => id === 'visible', chatSpace: async id => id === 'visible' ? 'shared' : null, requestAllowed: async () => false, requestSpace: async () => null };
  const bridge = createBridge({}, fakeTokens(), new PairingChallenge('code', Date.now()), undefined, access, fixtureAudit, `ws://127.0.0.1:${ipc.address().port}`); await new Promise(r => bridge.listen(0,'127.0.0.1',r));
  const base = `http://127.0.0.1:${bridge.address().port}`, pair = await fetch(base+'/pair',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({code:'code',label:'test'})}), {token} = await pair.json(), headers={authorization:`Bearer ${token}`};
  assert.equal((await fetch(base+'/events?chat=hidden',{headers})).status,403);
  const abort = new AbortController(), res = await fetch(base+'/events?chat=visible',{headers,signal:abort.signal}), reader = res.body.getReader();
  assert.equal(res.status,200); await reader.read();
  while (watchers.size < 4) await new Promise(r=>setTimeout(r,5));
  watchers.get('WatchChats').send(JSON.stringify({id:1,item:[{id:'visible',spaceId:'shared'},{id:'hidden',spaceId:'private',title:'changed secret'}]}));
  watchers.get('WatchDocMessages').send(JSON.stringify({id:1,item:{delta:'visible update'}}));
  const next = new TextDecoder().decode((await reader.read()).value); assert.match(next,/event: transcript/); assert.doesNotMatch(next,/secret|refresh/);
  abort.abort(); bridge.closeAllConnections(); await new Promise(r=>bridge.close(r)); for (const ws of ipc.clients) ws.terminate(); await new Promise(r=>ipc.close(r));
});

test('at most two streams per device, released on disconnect', async () => {
  const ipc = new WebSocketServer({host:'127.0.0.1',port:0}); await new Promise(r=>ipc.once('listening',r));
  ipc.on('connection',ws=>ws.on('message',bytes=>{const frame=JSON.parse(bytes);ws.send(JSON.stringify({id:1,item:frame.method==='WatchChats'||frame.method==='WatchSessions'?[]:{}}));}));
  const bridge=createBridge({},fakeTokens(),new PairingChallenge('fixture',Date.now()),undefined,fixtureAccess,fixtureAudit,`ws://127.0.0.1:${ipc.address().port}`); await new Promise(r=>bridge.listen(0,'127.0.0.1',r));
  const base=`http://127.0.0.1:${bridge.address().port}`, pair=await fetch(base+'/pair',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({code:'fixture',label:'phone'})}), {token}=await pair.json(), headers={authorization:`Bearer ${token}`};
  const a=new AbortController(), b=new AbortController();
  try {assert.equal((await fetch(base+'/events',{headers,signal:a.signal})).status,200);assert.equal((await fetch(base+'/events',{headers,signal:b.signal})).status,200);assert.equal((await fetch(base+'/events',{headers})).status,429);a.abort();let status=429;for(let i=0;i<50&&status===429;i++){await new Promise(r=>setTimeout(r,10));const c=new AbortController();const result=await fetch(base+'/events',{headers,signal:c.signal});status=result.status;c.abort();}assert.equal(status,200);}
  finally {a.abort();b.abort();bridge.closeAllConnections();await new Promise(r=>bridge.close(r));for(const ws of ipc.clients)ws.terminate();await new Promise(r=>ipc.close(r));}
});

test('at most four streams across devices', async () => {
  const { tokenDigest } = await import('../dist/pairing.js');
  const ipc=new WebSocketServer({host:'127.0.0.1',port:0});await new Promise(r=>ipc.once('listening',r));
  ipc.on('connection',ws=>ws.on('message',bytes=>{const frame=JSON.parse(bytes);ws.send(JSON.stringify({id:1,item:frame.method==='WatchChats'||frame.method==='WatchSessions'?[]:{}}));}));
  const secrets=[1,2,3].map(n=>Buffer.alloc(32,n).toString('base64url')), records=new Map(secrets.map((value,index)=>[tokenDigest(value),{id:String(index),label:'device'}]));
  const tokens={authenticate:async hash=>records.get(hash)??null,add:async()=>{throw Error('unused')}};
  const bridge=createBridge({},tokens,new PairingChallenge('unused',Date.now()),undefined,fixtureAccess,fixtureAudit,`ws://127.0.0.1:${ipc.address().port}`);await new Promise(r=>bridge.listen(0,'127.0.0.1',r));
  const base=`http://127.0.0.1:${bridge.address().port}`, controllers=[];
  try {for(const index of [0,0,1,1]) {const c=new AbortController();controllers.push(c);assert.equal((await fetch(base+'/events',{headers:{authorization:`Bearer ${secrets[index]}`},signal:c.signal})).status,200)} assert.equal((await fetch(base+'/events',{headers:{authorization:`Bearer ${secrets[2]}`}})).status,429);}
  finally {for(const c of controllers)c.abort();bridge.closeAllConnections();await new Promise(r=>bridge.close(r));for(const ws of ipc.clients)ws.terminate();await new Promise(r=>ipc.close(r));}
});
