# Mast iOS companion (experimental)

The app in `apps/ios` and bridge are an experimental prototype. Live iPhone use requires appropriate approval. The bridge exposes only companion operations on `127.0.0.1:27655`; the engine IPC on port 27654 is privileged and must never be exposed directly. The app defaults to built-in demo data. There is no push, attachment support, live command approval or new chat. Reply & resolve supports ask, choose, and approve cards and is enabled by default; approval-rule proposals remain desktop-only.

## Build and fixture tests

From the repository root, with Xcode 27 and Node 22:

```sh
xcodebuild -project apps/ios/Mast.xcodeproj -scheme Mast -destination 'generic/platform=iOS Simulator' -derivedDataPath /tmp/mast-ios-derived CODE_SIGNING_ALLOWED=NO build
cd apps/ios/bridge
PATH="$HOME/.nvm/versions/node/v22.17.0/bin:$PATH" npm ci --ignore-scripts
PATH="$HOME/.nvm/versions/node/v22.17.0/bin:$PATH" npm test
```

Run the unit tests only on the designated iPhone 15 Pro Max simulator, not the iPhone 15 Pro used for live QA:

```sh
xcodebuild -project apps/ios/Mast.xcodeproj -scheme Mast -destination 'platform=iOS Simulator,id=E50FC8FD-69AB-4633-B2C5-49A34B1B6299' -derivedDataPath /tmp/mast-ios-design-derived CODE_SIGNING_ALLOWED=NO test -quiet
```

The bridge tests use isolated state, fake IPC and fake MCP; they never connect to the live engine. The app targets iOS 17.0. For the Space sharing and lock changes, the then-current Node 18 suite passed 27 tests and the simulator command above passed 13 tests on iOS 17.5. Interactive Face ID, the app switcher, signing and the phone transport still need device QA.

## QA and administration

1. Open `apps/ios/Mast.xcodeproj` in Xcode and run the Mast scheme on the simulator yourself. Demo mode uses fixtures. Verify the lock on cold launch, background timeout, app switcher cover, and passcode fallback; the locked-screen ImageRenderer fixtures are `apps/ios/build/snapshots/locked-light.png` and `locked-dark.png`.
2. With approval for the environment and Mast running, build the bridge as above. From `apps/ios/bridge`, use Node 22:

   ```sh
   node dist/cli.js spaces
   node dist/cli.js spaces share "agent-mode-tools"
   node dist/cli.js start --pair
   ```

   Choose only approved Spaces. Sharing defaults to `view`, including legacy entries: verify that reads work and answer/send/stop are denied. Only if writes are approved, run `node dist/cli.js spaces share "agent-mode-tools" --allow send` (or `--allow answer` for decisions only). Pair with the one-time terminal code at `http://127.0.0.1:27655`; Connect shows shared Spaces and local display filters. Check disposable chat and request actions after Face ID/passcode unlock. Quick actions skips the confirmation dialog. Unshare with `node dist/cli.js spaces unshare "agent-mode-tools"` and verify reads and writes are refused. Stop the bridge with Control-C. Pairing is open only with `--pair`; the one-time upgrade prunes untimestamped tokens, so pair again.
3. Once approved for live iPhone use, sign with your own Xcode team. Use an approved Tailscale setup on both devices; this project does not install or configure it. Review `tailscale serve --help` and current configuration; with the bridge running, `tailscale serve --bg http://127.0.0.1:27655` exposes its HTTPS tailnet endpoint. Check `tailscale serve status`, enter that HTTPS URL and a fresh pairing code on the phone, and stop Serve using the installed version's help when finished. The phone and Serve path have not been verified here.

List devices with `node dist/cli.js devices`, revoke one by ID or unique label with `node dist/cli.js revoke <device>`, or revoke all with `node dist/cli.js revoke --all`. `node dist/cli.js audit 50` displays the last 50 metadata records (maximum 1000). Tokens without trusted creation and usage timestamps are pruned; existing devices must pair again. Tokens expire after 30 days absolute or 7 days idle. The app can revoke itself using Forget this Mac.

## Bridge security self-review

