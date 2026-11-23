# Peer link: drive another Mast engine directly

Status: first release (remote worker host). Experimental, off by default.

A Manager on one machine can start and steer chats on a second machine without the hosted relay, registry sync or sign-in. The machine being driven (B) opens a listener; the controlling machine (A) pairs once and then runs a second MCP server that proxies B's engine.

## On B (the machine that builds and tests)

```sh
MAST_PEER_LISTEN=127.0.0.1:27700 mast headless      # or a tailnet address, e.g. 100.101.102.103:27700
mast peer pair                                       # prints a one-time code, valid 5 minutes, 5 attempts
mast peer list
mast peer revoke <id|label> | --all                  # the running engine closes that peer's sockets within seconds
```

The listener refuses any address that is not loopback or a tailnet address (`100.64.0.0/10`, which is also shared carrier space, so use only a tailnet address you own; or `fd7a:115c:a1e0::/48`). Mast does not encrypt this link: use Tailscale, or an SSH tunnel (`ssh -N -L 27700:127.0.0.1:27700 <B>`) to a loopback listener. The headed app does not start the listener yet; use `mast headless` or the daemon.

## On A (the controlling machine)

```sh
mast peer add fedora ws://127.0.0.1:27700            # reads the code from stdin; `<code>` as a third argument also works
mast peer config fedora      # prints MAST_PEER_URL and MAST_PEER_TOKEN for a second `mast mcp` server
```

Register `mast mcp` with those two variables as an extra MCP server for the Manager. With `MAST_PEER_URL` set, `mast mcp` proxies that engine, presents no Run token, and behaves as the unscoped human-style server: its tools act on B's device, Spaces and worktrees. B needs its own Space for the repository.

## What a paired peer may do

Only the methods the MCP server calls: device, engine info, chat, session and transcript watches, harness and model lists, `Mutate`, `QueueCommand` and `QueueMessage` (`PEER_METHODS` in `crates/engine/src/peer_link.rs`). `Mutate` is narrowed to `createChat` (hosted on this device only), `renameChat` and `setChatArchived`; `QueueCommand` to `run`, `steer`, `autoMessage`, `interrupt` and `respondInput`; any `targetDeviceId` is refused, so a peer cannot hop through this engine to another device. Workspace files, terminals, project actions, settings, engine stop and the human-only methods are refused. The scope is by method, not by chat: a peer can read, steer, interrupt and archive any chat on this device. The listener refuses to start on a signed-in engine, because a synced engine would also sync what a peer writes and could host chats on the account's other devices. A peer that can start an agent can run code as this user on B, so treat pairing like handing out an SSH key.

Pairing codes and tokens are about 244 random bits. Tokens are stored hashed in `peer-devices.json` (mode 0600) in the engine data dir and expire after 30 days or 7 days idle (activity refreshes the idle clock once a minute). The pairing window lives in the same file so the CLI and the engine agree; writers take an advisory lock, and a damaged file means nobody is paired. A handshake that presents a pairing code is accepted only while a window is open. The listener serves at most 16 peer sockets, gives each 10 seconds to finish the handshake, and the engine sweeps every 2 seconds, so a revoke or expiry closes the socket and aborts its in-flight requests within a few seconds.

## Limits in this release

- Direction is A to B only.
- Worker reports and Required Actions do not cross: A's Manager reads B's chats (`read_chat`, `wait_for_turn`) and raises its own Required Actions. `report_to_parent` still refuses across devices.
- B's workers have no copy of A's Work Outline item.
- The headed app does not start the listener.
