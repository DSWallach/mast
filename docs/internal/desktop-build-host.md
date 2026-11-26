# Desktop build host over Tailscale

Checked 2026-11-26 (read-only probes). Read this if a remote build or test on the desktop will not start. For the script's own behaviour (`start`, `wait`, `status`, `stop`, exit codes) see "Remote Cargo builds" in [docs/reference/development.md](../reference/development.md).

## What it is

The user's Fedora 45 desktop (Ryzen 9 3900X, 12 cores / 24 threads, 31 GiB RAM, one 464 GiB volume for `/` and `/home`) is a tailnet peer named `desktop`. Laptop (`fedora`/thinkpad) to desktop uses plain sshd as `david`. It is not Tailscale SSH, and it is not a paired Mast peer: nothing listens on port 27700 there, so `list_devices` shows only the local device and `run_command` and `peer_command_status` cannot target it.

## How it was set up

1. Tailscale was already up on both machines (tailnet `tail59be8a.ts.net`). `tailscale status` lists `desktop`; `tailscale ping desktop` should answer direct (about 26 ms). A first reply via DERP is normal.
2. The laptop's `~/.ssh/id_rsa.pub` (comment `david@fedora`, fingerprint `SHA256:r+DLQd2OfMVrM+cDpJlNyaJTKpbPtfaPpwBp6S5Hq0I`) was added by the user to `david@desktop:~/.ssh/authorized_keys`. Before that, ssh failed with `Permission denied (publickey,gssapi-keyex,gssapi-with-mic,password)`. The desktop host key was already in the laptop's `known_hosts`.
3. Code travels through GitHub (`origin` = `git@github.com:DSWallach/mast.git`). The desktop can reach GitHub over ssh. Push from the laptop, then on the desktop `git fetch origin <branch>` in `~/Work/mast`, and put the commit in a worktree. Do not move the desktop's main checkout. A fetch only updates `origin/<branch>`.
4. Builds run through `scripts/remote-cargo.sh <start|wait|status|stop> david@desktop ...`, which uses `~/Work/mast-target-<slot>` as `CARGO_TARGET_DIR` on the desktop.

## Quick checks when stuck

```sh
tailscale status | grep desktop            # online? "-" or "idle" is fine, "offline" is not
tailscale ping -c 3 desktop                # direct vs relayed
ssh -o BatchMode=yes -o ConnectTimeout=5 desktop true; echo $?
ssh -o BatchMode=yes desktop 'df -h /home | tail -1; pgrep -a -x cargo; pgrep -a -x rustc | head -3'
ssh -o BatchMode=yes desktop 'cd ~/Work/mast && git fetch origin <branch> && git cat-file -t <sha>'
```

| Symptom | Likely cause |
|---|---|
| `Permission denied (publickey...)` | The laptop key is missing from `david@desktop:~/.ssh/authorized_keys`, or the account or permissions changed. `ssh -vv` should show `Offering public key: ... id_rsa`. The user must fix it, so raise a Required Action. Do not edit the desktop's ssh config yourself. |
| `Host key verification failed` | You used the bare IP, which is not in `known_hosts`. Use the name `desktop`. Never add keys or disable checking without the user. |
| Connection timeout, or `tailscale status` shows offline | The desktop is off or asleep, or Tailscale is down. Ask the user to wake it. |
| `fatal: Not a valid object name <sha>` on the desktop | The commit was not pushed or fetched. Push it, then run `git fetch origin <branch>` there. |
| `no such directory on host` from `start` | The worktree path does not exist on the desktop. Create it with `git worktree add --detach <path> <sha>` there first. |
| `slot <x> busy` or `name in use` | Another job holds the slot, or the name was used. Use a new name or another slot. See the script docs. |

## Build rules on the desktop (Space instructions)

- Run `ssh desktop df -h /home` before a build. Plan for about 50 GiB free. Hold new Cargo work below 15 GiB.
- Observed at the check: 289 GiB free, with `mast-target-{1,a,b,c,installed}` using about 6.8, 42, 63, 37 and 11 GiB. These grow, and pruning needs the user's approval (see `scripts/prune-cargo-target.py` and the Space instruction on `debug/incremental`).
- One Cargo build, test or preview at a time on the device across all slots. The slots do not enforce this, so check `pgrep -a -x cargo` first. Use `CARGO_INCREMENTAL=0` for one-off verification on branches other than the active development one.
- The toolchain is cargo and rustc 1.100.0 stable, with mold through `clang` in `~/.cargo/config.toml`, sccache, and cargo-nextest. `CARGO_TARGET_DIR` is not set globally. Use the slot's value.

## Mast peer pairing

Done 2026-11-26: the desktop's `mast headless` listens on `127.0.0.1:27700` and this laptop is paired through an SSH tunnel (`localhost:27701`). The exact commands, gotchas, undo steps and persistent-tunnel options are in [peer-link.md](peer-link.md#recipe-pair-the-fedora-desktop-over-an-ssh-tunnel). The listener refuses a signed-in engine.

Full investigation write-up: the "Desktop build host via Tailscale" artifact on the work item "Investigate using the desktop over Tailscale as a Mast dev build queue host".
