# Peer link: drive another Mast engine directly

Status: second release (remote workers with branch transfer and reports). Experimental, off by default.

A Manager on one machine can start and steer chats on a second machine without the hosted relay, registry sync or sign-in. The machine being driven (B) opens a listener; the controlling machine (A) pairs once and then runs a second MCP server that proxies B's engine.

## On B (the machine that builds and tests)

```sh
MAST_PEER_LISTEN=127.0.0.1:27700 mast headless      # or a tailnet address, e.g. 100.101.102.103:27700
mast peer pair                                       # prints a one-time code, valid 5 minutes, 5 attempts
mast peer list                                       # scope, chats created and reports waiting per peer
mast peer scope <id|label> own|all                   # what that peer may reach (see below)
mast peer revoke <id|label> | --all                  # the running engine closes that peer's sockets within seconds
```

The listener refuses any address that is not loopback or a tailnet address (`100.64.0.0/10`, which is also shared carrier space, so use only a tailnet address you own; or `fd7a:115c:a1e0::/48`). Mast does not encrypt this link: use Tailscale, or an SSH tunnel (`ssh -N -L 27700:127.0.0.1:27700 <B>`) to a loopback listener. The headed app does not start the listener yet; use `mast headless` or the daemon.

## On A (the controlling machine)

```sh
mast peer add fedora ws://127.0.0.1:27700            # reads the code from stdin; `<code>` as a third argument also works
mast peer config fedora      # prints MAST_PEER_URL and MAST_PEER_TOKEN for a second `mast mcp` server
```

Register `mast mcp` with those two variables as an extra MCP server for the Manager. With `MAST_PEER_URL` set, `mast mcp` proxies that engine, presents no Run token, and behaves as the unscoped human-style server: its tools act on B's device, Spaces and worktrees. B needs its own Space for the repository.

## Running a worker on B

The Manager's second MCP server (the one with `MAST_PEER_URL`) has two extra tools for this:

1. `peer_push_branch { repo, branch, project, base? }`: bundles a local branch with `git bundle` (only commits after `base`, a revision B already has, such as `main`), sends it in chunks and returns `worktreePath`, `peerBranch` and `head`. B files it as `refs/heads/peer/<peer>/<branch>` with a Mast worktree; nothing in B's own branches or checked-out folders moves.
2. `create_chat { project, cwd: worktreePath, branch: peerBranch, parent: <the Manager's own chat id> }` starts the worker. `parent` is an opaque id here: B keeps it for report delivery and does not treat it as a chat of its own.
3. The worker commits on B and calls `report_to_parent` as usual. B keeps the report durably (`peer-devices.json`); the engine on A polls each saved peer (`peer-links.json`, written by `mast peer add`) every 3 seconds, delivers new reports into the Manager's chat as "Worker updates" (with the worker's chat id, to read through the peer MCP server) and then acknowledges them. Delivery is at least once and replay is a no-op. The same text from the same worker within 45 seconds of the first is one report (a retry after a lost reply), whenever in that window it arrives. A report waits on B while A is offline, the parent chat is archived, or the link is down (a poll passes over reports it cannot deliver, so they do not hold back later ones); it is dropped when the peer is revoked or the parent chat no longer exists on A. Nothing connects from B to A.
4. `peer_fetch_branch { repo, branch, project, base: <head from push> }`: B bundles what the branch gained and the Manager fetches it into a local `peer/<branch>` branch. It never merges, pushes, or moves a checked-out branch.

Limits: bundles up to 512 MiB, sent in 384 KiB chunks (each chunk call is safe to repeat) and never held whole in memory on A; the scratch bundle on A lives in a temp folder only its owner can open and goes with the call. A git command on B is stopped after 5 minutes and a whole apply or bundle job after 10. A peer reaches only its own `peer/<its id>/` namespace.

## What a paired peer may do

Only the methods the MCP server calls: device, engine info, chat, session and transcript watches, harness and model lists, `Mutate`, `QueueCommand` and `QueueMessage`, the report outbox and the git transfer methods (`PEER_METHODS` in `crates/engine/src/peer_link.rs`). `Mutate` is narrowed to `createChat` (hosted on this device only; `parentChatId` is stripped and kept as the opaque remote parent), `renameChat` and `setChatArchived`; `QueueCommand` to `run`, `steer`, `autoMessage`, `interrupt` and `respondInput`; any `targetDeviceId` is refused, so a peer cannot hop through this engine to another device. Workspace files, terminals, project actions, settings, engine stop and the human-only methods are refused.

Scope is per pairing. A pairing made with this release is `own`: the peer can read, steer, interrupt, rename and archive only chats it created (a registry of up to 1000 per peer, with watches filtered to them). The Spaces list stays visible to an `own` peer so it can pick a project; it does not see other chats or their transcripts. Pairings made before scopes existed read as `all` (every chat on this device) and keep working unchanged; `mast peer scope <peer> own` narrows one, and a running engine applies the change within a second, including to transcript watches already open (they end within about a second; the Manager reopens what it may still read). A peer that can start an agent can run code as this user on B, so treat pairing like handing out an SSH key whatever the scope. The listener refuses to start on a signed-in engine, because a synced engine would also sync what a peer writes and could host chats on the account's other devices.

Pairing codes and tokens are about 244 random bits. Tokens are stored hashed in `peer-devices.json` (mode 0600) in the engine data dir and expire after 30 days or 7 days idle (activity refreshes the idle clock once a minute). The pairing window lives in the same file so the CLI and the engine agree; writers take an advisory lock, and a damaged file means nobody is paired. A handshake that presents a pairing code is accepted only while a window is open. The listener serves at most 16 peer sockets, gives each 10 seconds to finish the handshake, and the engine sweeps every 2 seconds, so a revoke or expiry closes the socket and aborts its in-flight requests within a few seconds.

## Limits in this release

- Reports flow B to A by A polling; there is no push, and a Manager reaches a worker on B only through the peer MCP server it was started from.
- Required Actions do not cross: the Manager raises its own, and a worker's questions are answered with `respond_to_input` through the peer server.
- B's workers have no copy of A's Work Outline item.
- A peer's pending reports are capped at 200 on B; reports for an archived parent count toward it until the parent is unarchived or handed off.
- A report can be delivered after the Manager hands its role to another chat on A; it follows the handoff.
- The headed app does not start the listener.
- Per peer on B: at most 32 bundle files and 2 GiB of them (cleared 15 minutes after last use), 32 worktrees on `peer/` branches, and 1000 recorded chats (a record stays until the peer is revoked); two git jobs run at once across all peers. An `own` peer starts chats, and queues runs, only in a Space's folder or a worktree it sent a branch to (a run that asks for a worktree or attachments is refused), and only on its own `peer/` branches. Worktree checkouts run with repository hooks off.
- A peer that knows a full commit id B already holds can file it as its own branch and fetch it back. Ids are not guessable, so this is accepted; do not paste commit ids from private branches into a peer's chat.
- Do not run an older Mast against the same data directory after upgrading: its rewrite of `peer-devices.json` does not know `scope`, chat records or the outbox, so an `own` pairing would read as `all` after the next upgrade.
