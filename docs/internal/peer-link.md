# Peer link: drive another Mast engine directly

Status: second release (remote workers with branch transfer and reports). Experimental, off by default.

A Manager on one machine can start and steer chats on a second machine without the hosted relay, registry sync or sign-in. The machine being driven (B) opens a listener; the controlling machine (A) pairs once and then runs a second MCP server that proxies B's engine.

## On B (the machine that builds and tests)

```sh
MAST_PEER_LISTEN=127.0.0.1:27700 mast headless      # or a tailnet address, e.g. 100.101.102.103:27700
mast peer pair                                       # prints a one-time code, valid 5 minutes, 5 attempts
mast peer list                                       # scope, chats created and reports waiting per peer
mast peer scope <id|label> own|all                   # what that peer may reach (see below)
mast peer posture <id|label> ask|rules|auto          # maximum approval posture B accepts
mast peer revoke <id|label> | --all                  # the running engine closes that peer's sockets within seconds
```

The listener refuses any address that is not loopback or a tailnet address (`100.64.0.0/10`, which is also shared carrier space, so use only a tailnet address you own; or `fd7a:115c:a1e0::/48`). Mast does not encrypt this link: use Tailscale, or an SSH tunnel (`ssh -N -L 27700:127.0.0.1:27700 <B>`) to a loopback listener. The headed app does not start the listener yet; use `mast headless` or the daemon.

## On A (the controlling machine)

```sh
mast peer add fedora ws://127.0.0.1:27700            # reads the code from stdin; `<code>` as a third argument also works
mast mcp --peer fedora                              # serves the saved pairing when launched by an MCP client
```

Register the installed Mast binary as a separate stdio server in the harness's MCP settings, with arguments `["mcp", "--peer", "fedora"]`. Use the same command and arguments in Claude Code and Codex. Set the remote pairing scope to `own` for routine worker dispatch. The remote listener enforces the saved pairing's scope. The registration is explicit and needs only the pairing name. B needs its own Space for the repository. Existing server registrations using `MAST_PEER_URL` and `MAST_PEER_TOKEN` still work.

## Approval posture and capacity

B records the peer's maximum approval posture in `peer-devices.json` as `maxPosture`. New pairings and rows written by 0.2.82 default to `ask`. Both scopes use this ceiling. B refuses a create or run that requests Auto above the ceiling, or Rules under an Ask ceiling. Raising the ceiling preserves a caller's narrower request. The bearer authenticates A's device. The standalone peer MCP server does not attest the individual calling chat's posture to B. Rules runs use B's configured approval rules. Creating a side chat or scheduled follow-up requires an Auto ceiling because the engine derives its configuration. Configure `CARGO_TARGET_DIR` in B's engine environment for remote builds. Peer create and run requests cannot supply a replacement environment.

Known limits of the ceiling in 0.3.0:
- At an `ask` ceiling, B refuses `respondInput` and `respondLiveInput` replies that affirm an approval prompt or cannot be classified as a decline (`No`, `Decline`, `Cancel`) or an answer to a non-approval question (`interrupt` stays allowed). At a `rules` or `auto` ceiling, B allows affirmative replies through the peer gate; B's own downstream approval rules/posture and caller-side posture checks on A still apply.
- `CARGO_TARGET_DIR` is inherited from B's engine environment. Mast does not inject or enforce it for a worker's own shell commands.
- Lowering the ceiling with `mast peer posture` affects future create and run calls only. Chats already created with Auto keep their stored configuration.

An authenticated `PeerPreflight` RPC with `{}` returns `freeDiskBytes` on B's target filesystem (engine data filesystem when unset), `slots` (one when the device build lock is available, zero while held), and the peer's `maxPosture`. This reports capacity. Build dispatch still acquires the device lock. A 0.2.82 listener lacks this RPC, so a caller that requires the ceiling must treat an unavailable preflight as unknown.

