# Windows development

Windows is outside Mast’s current macOS-only release scope. The notes below
describe the retained upstream source support; Mast does not provide Windows
packages or in-app updates.

## Build and run

Install stable MSVC Rust, Visual Studio C++ build tools, Windows SDK, CMake,
and Git for Windows, then run:

```powershell
cargo run --locked -p mast
```

Close the app before rebuilding. For release builds, use
`cargo build --release --locked -p mast`. If shader compiler discovery fails,
set `GPUI_FXC_PATH` to the Windows SDK's `fxc.exe`.

## Configuration and agent support

| Setting | Behavior |
| --- | --- |
| Application data | `%LOCALAPPDATA%\Zeron`, falling back to `%USERPROFILE%\AppData\Local\Zeron`. Override with `MAST_DATA_DIR`. |
| Managed adapters | `MAST_ADAPTERS_DIR`, then `MAST_DATA_DIR/adapters`, then the default application's `adapters` directory. |
| Provider credentials | Keep their provider-owned locations; changing Mast's data root does not migrate them. |
| `CODEX_EXECUTABLE` | Executable override. `.exe` (and `.com`) launch directly; `.cmd`/`.bat` shims launch through a wrapped `cmd.exe` with literal, individually escaped arguments. The override must exist on disk. |

ACP, Claude, Codex, and opencode search PATH and known native installation
directories. Discovery is PATHEXT-aware: npm's `.cmd` shims (and any `.bat`)
resolve like `cmd.exe` would — per directory, extensions in PATHEXT order —
and spawn through `cmd.exe /e:ON /v:OFF /d /c` inside the same Job Object, so npm-
installed agents (`codex`, `opencode`, `pi-acp`, a bare `npm i -g grok`)
work without following `node_modules` payloads. Batch arguments containing
CR/LF and batch executable paths containing percent expansion syntax are rejected.
GUI launches additionally
backfill `%APPDATA%\npm`, `%LOCALAPPDATA%\{pnpm,Programs\nodejs,Volta\bin}`,
scoop shims, and `%USERPROFILE%\{.local\bin,.bun\bin}` from PATH. Managed
JavaScript adapters run through Node; installation requires `node.exe` beside
`node_modules/npm/bin/npm-cli.js`. Volta's selected tool-image npm location is
unsupported.

An OS file lock prevents engines from sharing a profile; `engine.lock.pid` is
only diagnostic. Terminals use ConPTY. Windows agents, login commands, adapter
installs, and terminals own their child process trees through Job Objects.
Terminal close waits up to five seconds for cleanup and reports failure;
shutdown/drop log failures. Processes started through external services or
brokers are outside this ownership. Closing a terminal or exiting its shell also
terminates processes detached inside that job (including `start` children).

Frosted window chrome uses native Acrylic and requires Windows **Settings >
Personalization > Colors > Transparency effects**. Content cards and popovers
remain opaque because in-app backdrop blur is not supported. The pinned
[Zui DirectX fix](https://github.com/zeronsh/zui/pull/7) supplies the renderer
layout and edge-fade corrections.

## Verification

The upstream Windows CI workflow is not included in Mast. These historical commands describe the upstream checks; Windows is outside the current macOS-only scope:

```powershell
cargo test --locked -p mast-engine -p mast-harness --lib
cargo test --locked -p mast-harness --features native-fixture --test codex_availability --test windows_native
cargo test --locked -p mast-engine --test codex_catalog --test codex_login_resolution --test auth
```

Fixtures use synthetic agents, so these tests do not establish authenticated
provider compatibility.

Remaining acceptance work includes authenticated provider runs and shutdown,
broader GPU/DPI coverage, accessibility, native browser and cross-device parity,
concurrent-launch log rotation, and invalid-window teardown diagnostics.
