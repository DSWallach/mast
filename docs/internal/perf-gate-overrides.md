# Perf-gate overrides

One line per install that skipped the gate: time, HEAD, reason.

- 2026-11-17T22:53:37Z 9a07243be3276852ea7621421cc1105b65aee963 Release 105 (9a07243be): Mikey OK'd 2026-11-17. Release tests fail only on environment load: a VM at ~290% CPU and fseventsd at ~100% made three diff_sync startup-settle tests (watch attach; 47 s alone at 104 vs 55 s at 105) and one 2 s harness read timeout fail; each passes alone. Soak vs 104 passed (CPU 5.2 vs 5.2 %, footprint 406 vs 432 MB, git/min 116 vs 112).
- 2026-11-25T18:25:20Z ac1714a5a0c2a7339e45b4b456d023fbd0c184cb David explicitly approved skipping the perf gate (Required Action a748096e, 2026-11-25) to install Scheduled pins and the Needs You formatting on the Mac while Fedora is offline. Targeted tests passed; full suites and soak to run on Fedora later.
- 2026-11-25T19:15:42Z 3be7d6fba90c7a497090c95f94d0dde654de3203 rolling 3be7d6fba: sidebar pin init on edge-less hosts; David approved override 2026-11-25; one pre-existing agent_tools test failure at base