On A, concurrent `peer add` and `peer remove` commands take an exclusive lock on `peer-links.lock`, reread the list, then atomically replace the owner-only `peer-links.json`. Pairing over the network finishes before the lock is taken. After an engine restart, A reloads the saved URLs and tokens. A closed tunnel or broken route discards the cached connection and retries with backoff up to one minute. Restoring the same endpoint resumes delivery. Re-pairing the same name at another URL or with a new token takes effect on the next polling round, including during the five-minute retry interval for a refused token.

## Recipe: pair the Fedora desktop over an SSH tunnel

Verified 2026-11-26 (laptop `thinkpad` as A, `desktop` as B, both mast 0.2.82). Prerequisites: `ssh desktop` works with key auth, `~/.local/bin/mast` exists on the desktop, and the desktop runs `mast headless` with `MAST_PEER_LISTEN=127.0.0.1:27700` (check: `ssh desktop 'ss -ltn | grep 27700'`). The listener is loopback only, so the tunnel is how A reaches it. No firewall or SELinux change is needed.

```sh
ssh -N -f -L 27701:127.0.0.1:27700 desktop      # 1. tunnel (27701 avoids a local 27700)
ss -ltn | grep 27701                             # 2. must show a listener before step 4
ssh desktop '~/.local/bin/mast peer pair'        # 3. prints a code: 5 minutes, 5 attempts
mast peer add desktop ws://127.0.0.1:27701       # 4. prompts "Pairing code:"; paste the code from step 3
```

Gotchas: `peer add` before the tunnel fails with `Connection refused (os error 111)`. The `Pairing code:` line is a prompt, not a code. The new pairing is scope `own` (check with `ssh desktop '~/.local/bin/mast peer list'`). The desktop engine can hold several pairings; do not revoke the others.

Register the harness side (the pairing alone does not add tools): a stdio MCP server named e.g. `mast-desktop` with command `mast` and args `["mcp", "--peer", "desktop"]`, in Claude Code and Codex settings. The local `list_devices` still shows only A; the peer server's `list_devices` and `list_projects` show the desktop's device and Spaces. The peer server has no `run_command`; remote work is `peer_push_branch` plus `create_chat` (a worker on B), and `peer_command_status` for its runs.

Verify without starting an agent: `mast peer list` on A prints `desktop ws://127.0.0.1:27701  last poll ...: ok`, and calling `list_devices` on `mast mcp --peer desktop` returns the desktop.

Undo or re-pair: `mast peer remove desktop` on A, `ssh desktop '~/.local/bin/mast peer revoke desktop'` on B, kill the tunnel (`pkill -f 'ssh -N -f -L 27701'`), remove the MCP registration, then repeat the recipe.

The `ssh -f` tunnel does not survive a reboot or a network drop, and a dead tunnel shows as an unreachable peer in `mast peer list`. On the thinkpad this is installed as `~/.config/systemd/user/mast-desktop-tunnel.service` (`systemctl --user enable --now mast-desktop-tunnel`; stop and disable it to undo, and use it instead of the `ssh -f` command in step 1), and `mast-desktop` is registered at user scope with `claude mcp add --scope user mast-desktop -- ~/.local/bin/mast mcp --peer desktop` and `codex mcp add mast-desktop -- ~/.local/bin/mast mcp --peer desktop` (undo: `claude mcp remove mast-desktop`, `codex mcp remove mast-desktop`). Persistent options: a systemd user unit on A running `ssh -N -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -L 27701:127.0.0.1:27700 desktop` with `Restart=always`; or bind B's listener to its tailnet address (`MAST_PEER_LISTEN=100.105.54.55:27700`, run via `mast daemon install`), then `mast peer add desktop ws://100.105.54.55:27700` and update `peer-links.json`. The second restarts B's engine, which interrupts any peer using it.

## Running a worker on B

The Manager's second MCP server (the one with `MAST_PEER_URL`) has two extra tools for this:

