export class DispatchDenied extends Error {
  constructor(readonly status: 401 | 403, readonly need?: "answer" | "send") { super("authorization changed"); }
}
