# Appshots

Appshots capture an application window on the desktop and stage it in a composer
for review before sending. Enable the feature in **Settings → Appshots**, choose
a global shortcut and destination, and optionally enable capture sound. Invoke
the shortcut while another application is focused. Invoking it inside Mast is
ignored. No message is sent automatically.

## Platforms and permissions

- macOS uses ScreenCaptureKit with a bounded CoreGraphics fallback. Screen
  Recording permits pixels; optional Accessibility permission adds application
  text. The source accessibility window is retained before asynchronous capture.
- Linux supports X11 capture and compatible screenshot portals. Portal capture
  requires an advertised window target; cancellation ends the operation. Once a
  portal request starts, failure does not silently fall through to X11 capture.
  X11 text enrichment requires matching native window, process, title, and
  retained AT-SPI object identity. Portal captures omit accessibility text because
  the selected native window cannot currently be verified.
- Windows capture is not implemented.

Optional accessibility collection has a 900 ms budget. Linux races enrichment
against the overall deadline; macOS applies the remaining timeout to each queried
AX element. If enrichment is unavailable, the screenshot remains usable.
Native fallback dimensions are checked before acquisition, actual dimensions
before encoding, and encoded bytes before copying into Rust.

## Presentation and delivery

Composer and transcript cards identify the application and window, show a
contained preview, and open the full image on activation. macOS uses the locally
installed application icon where available; other clients use a source icon.
Accessibility text is agent context, not visible message text.

Queued images use small cropped thumbnails. Narrow desktop windows show one
thumbnail with an additional-image count; desktop actions retain their fixed
slots when modifier hints appear.

Visible desktop queue thumbnails have their own bounded lifecycle, independent
of eviction from the full-image cache. Full images load when opened. Failed
previews require user activation to retry. Queue editing preserves the original
Appshot context and attachment references.

Capture and settings belong to the viewer's UI. Sending uses the existing
attachment upload/host acknowledgement flow and prompt context; no RPC or
persistent schema migration is introduced. Existing settings deserialize with
backward-compatible defaults. Appshots setup controls support Tab/Shift-Tab,
Enter/Space, accessible names and state, and visible keyboard focus.

## Verification and visual fixtures

The focused final suites passed on macOS (829 UI tests), native Linux (849 UI
tests). App checks
passed on macOS and Linux. Native capture permission dialogs, real desktop
focus changes, interactive Linux portals, VoiceOver, and physical remote-device
delivery still require a live release pass; the fixtures do not establish them.

The opt-in `appshots-fixture` Rust example renders isolated native GPUI frames
from supplied PNG fixtures. Run it with an output directory and a directory
containing `wide.png`, `tall.png`, and `square.png` after building with
`cargo build -p mast-ui --example appshots-fixture --features appshots-fixture`.
It uses a temporary data directory and an ephemeral IPC listener and does not
send an agent prompt.

The initial whole-workspace review reported four failures, three reproduced on
an upstream baseline, plus existing upstream formatting differences. Focused
passing suites do not imply the global gate is clean. See the accompanying
review resolution and logs when preparing publication.
