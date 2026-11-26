# What changes when you update

Mast 0.3.0 runs with standalone Mast-owned storage, preferences, and credentials.

## Profile and data directory

Mast uses `<home>/.mast` by default (and `<home>/.mast-dev` for macOS
development builds). Set `MAST_DATA_DIR` (or `MAST_DEV_DATA_DIR` for
`scripts/run-macos-dev.sh`) to choose a different location.

Existing native `<home>/.mast` profiles continue to load in place without
moving any data. Mast no longer adopts or falls back to `<home>/.comet-native`,
`<home>/.zeron`, `<home>/.zeron-dev`, `ZERON_DATA_DIR`, or `ZERON_DEV_DATA_DIR`.
Any legacy directories already on disk are left untouched and are neither read
nor modified by Mast.

## Preferences and UI settings

Headed macOS launches no longer import defaults from legacy `sh.zeron.app`
preference domains, and `ui-settings.json` no longer honors the retired
`openWebLinksInZeron` alias (use `openWebLinksInMast` instead). Existing
Mast-native preferences in `<home>/.mast/ui-settings.json` continue to load as
before.

macOS permissions (Accessibility, Screen Recording, Automation, and
Notifications) remain scoped to Mast's app identity (`computer.mikey.mast`).

## Linear and agent credentials

On macOS, Mast reads and writes Linear API keys only under the
`computer.mikey.mast.linear` Keychain service (or from the `LINEAR_API_KEY`
environment variable) and no longer reads or copies `sh.zeron.linear`.

- Keys already saved in `computer.mikey.mast.linear` continue to work.
- If your Linear key was previously stored only under `sh.zeron.linear`, add it
  in **Settings → Linear** (or set `LINEAR_API_KEY`). Existing legacy Keychain
  items are left untouched.
