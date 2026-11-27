<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/media/readme/mast-lockup-dark.png">
    <img src="docs/media/readme/mast-lockup-light.png" alt="Mast" width="320">
  </picture>
</p>

Run a team of coding agents from one outline of your work.

Mast is a native desktop app (macOS and Linux) for running Claude Code and Codex (plus a few other agents) in chats, and for keeping those chats organised around the work they belong to. Your sessions stay on your machine. There is no Mast account.

## Why use it

You already run several agent chats at once. Mast gives them a place to live, so you can see what is running, what needs you, and what is done.

- **Spaces.** A Space is a folder you work in. Each Space has its own outline, instructions, accounts and settings.
- **Work Outline.** Nest chats under work items: a workstream, a ticket, a PR, an investigation. An item holds instructions that its chats inherit (Space → parent items → this item), where its chats run, and a status. Keys work everywhere, and `⌘Z` undoes a move. See [the outline guide](docs/guide/work-outline.md).
- **Required Actions.** When an agent needs a decision, an approval or missing information, it files a request on its item. The item shows it until you answer. Your reply goes back to the chat.
- **Chats on Claude Code and Codex.** Each chat shows the model it actually ran on and the account it uses.
- **Orchestrator and worker chats.** A coordinator chat can read its subtree, start worker chats on items below it, and collect their results through the Mast MCP tools. Workers finish with a "Ready to merge" action. See [orchestrating](docs/guide/orchestrating.md).
- **Plans you can review.** Agents publish plans, diagrams and option comparisons. You read them in the app, answer questions inline (blocking questions become Required Actions), comment on parts, zoom, compare revisions visually and pick an option. See [plans and artifacts](docs/guide/plans-and-artifacts.md).
- **PR and Linear sync.** A PR page buckets your PRs and review requests by what needs you. Review requests can appear as outline items on their own. Linear is opt-in per Space, read-only, and you add the tickets you want. See [GitHub and Linear](docs/guide/github-and-linear.md).
- **Tracker.** Follow ticket rows, worker progress, reviews and PRs in one tab. See [orchestrating](docs/guide/orchestrating.md).
- **Per-Space accounts.** Use different Claude Code, Codex or Linear accounts in each Space. See [accounts](docs/guide/accounts.md).
- **Side browser.** Web and PR links open beside the chat.
- **Agent tools.** Every chat gets the `mast-agent` MCP server. See [docs/mcp.md](docs/mcp.md).



## Install

Mast runs on macOS and Linux and is built and updated from source. Device sync is disabled by default.

You need:

