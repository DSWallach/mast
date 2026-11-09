import { DispatchDenied } from "./authorization.js";
import { createHash, randomUUID } from "node:crypto";

export type AnswerInput = { outcome: string; text?: string; optionId?: string };
export type AnswerRow = { id: string; text: string; kind: string; state: string; chatId: string; system: boolean; chatAvailable: boolean; options: { id: string; label: string }[]; resolution?: { outcome?: string; note?: string; values?: { action?: string; optionId?: string }; by?: { kind?: string } } };
export type Intent = { inputHash?: string; key: string; input: AnswerInput; requestId: string; operationId: string; chatId: string; rowSignature: string; message: string; note: string; outcome: string; optionId?: string; phase: "prepared" | "idleDispatch" | "queueDispatch" | "sent" | "resolving" | "resolved" | "unknown" | "conflict"; resolveOnly: boolean };
export interface IntentStore { load(requestId: string): Promise<Intent | null>; save(intent: Intent): Promise<void>; }
export interface ReplyEngine {
  openRow(id: string): Promise<AnswerRow | null>;
  isBusy(chatId: string): Promise<boolean>;
  sendIdle(intent: Intent, guard?: () => Promise<void>): Promise<void>;
  sendQueued(intent: Intent, guard?: () => Promise<void>): Promise<void>;
  resolve(intent: Intent, operationId: string, guard?: () => Promise<void>): Promise<void>;
}
export function deriveId(operationId: string, role: string): string {
  return createHash("sha256").update(operationId).update(Buffer.from([0])).update(role).digest("hex").slice(0, 32);
}
export function decisionMessage(request: string, id: string, outcome: string, note: string): string {
  const labels: Record<string, string> = { approved: "Approved", declined: "Declined", changesRequested: "Changes requested", dismissed: "Dismissed", chosen: "Chosen" };
  const label = labels[outcome] ?? "Answered";
  const decision = `${label} in Mast by you (request ${id})${note ? `: ${note}` : ""}`;
  const collapsed = request.trim().split(/\s+/u).join(" ");
  const chars = Array.from(collapsed);
  const quote = chars.length > 120 ? chars.slice(0, 119).join("").trimEnd() + "…" : collapsed;
  return `Re: "${quote}": ${decision.trim()}`;
}
function prepare(row: AnswerRow, key: string, input: AnswerInput): Intent {
  // These cards perform an additional desktop-only approval-rule mutation.
  if (row.text.startsWith("Review proposed approval rule: ")) throw new Error("approval rule decisions require desktop");
  let note = "", outcome = input.outcome, decision = outcome, optionId: string | undefined;
  if (row.kind === "choose" && input.optionId && input.outcome === "chosen" && !input.text) {
    const option = row.options.find(value => value.id === input.optionId);
    if (!option) throw new Error("invalid option");
    note = option.label; optionId = option.id; outcome = "done"; decision = "chosen";
  } else if (row.kind === "approve" && ["approved", "declined"].includes(outcome) && !input.optionId && !input.text) {
    note = "";
  } else if (row.kind === "ask" && outcome === "answered" && !input.optionId && typeof input.text === "string") {
    note = Array.from(input.text.trim()).slice(0, 1000).join("");
    if (!note) throw new Error("empty reply");
  } else { throw new Error("invalid decision"); }
  return { key, input, requestId: row.id, operationId: randomUUID(), chatId: row.chatId, rowSignature: rowSignature(row), message: decisionMessage(row.text, row.id, decision, note), note, outcome, optionId, phase: "prepared", resolveOnly: row.system || !row.chatAvailable };
}
function rowSignature(row: AnswerRow): string {
  return createHash("sha256").update(JSON.stringify([row.id, row.text, row.kind, row.chatId, row.system, row.options.map(option => [option.id, option.label])])).digest("hex");
}

