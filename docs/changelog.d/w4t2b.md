# W4-T2b: quota broker wiring (in progress)

- The broker now holds each provider to its own hard cap. Pinned freeze stops at that cap when you set one, and at the freeze cap otherwise. Mast does not run the broker yet.
- Poll intervals and the reservation TTL are bounds-checked. After reactivation, or when a reading is older than three poll intervals, a provider takes no new work until it is read again.