1. `peer_push_branch { repo, branch, project, base? }`: bundles a local branch with `git bundle` (only commits after `base`, a revision B already has, such as `main`), sends it in chunks and returns `worktreePath`, `peerBranch` and `head`. B files it as `refs/heads/peer/<peer>/<branch>` with a Mast worktree; nothing in B's own branches or checked-out folders moves.
2. `create_chat { project, cwd: worktreePath, branch: peerBranch, parent: <the Manager's own chat id> }` starts the worker. `parent` is an opaque id here: B keeps it for report delivery and does not treat it as a chat of its own.
3. The worker commits on B and calls `report_to_parent` as usual. B keeps the report durably (`peer-devices.json`); the engine on A polls each saved peer (`peer-links.json`, written by `mast peer add`) every 3 seconds, delivers new reports into the Manager's chat as "Worker updates" (with the worker's chat id, to read through the peer MCP server) and then acknowledges them. Delivery is at least once and replay is a no-op. The same text from the same worker within 45 seconds of the first is one report (a retry after a lost reply), whenever in that window it arrives. A report waits on B while A is offline or the link is down (a poll passes over reports it cannot deliver, so they do not hold back later ones); it is dropped when the peer is revoked or the parent chat no longer exists on A. A report whose parent chat is archived goes to the Manager of that chat's work item (the nearest ancestor item with one) with a line naming the archived parent, and is dropped with a warning when there is none; it is not retried. Nothing connects from B to A. A decision to remember: the URL A polls is the one saved by `mast peer add`, separate from any `MAST_PEER_URL` an MCP server uses, so changing how A reaches B (Tailscale off, an SSH tunnel) means updating `peer-links.json` too. A dead link is not silent: the first failed poll of an outage logs a warning, A writes the last poll result per saved peer to `peer-link-status.json` (`mast peer list` prints each saved URL with it), and after 2 minutes unreachable it tells the parent chats that peer's workers have named (learned on earlier polls and kept in `peer-link-status.json`, so a restart does not forget them) with one "Worker updates" notice per outage.
4. `peer_fetch_branch { repo, branch, project, base: <head from push> }`: B bundles what the branch gained and the Manager fetches it into a local `peer/<branch>` branch. It never merges, pushes, or moves a checked-out branch.

For an engine-hosted `run_command`, the runner can include its chat ID and run `idempotencyKey` in the handoff or report. The launching chat can call `peer_command_status { peer, chatId, idempotencyKey }` on its regular injected Mast tools. The remote device checks that the saved pairing created the runner for that exact parent chat, so a runner created without `parent` is ineligible. The result includes the run state, the available exit code, and the last 64 KiB of the command's log. The log crosses the link as recorded. This endpoint needs current engines on both devices and addresses a Mast run by identity, so arbitrary log paths and old shell-only jobs are outside its scope.

Limits: bundles up to 512 MiB, sent in 384 KiB chunks (each chunk call is safe to repeat) and never held whole in memory on A; the scratch bundle on A lives in a temp folder only its owner can open and goes with the call. A git command on B is stopped after 5 minutes and a whole apply or bundle job after 10. A peer reaches only its own `peer/<its id>/` namespace.

## What a paired peer may do

Only the methods the MCP server calls: device, engine info, chat, session and transcript watches, harness and model lists, `Mutate`, `QueueCommand` (including `respondInput` and `respondLiveInput`) and `QueueMessage`, the report outbox and the git transfer methods (`PEER_METHODS` in `crates/engine/src/peer_link.rs`). `Mutate` is narrowed to `createChat` (hosted on this device only; `parentChatId` is stripped and kept as the opaque remote parent), `renameChat` and `setChatArchived`; `QueueCommand` to `run`, `steer`, `autoMessage`, `interrupt` and `respondInput`; any `targetDeviceId` is refused, so a peer cannot hop through this engine to another device. Workspace files, terminals, project actions, settings, engine stop and the human-only methods are refused.