/** At most one active logical decision per request; ambiguous queue sends never repeat. */
export class ReplyCoordinator {
  private readonly running = new Map<string, Promise<Intent>>();
  constructor(private readonly store: IntentStore, private readonly engine: ReplyEngine) {}
  async retryResolve(requestId: string, key: string, guard: () => Promise<void> = async () => {}): Promise<Intent> {
    const running = this.running.get(requestId);
    if (running) await running.catch(() => undefined);
    const intent = await this.store.load(requestId);
    if (!intent || intent.key !== key || !["sent", "resolving", "resolved"].includes(intent.phase)) throw new Error("resolve-only retry unavailable");
    if (intent.phase === "resolved") return intent;
    return this.answer(requestId, key, intent.input, false, guard);
  }
  async answer(requestId: string, key: string, input: AnswerInput, stopAfterSend = false, guard: () => Promise<void> = async () => {}): Promise<Intent> {
    if (!/^[A-Za-z0-9._-]{16,64}$/.test(key)) throw new Error("invalid idempotency key");
    const current = this.running.get(requestId);
    if (current) { await current.catch(() => undefined); return this.answer(requestId, key, input, stopAfterSend, guard); }
    const task = this.perform(requestId, key, input, stopAfterSend, guard);
    this.running.set(requestId, task);
    try { return await task; } finally { this.running.delete(requestId); }
  }
  private async perform(requestId: string, key: string, input: AnswerInput, stopAfterSend: boolean, guard: () => Promise<void>): Promise<Intent> {
    let intent = await this.store.load(requestId);
    if (intent && (intent.key !== key || (intent.inputHash ? intent.inputHash !== createHash("sha256").update(JSON.stringify(input)).digest("hex") : JSON.stringify(intent.input) !== JSON.stringify(input)))) throw new Error("decision conflict");
    if (!intent) {
      const row = await this.engine.openRow(requestId);
      if (!row || row.state !== "open") throw new Error("request no longer open");
      intent = prepare(row, key, input); await this.store.save(intent);
    }
    if (intent.phase === "resolved" || intent.phase === "unknown" || intent.phase === "conflict") return intent;
    if (intent.phase === "queueDispatch") { intent.phase = "unknown"; await this.store.save(intent); return intent; }
    if (intent.phase === "prepared") {
      const row = await this.engine.openRow(requestId);
      if (!row || row.state !== "open" || rowSignature(row) !== intent.rowSignature) { intent.phase = "conflict"; await this.store.save(intent); return intent; }
      if (row.system || !row.chatAvailable) intent.resolveOnly = true;
      if (intent.resolveOnly) { intent.phase = "sent"; await this.store.save(intent); }
      else { intent.phase = await this.engine.isBusy(intent.chatId) ? "queueDispatch" : "idleDispatch"; await this.store.save(intent); }
    }
    if (intent.phase === "idleDispatch") {
      try { await guard(); await this.engine.sendIdle(intent, guard); } catch (error) { if (error instanceof DispatchDenied) throw error; throw new Error("idle delivery unconfirmed; retry same operation"); }
      intent.phase = "sent"; await this.store.save(intent);
    } else if (intent.phase === "queueDispatch") {
      try { await guard(); await this.engine.sendQueued(intent, guard); } catch (error) { if (error instanceof DispatchDenied) { intent.phase = "prepared"; await this.store.save(intent); throw error; } intent.phase = "unknown"; await this.store.save(intent); return intent; }
      intent.phase = "sent"; await this.store.save(intent);
    }
    if (intent.phase === "sent" && stopAfterSend) return intent;
    if (intent.phase === "sent") {
      const row = await this.engine.openRow(requestId);
      if (!row || row.state !== "open") { intent.phase = "conflict"; await this.store.save(intent); return intent; }
      intent.phase = "resolving"; await this.store.save(intent);
    }
    if (intent.phase === "resolving") {
      try { await guard(); await this.engine.resolve(intent, deriveId(intent.operationId, "reply-resolve"), guard); }
      catch { return intent; }
      let row: AnswerRow | null;
      try { row = await this.engine.openRow(requestId); } catch { return intent; }
      if (!row || row.state !== "resolved") return intent;
      const resolution = row.resolution;
      if (!resolution || resolution.by?.kind !== "user" || resolution.outcome !== intent.outcome || (resolution.note ?? "") !== intent.note || resolution.values?.action !== intent.outcome || (resolution.values?.optionId ?? undefined) !== intent.optionId) {
        intent.phase = "conflict"; await this.store.save(intent); return intent;
      }
      intent.phase = "resolved"; await this.store.save(intent);
    }
    return intent;
  }
}
