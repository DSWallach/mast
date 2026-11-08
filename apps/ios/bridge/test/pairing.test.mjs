import { test } from "node:test";
import assert from "node:assert/strict";
import { PairingChallenge, generatePairingCode, tokenDigest } from "../dist/pairing.js";

test("pairing succeeds once and produces a distinct bearer token", () => {
  const code = generatePairingCode();
  const challenge = new PairingChallenge(code, 0);
  const token = challenge.exchange(code, 1);
  assert.ok(token && token.length >= 40);
  assert.notEqual(token, code);
  assert.equal(challenge.exchange(code, 2), null);
  assert.notEqual(tokenDigest(token), token);
});

test("expired and exhausted challenges fail closed", () => {
  assert.equal(new PairingChallenge("secret", 0).exchange("secret", 300_000), null);
  const challenge = new PairingChallenge("secret", 0);
  for (let i = 0; i < 5; i++) assert.equal(challenge.exchange("wrong", 1), null);
  assert.equal(challenge.exchange("secret", 1), null);
});
