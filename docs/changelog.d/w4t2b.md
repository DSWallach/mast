# W4-T2b: quota broker wiring

- `quota_status` also returns `broker`, the quota broker's compact per-provider view (under 550 bytes), and a one-line `wake` when a provider is at or above the soft cap. Existing fields are unchanged.
- The broker probes the live Claude Code, Codex and Antigravity sign-ins read-only, never refreshing a token. When a probe fails it estimates from the local token ledger, and marks the figure `estimated`. Usage is probed only when read; there is no background poll.
- The Quota caps headroom bars on Accounts now come from the broker, through the new read-only `GetQuotaHeadroom` request. Older engines keep showing account windows.
- Balanced failover holds each provider to its own hard cap. Pinned freeze stops at that cap when you set one, and at the freeze cap otherwise.
- Poll intervals and the reservation TTL are bounds-checked. After reactivation, or when a reading is older than three poll intervals, a provider takes no new work until it is read again.
- Mast does not yet dispatch, pause or freeze work with the broker.
