import { createHash, randomBytes, timingSafeEqual } from "node:crypto";

const digest = (value: string): Buffer => createHash("sha256").update(value).digest();

/** In-memory pairing challenge. Persist only token hashes, never this code. */
export class PairingChallenge {
  private readonly codeDigest: Buffer;
  private readonly expiresAt: number;
  private attempts = 0;
  private consumed = false;

  constructor(code: string, now: number, lifetimeMilliseconds = 300_000) {
    this.codeDigest = digest(code);
    this.expiresAt = now + lifetimeMilliseconds;
  }

  exchange(code: string, now: number): string | null {
    if (this.consumed || now >= this.expiresAt || this.attempts >= 5) return null;
    this.attempts += 1;
    if (!timingSafeEqual(this.codeDigest, digest(code))) return null;
    this.consumed = true;
    return randomBytes(32).toString("base64url");
  }
}

export function generatePairingCode(): string {
  return randomBytes(24).toString("base64url");
}

export function tokenDigest(token: string): string {
  return digest(token).toString("hex");
}
