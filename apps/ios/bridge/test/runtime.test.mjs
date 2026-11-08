import { test } from 'node:test'; import assert from 'node:assert/strict'; import { requireSupportedNode } from '../dist/runtime.js';
test('runtime requires supported Node', () => { for (const version of ['18.20.8','20.19.0','invalid']) assert.throws(()=>requireSupportedNode(version)); for (const version of ['22.17.0','24.14.1']) assert.doesNotThrow(()=>requireSupportedNode(version)); });