- The HTTP listener binds `127.0.0.1` only; engine IPC is never exposed. Only explicit pairing, shared Space, request, chat, transcript, send and answer operations are exposed, with no generic forwarding; browser Origin requests are rejected.
- `shared-spaces.json` in `~/Library/Application Support/MastCompanionBridge/` defaults to empty. The bridge checks the current allowlist and Space association for reads and writes, including direct IDs and both phases of answer. The phone can only narrow the shared list. Changes take effect for subsequent bridge checks; an IPC operation already in flight cannot be recalled. The foreground stream checks sharing on its heartbeat and hides an open detail when it detects unsharing or disconnection; previously delivered content cannot be recalled from a device.
- Pairing codes have 192 bits of entropy, expire after five minutes, permit five attempts and are consumed once. Device tokens have 256 bits of entropy; hashes are compared in constant time. Device labels and per-device revocation are stored separately from Mast's profile. Atomic state replacements are serialized across bridge and CLI processes; reads use file metadata caching. An operation already authorized and dispatched cannot be recalled by revocation.
- Shared Spaces have `view`, `answer`, or `send` capability (send includes answer), defaulting to `view`; the bridge enforces them and limits each device to 10 writes/minute and 200/day. Rates are in-memory and reset when the bridge restarts. Token-to-origin binding prevents a stored token from being sent to a different bridge. The service limits live streams to two per device and four total.
- Pairing, revoke and writes append metadata-only JSONL records (timestamp, device ID, action, target ID, Space ID when authorized, and outcome); no token or message text is logged. Denied IDs have no Space recorded if its association is not authorized. A crash can leave a `started` record without a final outcome. The one-time pairing code is printed only when started with `--pair`.
- State locks recover when the recorded owner PID is confirmed absent; malformed, live-owner or recovery locks fail closed. If a lock remains, stop the bridge and all bridge CLI processes, inspect the PID in the relevant `.tokens.lock`, `.spaces.lock`, or `.audit.lock` owner file and verify it is no longer running; remove only that owner file and empty lock directory, then restart. Do not remove state files.
- The app uses LocalAuthentication device-owner authentication (Face ID or passcode), locks on cold launch and on the configured background timeout (default immediately), and covers content when inactive. Only a device without a configured passcode can explicitly continue without authentication; this exception lasts only for the session. Keychain uses `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`: it requires the device to be unlocked without a separate user-presence prompt on each refresh. Send, stop and answer require a device-owner-authenticated app unlock in the current session; continuing without a passcode remains read-only. The app permits HTTPS or loopback HTTP and refuses redirects; the local-network ATS exception is present only in Debug builds.
- The CLI strips inherited Mast agent tokens and IPC port overrides before starting its unscoped MCP process. Fixture tests and this self-review do not replace live compatibility, environment approval or real-device security testing.

## Horizon design pass

The iOS 17.5 iPhone 15 Pro Max simulator (`E50FC8FD-69AB-4633-B2C5-49A34B1B6299`) ran six passing unit tests, including ImageRenderer fixture output, with:

```sh
xcodebuild -project apps/ios/Mast.xcodeproj -scheme Mast -destination 'platform=iOS Simulator,id=E50FC8FD-69AB-4633-B2C5-49A34B1B6299' -derivedDataPath /tmp/mast-ios-design-derived CODE_SIGNING_ALLOWED=NO test -quiet
```

The bridge suite has 10 passing tests, including message author classification, transcript paging and participation caching. Headless light/dark PNGs are generated in `apps/ios/build/snapshots/` and ignored by git. ImageRenderer does not render UIKit-backed scroll containers, navigation or input controls in isolation, so snapshots render the same content without those containers and use static representations for inputs. The long-chat image renders a tail fixture; it does not verify interactive scrolling. QA should check opening at the most recent message, Load earlier, scrolling after a send, collapsed system/relay rows, and the Show worker chats toggle. The bridge must be restarted after pulling this branch to serve the updated transcript and visibility contract.

## Horizon design pass 2

The source is split by app, model, networking, view and theme; the pre-change snapshot hashes matched immediately after that split. Replies render inline Markdown and simple block headings, lists and fenced code. Tool summaries come from `read_chat` and omit tool output; MCP folds text and tool summaries into each message, so their original interleaving within one message is not available. The official repository icon and light/dark Mast marks are used without recolouring. Offline/empty/loading states, ticking relative times and local send status pills are included. An error during send can leave the delivery outcome unknown: inspect the chat before tapping Retry.

ImageRenderer output in `apps/ios/build/snapshots/` includes `markdown-reply-{light,dark}.png`, `tool-row-{light,dark}.png`, `empty-inbox-{light,dark}.png`, `offline-full-{light,dark}.png`, `inbox-ax3-{light,dark}.png` and `app-icon-{light,dark}.png`, in addition to the earlier screen renders. Snapshots use the headless substitutions described above. Check actual simulator scrolling, Markdown links and horizontal code blocks, launch screen, accessibility sizes, send reconciliation and haptics interactively. The launch screen and haptics cannot be established by ImageRenderer.

## Reply & resolve parity and limits