- Rust through [rustup](https://rustup.rs). `rust-toolchain.toml` pins the version.
- At least one agent CLI, signed in: [Claude Code](https://docs.claude.com/en/docs/claude-code) (`claude`) or Codex (`codex`).
- Optional: [`gh`](https://cli.github.com), signed in with `gh auth login`, for the PR page and PR sync.
- Platform build dependencies:
  - **macOS:** Xcode command line tools (`xcode-select --install`).
  - **Linux:** a C/C++ compiler, `pkg-config`, and development libraries for XCB/XKB, Fontconfig, FreeType, WebKitGTK 4.1, and JSON-GLib:
    - Ubuntu / Debian:

      ```bash
      sudo apt install build-essential pkg-config libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libfontconfig1-dev libfreetype-dev libwebkit2gtk-4.1-dev libjson-glib-dev
      ```

    - Fedora:

      ```bash
      sudo dnf install gcc gcc-c++ pkgconf-pkg-config libxcb-devel libxkbcommon-devel libxkbcommon-x11-devel fontconfig-devel freetype-devel webkit2gtk4.1-devel json-glib-devel
      ```

```bash
git clone https://github.com/Pluto8195/mast.git
cd mast                                   # the default branch is work-outline-implementation
cargo build --profile installed -p mast   # optimised build
```

Run `target/installed/mast doctor` from the checkout for a prerequisite checklist and suggested fixes (`--json` emits machine-readable results).

The binary is `target/installed/mast`.

### macOS

To get a normal app you can keep in `/Applications`, build the bundle and disk image. Build from the branch you actually run: if you use a fork or an integration branch, check that out instead of the default `work-outline-implementation`. Packaging also needs Python 3, pip and the `dmgbuild` Python package. It builds the release profile separately, so it is a large extra build on top of the optimised one, and the first packaging run compiles again. Make sure you have the disk space:

```bash
scripts/package-macos.sh
```

The script attempts to install `dmgbuild` with pip if it is missing. It writes `target/package/Mast.app` and a `.dmg` next to it. With `CARGO_TARGET_DIR` set, outputs land under that directory instead (`package/Mast.app`, `installed/mast`), not `./target`. Copy the app to `/Applications`. The bundle is ad-hoc signed, so the first time you open it, right-click it and choose Open.

To try it without touching an installed Mast, run the isolated dev app:

```bash
scripts/run-macos-dev.sh
```

It builds a debug binary and opens `Mast Dev.app` with its own data folder (`<home>/.mast-dev`), bundle id and engine port. Set `MAST_DEV_DATA_DIR` or `MAST_DEV_IPC_PORT` to change them.

### Linux

To install the binary into `~/.local/bin/mast` along with its icons and desktop launcher entry (`~/.local/share/applications/mast.desktop`), run:

```bash
scripts/install-linux-app.sh
```

You can also pass a different binary path (for example `scripts/install-linux-app.sh target/release/mast`), or run `target/installed/mast` directly. At runtime, the desktop UI uses Vulkan (or an OpenGL fallback) and the side browser needs your distribution's WebKitGTK 4.1 and JSON-GLib runtime packages; see [docs/reference/linux-browser.md](docs/reference/linux-browser.md).

To try a debug build without touching your `~/.mast` data or a running instance, point `MAST_DATA_DIR` and `MAST_IPC_PORT` at a separate profile:

```bash
cargo build -p mast
MAST_DATA_DIR=~/.mast-dev MAST_IPC_PORT=49777 target/debug/mast
```

To run the engine in the background without the UI, `mast daemon install` (or `target/installed/mast daemon install`) installs and starts a `systemd --user` service running `mast headless`. The unit is `mast.service`, and it reads optional environment from `~/.mast/env`. Mast never stops a service from an earlier build: if you still run an old `zeron.service`, stop it yourself first with `systemctl --user disable --now zeron.service`. If the engine port is already in use, `mast daemon install` fails and changes nothing.

## Updating

### macOS

Pull and rebuild, then package again and replace the app. Pull the branch you actually run (a fork or integration branch, if that is what you use), not necessarily the default:

```bash
git pull
scripts/package-macos.sh
```

Quit Mast, copy the new `target/package/Mast.app` over the one in `/Applications`, and open it. With `CARGO_TARGET_DIR` set, look under that directory (`package/Mast.app`) instead. Your data stays in `<home>/.mast`.

Packaging builds the release profile separately, so it is a large extra build. Make sure you have the disk space.

To skip repackaging, install a new binary into an existing Mast.app:

```bash
cargo build --profile installed -p mast
scripts/install-macos-app.sh target/installed/mast /Applications/Mast.app computer.mikey.mast.dev
```

The arguments are the built binary, the app path and the bundle id. With `CARGO_TARGET_DIR` set, the binary is `installed/mast` under that directory. The script refuses unless that exact binary has passed the perf gate: release and budget tests plus a real-data soak, with stamps in `target/perf-gate/` (see [docs/reference/development.md](docs/reference/development.md)). `--skip-perf-gate "<reason>"` installs anyway and records a logged override.

### Linux

Pull, rebuild, and reinstall the binary:

```bash
git pull
cargo build --profile installed -p mast
scripts/install-linux-app.sh
```

Quit and reopen Mast. Your data stays in `~/.mast`.

### Sharing a build instead

On macOS, if someone can't build from source, run `scripts/package-macos.sh` on your Mac and send them the DMG. It only runs on Macs with the same architecture as yours (Apple silicon or Intel). The app is ad-hoc signed, so on first launch they right-click Mast and choose **Open**. If macOS still blocks it, run `xattr -dr com.apple.quarantine /Applications/Mast.app`. Notarised builds are not available yet.

## First run

1. Open Mast. Add a Space with `⇧⌘N`, or **New Space…** in the Space picker, and choose your repo folder.
2. Sign in to an agent CLI, or add an account for the Space (see [accounts](docs/guide/accounts.md)). If Mast detects a signed-out account when starting a chat, it offers **Sign in…** and **Start anyway**.
3. Press `⌘N`, choose a model, and send a message. Or open the Outline with `⇧⌘L`, create an item with `⇧⌘I`, and start a chat in it with `⌘↩`.
4. Optional: for Linear, add an account in **Settings → Linear**, then switch on **Linear** and select the account in Space details. See [GitHub and Linear](docs/guide/github-and-linear.md).

[Getting started](docs/guide/getting-started.md) walks through this with a first chat.

## Keys to learn first

| Key | Does |
| --- | --- |
| `⌘K` | Command palette |
| `⇧⌘L` | Open the Outline |
| `⇧⌘I` / `N` | New work item |
| `⌘↩` | Start a chat in the selected item |
| `⌘;` | Jump between a chat and its outline row |
| `⇧⌘↩` | Open the chat full screen |
| `⌘I` | Work Item Details for the selected row |
| `⌘Z` | Undo the last outline change |
| `⌘P` | PR page |
| `⌥⌘L` | Inbox column (Linear and GitHub) |
| `?` | Shortcut reference, in the outline |

The full list is in [docs/guide/keyboard.md](docs/guide/keyboard.md).

## Docs

Guides (see the [guide index](docs/guide/README.md)):

- [Getting started](docs/guide/getting-started.md)
- [What changes when you update](docs/guide/updating-to-mast.md)
- [The Work Outline](docs/guide/work-outline.md)
- [Orchestrating with coordinator and worker chats](docs/guide/orchestrating.md)
- [Plans and artifacts](docs/guide/plans-and-artifacts.md)
- [GitHub and Linear](docs/guide/github-and-linear.md)
- [Review a change](docs/guide/review.md)
- [Respond to Needs you](docs/guide/needs-you.md)
- [Blueprints and child templates](docs/guide/templates.md)
- [Friction and menu bar](docs/guide/friction-and-status.md)
- [Accounts](docs/guide/accounts.md)
- [Keyboard](docs/guide/keyboard.md)

Reference:

- [docs/work-outline.md](docs/work-outline.md): every key, menu and rule of the outline.
- [docs/pr-review.md](docs/pr-review.md): the review PR launcher.
- [docs/planning-artifact-conventions.md](docs/planning-artifact-conventions.md): how agents publish plans.
- [docs/mcp.md](docs/mcp.md): the MCP server that serves the agent tools.
- [Developer tooling](docs/reference/development.md): release checks, headless UI driver and previews.
- [ARCHITECTURE.md](ARCHITECTURE.md): how the engine and UI fit together.

## Credits

Mast began as a fork of [Zeron](https://github.com/zeronsh/zeron), created by Wing and contributors. Zeron provided the engine, Loro-based sync and documents, agent adapters, and chat UI. Mast builds on that foundation with the Work Outline, Overview, Needs you, PR review, Linear integration, Horizon styles, and a new iOS companion (replacing Zeron's iOS app).

[Tod](https://github.com/mentics/tod) inspired parts of the outline keyboard navigation and Linear integration. Those ideas were reimplemented; no Tod code was copied.

Zeron is MIT licensed, copyright 2026 Wing. Mast is also licensed under the [MIT License](LICENSE). See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
