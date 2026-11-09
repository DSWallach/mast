# Packaging

## macOS

On a Mac with Rust, Xcode command line tools, Python 3 and pip, run:

```sh
scripts/package-macos.sh
```

The script builds the host architecture's release binary and creates `target/package/Mast.app`, `zeron-<version>-macos-<arch>.dmg`, and `zeron-<version>-macos-<arch>-app.tar.gz`. If `CARGO_TARGET_DIR` is set, the package directory is under that target directory. The script installs Python's `dmgbuild` if needed.

The bundle contains `Contents/MacOS/mast` and a compatibility symlink named `zeron`. The release filenames and the update archive's internal `Zeron.app` directory preserve updater compatibility. The DMG volume and app are named Mast.

By default the bundle is ad-hoc signed. On first launch, right-click and choose Open. Set `CODESIGN_IDENTITY` for Developer ID signing. Set `NOTARY_KEY_PATH`, `NOTARY_KEY_ID` and `NOTARY_ISSUER_ID` together to notarize and staple the app and DMG.