The bridge reads the current open request before delivery and uses the desktop decision message and option/outcome fields. Idle chats use `ContinueChatForWorkItem` with one durable operation ID; busy chats use a held `QueueMessage`, after capability checks. Acknowledged sends are never sent again: resolution uses the desktop-derived `reply-resolve` ID, and retries call only the resolve half. The bridge checks the resulting user-authored resolution before reporting success. The phone keeps its pending decision in device-only Keychain storage; the bridge journals decisions with private permissions and synchronous writes under `MastCompanionBridge/replies/`. Neither logs content.

A queue acknowledgement can be lost after the host created a row. Because `QueueMessage` does not accept a client id, that case remains **delivery unknown** and is never retried automatically. Check the origin chat on the Mac. The two operations are not atomic: another actor can resolve a request after the last open-row check. The bridge detects a different resolved outcome on readback, but the IPC exposes no resolution operation ID, so an identical concurrent decision cannot be independently attributed. The desktop also uses a transient UI send-pending state which the bridge cannot observe; it uses session status and the same 45-second staleness window. System or unavailable origins resolve without sending a chat message; approval-rule proposal cards are desktop-only because their desktop flow makes an additional settings mutation.

Fixture tests cover duplicate submits, per-kind fields, fresh-row conflicts, unavailable origins, restarts, and lost acknowledgements without connecting to the live engine. Before live QA, restart the bridge to load the changed code, create a disposable request, and inspect the origin chat and resolution on the Mac. Reply & resolve can be switched off in Connect.

## Refinement 3: live updates, QR, and attachment design

The bridge uses authenticated SSE (`/events`) as an invalidation stream. It subscribes to engine chat, session, shared outline and (when a chat is open) transcript watches; the event carries no content. Every event is rechecked against the current Space sharing and device token. The app reloads through the existing authorized endpoints, labels the connection Live or Stale, and retries with exponential backoff and refreshes on failures. Sharing changes or device revocation close the stream on its heartbeat. Confirm streaming, reconnect and revocation interactively; the fixture tests do not establish live watch compatibility. Already dispatched responses cannot be recalled.

`node dist/cli.js start --pair` opens pairing for five minutes and prints a terminal QR as well as the manual code; ordinary `start` does not open pairing. For Tailscale use `node dist/cli.js start --pair --url https://your-machine.tailnet.ts.net`. Connect's Scan QR uses the camera and fills the fields; review them and tap Connect. QR codes contain a short-lived pairing secret: do not share them. Camera permission, physical scanning, Serve and signing need interactive QA. The terminal QR encoder is pinned `qrcode-terminal` 0.12.0 (Apache-2.0); `ws` is pinned 8.21.0 after npm audit flagged earlier versions.

Photo attachments remain unimplemented. The desktop stages chunked `UploadChunk` / `UploadCommit` into engine-managed durable files, then uses different local/remote and queued transfer semantics (`pending://` references and `transfers`), while MCP `send_message` accepts text only. A safe companion implementation needs a bounded, independently validated image decoder/re-encoder on the bridge (not just a phone-supplied MIME type or phone-side EXIF stripping), per-chunk authorization, cleanup of abandoned or revoked uploads, and tested host-capability/queue handling with lost acknowledgements. The exposed IPC upload/commit contract does not provide a documented cleanup/rollback operation for a committed upload. Without those guarantees a paired client could persist arbitrary bytes or metadata into engine storage even after a failed send, and retry could duplicate delivery. Do not expose a generic upload or raw path endpoint; implement this once those lifecycle and delivery guarantees are established, with fake IPC tests for malformed images, limits, revocation, remote host and failures. No attachment data is accepted by this bridge.


## Style selection

Connect offers Match Mac (default), Default, Industrial and Horizon, plus System/Light/Dark appearance. Match Mac requests `/appearance` with the paired token. The bridge reads `ui-settings.json` briefly and returns only `uiStyle`, `uiLayout` and supported light/dark theme IDs, never the other settings. It uses `MAST_DATA_DIR`, then `ZERON_DATA_DIR`, then `~/.zeron`; a custom desktop launch using a different environment must start the bridge with the same data-dir setting. Missing, invalid or unsupported settings fall back to Default/Mast themes. The stream checks for changes at its heartbeat. Layout is reported but phone layouts are adapted rather than reproducing desktop density exactly. Explicit phone style choices use their corresponding desktop theme families. UI style selection does not write the Mac settings. Fixture tests do not read the live settings file.

The iOS ImageRenderer style matrix and desktop comparison PNGs are ignored outputs in `apps/ios/build/style-snapshots/` and `apps/ios/build/style-comparisons/`. The desktop reference uses the nearest available outline details view for Needs-you and the chat sidebar for chats; it is not a pixel-equivalent mobile layout. Check selections, appearance changes, typography, scrolling and accessibility in the simulator. Attach and Ask remain non-interactive visual labels.

