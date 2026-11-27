# Packaging

## macOS

On a Mac with Rust, Xcode command line tools, Python 3 and pip, run:

```sh
scripts/package-macos.sh
```

The script builds the host architecture's release binary and creates `target/package/Mast.app`, `mast-<version>-macos-<arch>.dmg`, and `mast-<version>-macos-<arch>-app.tar.gz`. If `CARGO_TARGET_DIR` is set, the package directory is under that target directory. The script installs Python's `dmgbuild` if needed.

The bundle contains `Contents/MacOS/mast` only; there is no `zeron` executable alias. The update archive holds a top-level `Mast.app`. The DMG volume and app are named Mast.

The package script and `scripts/install-macos-app.sh` also stop creating the `zeron` symlink, and the installer removes a stale one from an existing bundle.

By default the bundle is ad-hoc signed. On first launch, right-click and choose Open. Set `CODESIGN_IDENTITY` for Developer ID signing. Set `NOTARY_KEY_PATH`, `NOTARY_KEY_ID` and `NOTARY_ISSUER_ID` together to notarize and staple the app and DMG.

## Background service

`mast daemon install` writes `mast.service` (a `systemd --user` unit on Linux; the `computer.mikey.mast` LaunchAgent on macOS) that runs `mast headless`. The Linux unit reads optional settings from `~/.mast/env`.

Mast never stops or removes a service from an earlier build. If you still have a `zeron.service` (or the macOS `sh.zeron.app` launch agent), stop it yourself before installing:

```sh
systemctl --user disable --now zeron.service          # Linux
launchctl bootout gui/$(id -u)/sh.zeron.app           # macOS
```

If the engine port is already taken, `mast daemon install` refuses with an error and changes nothing. It never stops the other process. The port it checks is the one the service will use: a `MAST_IPC_PORT` line in `~/.mast/env` (Linux) overrides the shell value, which overrides the default 27654. A leftover `zeron.service` under `/etc/systemd/user` is detected too; stop it with `systemctl --user stop zeron.service`.

## Source builds and updates

Mast has one executable, `mast` (`cargo build -p mast` builds nothing else). The engine does not poll for releases and the desktop UI opens no update watch: update by pulling and rebuilding. `mast update` only says so.
