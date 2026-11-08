import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, unlink, rmdir } from 'node:fs/promises';
import { tmpdir } from 'node:os'; import { join } from 'node:path';
import { readAppearance, defaultAppearance } from '../dist/appearance.js';
test('projects appearance only and defaults unknown or missing settings', async () => {
 const dir = await mkdtemp(join(tmpdir(),'mast-appearance-')), file = join(dir,'settings.json');
 try { assert.deepEqual(await readAppearance(file), defaultAppearance);
 await writeFile(file, JSON.stringify({uiStyle:'industrial',uiLayout:'compact',themeSelection:{light:'industrial-light',dark:'industrial-dark'},secret:'never expose'}));
 assert.deepEqual(await readAppearance(file), {style:'industrial',layout:'compact',lightTheme:'industrial-light',darkTheme:'industrial-dark'});
 await writeFile(file, JSON.stringify({uiStyle:'invalid',themeSelection:{light:'unknown'}})); assert.deepEqual(await readAppearance(file), defaultAppearance);
 } finally { await unlink(file); await rmdir(dir); }
});

test('appearance endpoint requires authentication and returns only projected fields', async () => {
 const { createBridge } = await import('../dist/server.js'); const { PairingChallenge } = await import('../dist/pairing.js'); const { fakeTokens, fixtureAccess, fixtureAudit } = await import('./support.mjs');
 const bridge=createBridge({},fakeTokens(),new PairingChallenge('fixture',Date.now()),undefined,fixtureAccess,fixtureAudit,undefined,async()=>({style:'horizon',layout:'minimal',lightTheme:'horizon-light',darkTheme:'horizon-dark'}));
 await new Promise(r=>bridge.listen(0,'127.0.0.1',r)); const base=`http://127.0.0.1:${bridge.address().port}`;
 try { assert.equal((await fetch(base+'/appearance')).status,401); const pair=await fetch(base+'/pair',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({code:'fixture',label:'test'})}); const {token}=await pair.json(); const res=await fetch(base+'/appearance',{headers:{authorization:`Bearer ${token}`}}); assert.deepEqual(await res.json(),{style:'horizon',layout:'minimal',lightTheme:'horizon-light',darkTheme:'horizon-dark'}); }
 finally { bridge.closeAllConnections(); await new Promise(r=>bridge.close(r)); }
});