Scope is per pairing. A pairing made with this release is `own`: the peer can read, steer, interrupt, rename and archive only chats it created (a registry of up to 1000 per peer, with watches filtered to them). The Spaces list stays visible to an `own` peer so it can pick a project; it does not see other chats or their transcripts. Pairings made before scopes existed read as `all` (every chat on this device) and keep working unchanged; `mast peer scope <peer> own` narrows one, and a running engine applies the change within a second, including to transcript watches already open (they end within about a second; the Manager reopens what it may still read). A peer that can start an agent can run code as this user on B, so treat pairing like handing out an SSH key whatever the scope. The listener refuses to start on a signed-in engine, because a synced engine would also sync what a peer writes and could host chats on the account's other devices.

Pairing codes and tokens are about 244 random bits. Tokens are stored hashed in `peer-devices.json` (mode 0600) in the engine data dir and expire after 30 days or 7 days idle (activity refreshes the idle clock once a minute). The pairing window lives in the same file so the CLI and the engine agree; writers take an advisory lock, and a damaged file means nobody is paired. A handshake that presents a pairing code is accepted only while a window is open. The listener serves at most 16 peer sockets, gives each 10 seconds to finish the handshake, and the engine sweeps every 2 seconds, so a revoke or expiry closes the socket and aborts its in-flight requests within a few seconds.

## Limits in this release

- Reports flow B to A by A polling; there is no push, and the unreachable notice only reaches parents A learned about on an earlier successful poll (a worker started while the link was already down is not named until the first poll succeeds), and a Manager reaches a worker on B only through the peer MCP server it was started from.
- Required Actions do not cross: the Manager raises its own, and a worker's questions are answered with `respond_to_input` through the peer server.
- B's workers have no copy of A's Work Outline item.
- A peer's pending reports are capped at 200 on B; they count toward it until A collects them.
- A report can be delivered after the Manager hands its role to another chat on A; it follows the handoff.
- The headed app does not start the listener.
- Per peer on B: at most 32 bundle files and 2 GiB of them (cleared 15 minutes after last use), 32 worktrees on `peer/` branches, and 1000 recorded chats (a record stays until the peer is revoked); two git jobs run at once across all peers. An `own` peer starts chats, and queues runs, only in a Space's folder or a worktree it sent a branch to (a run that asks for a worktree or attachments is refused), and only on its own `peer/` branches. Worktree checkouts run with repository hooks off.
- A peer that knows a full commit id B already holds can file it as its own branch and fetch it back. Ids are not guessable, so this is accepted; do not paste commit ids from private branches into a peer's chat.
- Do not run an older Mast against the same data directory after upgrading: its rewrite of `peer-devices.json` does not know `scope`, chat records or the outbox, so an `own` pairing would read as `all` after the next upgrade.

## Worker input

Native worker questions and approvals use the originating chat's live input resolver. When the worker and Manager share a synced workspace, Needs You can show the worker's request and send a one-time answer to its host. This requires the responding engine and the worker host to advertise `respond-scoped-live-input-v1`. This capability also signals that provider approvals carry structured scope. Needs You offers approval when the request displays the operation details and offers denial for summary-only requests. A pairing that connects separate workspaces lets its owner watch the worker and send a scoped response. Showing those requests in a different workspace's Needs You requires the cross-workspace request routing tracked in `f480c7fb`.

On the paired MCP server, `get_chat` returns a `pendingInput` only while the worker's current session reports a fresh waiting state. An unresolved transcript chip with a different request ID can be returned as `transcriptPendingInput` for recovery. That chip is historical evidence. Use the live request identity for any reply.

A command approval can show a literal Git command as the headline when Codex wraps it in a recognized shell. The card keeps the original command, provider-reported working directory and reason accessible. Shell expressions and arguments that need parsing keep their original representation. These labels leave the approval request and its scope intact.

Binding a worktree selects the chat's diff target. Codex permissions come from the executing turn and its cwd. For workspace-write turns, the adapter submits both the discovered worktree and common Git directories as writable roots. Linked worktree Git writes can still need approval under inherited read-only or managed sandbox restrictions, or when the executing cwd differs from the bound checkout. The provider's reason describes the particular request when available.