## Artifact links and viewer

Request detail and transcript Markdown route `mast-artifact:` links to the companion viewer. Typed request links retain their target type: shared chats open in the app, HTTPS links require confirmation before Safari, and file/diff links say Open on Mac. The bridge finds artifacts only in currently shared Spaces via `WatchArtifacts`, rechecks after content retrieval, projects metadata and never returns stored paths. Content is capped at 4 MiB and audit records contain only device, action, artifact ID, Space ID and outcome. `ArtifactContent` returns one complete IPC frame, so the bridge buffers the capped result before writing the HTTP response; it is not an upstream streaming RPC. In-flight bytes already sent cannot be recalled.

Markdown and supported images use native views. HTML uses a non-persistent WebKit store, JavaScript disabled, a deny-all content rule with only `about:`/`data:` exceptions, restrictive CSP, and a navigation delegate. Consequently interactive JavaScript mockups and Mermaid content may not render and should be opened on the Mac. File/diff targets also require the Mac. Archived chats are not currently reachable via companion chat links. ImageRenderer's HTML fixture is a placeholder because it cannot render WKWebView; verify the actual HTML, pinch zoom, link confirmation, camera/phone signing and share sheet interactively after approval. The 4 MiB cap, hidden-ID checks and revocation during an artifact read are covered by fake IPC tests; no live engine was used.


## Tailscale identity guard

For remote use start the bridge with `--tailscale-user <login>` and the HTTPS `--url` value. With that option, every request must have the matching `Tailscale-User-Login`; simulator loopback requests without that header are refused. The bridge refuses a `Tailscale-Funnel-Request` header and refuses startup when read-only `tailscale serve status --json` indicates Funnel; an unreadable status fails closed. The installed CLI status was inspected without changing Serve. Tailscale documents that Serve strips incoming identity headers and supplies its own, while Funnel does not supply identity headers; the additional Funnel header is defensive and has not been verified as a header the installed version emits. See [Tailscale Serve identity headers](https://tailscale.com/docs/features/tailscale-serve). Do not rely on the header check without `--tailscale-user`: local callers can forge headers, and a network ACL is still required.

A tailnet administrator can restrict the phone to the Mac's HTTPS port with a grant such as `{"src":["tag:mast-phone"],"dst":["tag:mast-mac"],"ip":["tcp:443"]}` in the tailnet's `grants` list, after assigning the tags and removing any broader grants that would also permit access. This is an example for administrator review, not a change made by the bridge. Tagged sources do not receive user identity headers; use a user-owned phone with the login check or a separately designed tagged-device authorization. Audit events include the received login when present.

The bridge requires Node 22 or newer; if absent, install a supported LTS using `nvm install 22` and select it with `nvm use 22`. Do not use an end-of-life runtime. Install bridge dependencies with `npm ci --ignore-scripts`.

Terminal reply journals redact decision text and keep a fingerprint for idempotency. Resolved/conflict journals older than seven days are pruned at bridge startup. Pending and delivery-unknown journals are retained because deleting one could permit an ambiguous reply to be sent twice; resolve or investigate them before manual removal.

The JSONL audit log includes a SHA-256 `prevHash`/`hash` chain and rotates at approximately 10 MiB; the chain continues across files. This detects changes when verified against a retained trusted checkpoint. A local owner who can rewrite all files and checkpoints can recompute the chain or truncate it; it is not a remote or immutable audit service.

Deleting the app may leave a Keychain item on iOS. The app clears its Keychain items on first launch after installation, but always revoke the device from the Mac too; deletion alone is not revocation. Existing installations also clear their old credential once when this sentinel is first introduced.

Write actions require an authenticated app unlock and check the lock generation immediately before dispatch; Quick actions skips only the dialog. A request already dispatched cannot be recalled if the app locks or the device is revoked during delivery.

Tailscale HTTPS certificate issuance can publish the machine's `*.ts.net` name in certificate-transparency logs. Use a neutral machine name and accept the remaining hostname disclosure before enabling Serve.

The pinned `qrcode-terminal` 0.12.0 encoder is unmaintained. It has no runtime dependencies and is used only for terminal QR output; this remains an accepted supply-chain risk until a maintained replacement is reviewed.

Artifact content requests use the artifact's recorded `targetDeviceId`; a shared artifact can therefore be fetched from another Mast device through the engine. The HTML viewer permits only `about:blank` navigation; data URLs remain available solely as restricted subresources under the content policy.
