export function fakeTokens() {
  const records = new Map();
  return { add: async (hash, label) => { const device = { id: "device", label }; records.set(hash, device); return device; }, authenticate: async hash => records.get(hash) ?? null };
}
export const fixtureAccess = { spaces: async () => [{ id: "space", name: "Space" }], chatSpace: async () => "space", requestSpace: async () => "space", chatAllowed: async () => true, requestAllowed: async () => true };
export const fixtureAudit = { append: async () => {} };
