# Mast iOS companion (experimental)

The app in `apps/ios` and bridge are an unverified prototype. The bridge exposes only companion operations on `127.0.0.1:27655`; the engine IPC on port 27654 is privileged and must never be exposed directly. The app defaults to built-in demo data. There is no push, attachment support, live approval, new chat or artifact viewer. Reply & resolve is disabled because desktop-parity delivery and recovery have not been established.

## Build and fixture tests

From the repository root, with Xcode 27 and Node 18:

```sh
xcodebuild -project apps/ios/Mast.xcodeproj -scheme Mast -destination 'generic/platform=iOS Simulator' -derivedDataPath /tmp/mast-ios-derived CODE_SIGNING_ALLOWED=NO build
cd apps/ios/bridge
PATH="$HOME/.nvm/versions/node/v18.20.8/bin:$PATH" npm ci
PATH="$HOME/.nvm/versions/node/v18.20.8/bin:$PATH" npm test
```

The build passed on this machine. Bridge tests use an isolated fake IPC server and fake MCP process (8 passed). This test command failed with exit 66 because the scheme has no test action:

```sh
xcodebuild -project apps/ios/Mast.xcodeproj -scheme Mast -destination 'platform=iOS Simulator,id=61F7B027-E816-4313-B0A6-975D5A95C44E' -derivedDataPath /tmp/mast-ios-derived CODE_SIGNING_ALLOWED=NO test
```

There is currently no iOS unit-test target; only an iOS 17.5 simulator runtime is installed, so the iOS 26 app has not been launched or tested in the simulator. The live engine connection, signing, phone and Tailscale paths have not been verified.

## Morning QA

1. Install an iOS 26-or-later simulator runtime in Xcode. Open `apps/ios/Mast.xcodeproj`, select the Mast scheme and an iOS 26 simulator, and run it. Demo mode displays sample requests and chats; sending in demo mode does not send a message. Check details, links as text, transcript, and the disabled reply control.
2. With Mast already running, build the bridge as above and run `PATH="$HOME/.nvm/versions/node/v18.20.8/bin:$PATH" node dist/cli.js start` from `apps/ios/bridge`. In the app's Settings, enter `http://127.0.0.1:27655` and the one-time code printed in the terminal, then pair. Check the inbox and running chats first. Send a harmless message to a disposable existing chat and inspect the result on the Mac. An error can mean the send's outcome is unknown; inspect the chat before retrying. Stop the bridge with Control-C. Run `node dist/cli.js revoke` using Node 18 to revoke **all** paired credentials. Restart the bridge to issue a fresh five-minute pairing code.
3. For an iPhone, sign the app in Xcode with your own team. Install and authenticate Tailscale on both devices separately; Tailscale is not installed or configured by this project. With the bridge running, consult the installed Tailscale version's `tailscale serve --help`, then use `tailscale serve --bg http://127.0.0.1:27655` on the Mac to provide the tailnet HTTPS endpoint, and check `tailscale serve status`. Enter that HTTPS URL and a fresh pairing code on the phone. Review existing Serve configuration before changing it. Stop the configured service using the version-specific Serve help when finished. These commands have not been exercised here.

Bridge credentials live in `~/Library/Application Support/MastCompanionBridge/`, separately from the Mast profile. The bridge logs the pairing code but not credentials or message contents. The app stores its device token in Keychain. A running Mac, Mast and bridge are required; refresh is foreground-only and stale state is shown on failure. The fixture command `node dist/cli.js fixture` serves sample content and never connects to Mast.
