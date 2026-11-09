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

  exchangeResult(code: string, now: number): { token: string } | { reason: "used" | "expired" | "attempts-exhausted" | "wrong-code" } {
    if (this.consumed) return { reason: "used" };
    if (now >= this.expiresAt) return { reason: "expired" };
    if (this.attempts >= 5) return { reason: "attempts-exhausted" };
    this.attempts += 1;
    if (!timingSafeEqual(this.codeDigest, digest(code))) return { reason: "wrong-code" };
    this.consumed = true;
    return { token: randomBytes(32).toString("base64url") };
  }
  exchange(code: string, now: number): string | null {
    const result = this.exchangeResult(code, now); return "token" in result ? result.token : null;
  }
}

export function generatePairingCode(): string {
  return randomBytes(24).toString("base64url");
}

export function tokenDigest(token: string): string {
  return digest(token).toString("hex");
}
