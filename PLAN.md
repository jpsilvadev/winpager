# WinPager — plan

A Windows 11 equivalent of the KDE Plasma Pager: a strip/grid of virtual-desktop
miniatures embedded in the taskbar. Each cell shows the outlines of that desktop's
windows at their real positions, with app icons. Click a cell to switch; scroll to go
next/previous; drag a window outline to another cell to move the window.

Target: Windows 11 24H2+ (dev machine: build 26300, 3440x1440 primary, 100% DPI,
48 px taskbar at the bottom, centered taskbar icons, 7 desktops).

## Stack

Rust (edition 2024), `windows` 0.58 (same version as `winvd`, so `HWND`/`GUID` are
shared types), `winvd` 0.0.49 for the undocumented virtual-desktop COM API,
Direct2D/DirectWrite/WIC for drawing, `serde` + `toml` for config, `anyhow` for errors.
Single-threaded UI (STA) except the winvd event thread.

## Modules and owners

`src/model.rs` is the shared contract (written by the orchestrator). Do not change it
without saying so explicitly in your report; if you must add something, keep it additive.

| Module | Owner | Responsibility |
|---|---|---|
| `vd.rs` | data | desktops list/current, switch, create, remove, move window, event listener posting a message to the host window |
| `enumerate.rs` | data | `build_snapshot`: Alt-Tab-like window filtering, frame rects, desktop per window, pinned detection, z-order, foreground |
| `icons.rs` | data | per-window icon as premultiplied BGRA, cached |
| `layout.rs` | render | grid geometry (rows/cols), window rect mapping, hit testing |
| `render.rs` | render | Direct2D drawing into a premultiplied DIB + `UpdateLayeredWindow`; `render_to_png` for tests |
| `theme.rs` | render | colors from Windows light/dark setting and accent color |
| `main.rs`, `host.rs`, `taskbar.rs`, `config.rs` | host | window + message loop, taskbar embedding, input, hooks, menu, tooltip, config file |

Public signatures in the stub files are the contract between modules. Keep them; you may
add new public items. Private implementation is yours.

## Key facts already verified on the dev machine

- `winvd` works on build 26300: `get_desktops`, `get_current_desktop`, `get_desktop_by_window`,
  `is_pinned_window` all return correct values.
- Windows on non-current desktops have `DWMWA_CLOAKED == DWM_CLOAKED_SHELL (2)` and still
  report correct `DWMWA_EXTENDED_FRAME_BOUNDS`. Windows with cloak `DWM_CLOAKED_APP (1)`
  must be skipped. Minimized windows sit at (-32000,-32000) — skip them (`IsIconic`).
- Windows the desktop manager doesn't track (Program Manager, overlays, input experience)
  return `Err(WindowNotFound)` from `get_desktop_by_window` — skip them.
- winvd debug logging is disabled via a Cargo profile override.
- Taskbar: `Shell_TrayWnd` [0,1392,3440,1440]; children include `TrayNotifyWnd`
  [2798..3440] (tray) and `ReBarWindow32` [1292..1751] (task buttons). Default pager
  position: right-aligned just left of `TrayNotifyWnd`.

## Behaviour spec

- Cells: one per desktop, aspect ratio = primary monitor aspect. Grid with `rows` rows,
  `cols = ceil(n / rows)`, row-major order. Height fills the taskbar minus margins.
- Windows: drawn bottom-to-top (snapshot is topmost-first), clipped to their cell,
  filled + 1 px border; the foreground window is emphasised; icon centered when the
  outline is big enough. Pinned windows appear in every cell.
- Current desktop: accent-colored highlight. Hovered cell: subtle highlight.
- Desktop name/number drawn faintly behind windows when `show_names`.
- Click (press+release without moving beyond the drag threshold) on a cell → switch.
- Wheel over the pager → next/previous desktop (wrap configurable, invert configurable).
  Accumulate wheel deltas (precision touchpads send small deltas).
- Drag a window outline to another cell → move that window to that desktop. While dragging
  draw the outline following the cursor and highlight the target cell.
- Tooltip on hover: desktop name, plus the hovered window's title.
- Right-click menu: Add desktop, Remove this desktop (confirm if it has windows), Rows
  (1/2/3), Show icons, Show names, Placement (Taskbar / Floating), Start with Windows
  (HKCU Run key), Open config file, Exit.
- Live updates: winvd events + `SetWinEventHook` (create/destroy/show/hide/location/
  name/foreground/minimize/cloak) coalesced with a ~30 ms timer. Ignore events for
  non-top-level objects.
- Survive Explorer restarts (`TaskbarCreated` registered message), taskbar size/DPI
  changes, theme changes (`WM_SETTINGCHANGE` "ImmersiveColorSet"), and desktop count changes.
- Single instance (named mutex).

## Testing rules for agents (IMPORTANT — the user is actively using this machine)

- Never remove, rename or reorder existing virtual desktops. If you test create/remove,
  only remove a desktop you created in the same test.
- Do not switch desktops or move other apps' windows in automated tests unless your task
  explicitly says so; if you do, restore the original state immediately.
- Do not synthesize real mouse/keyboard input (`SendInput`, `mouse_event`). To test input
  handling, post messages directly to the pager's own window.
- Always kill any `winpager.exe` you start before finishing.
- Screenshots: use PowerShell + System.Drawing (`Graphics.CopyFromScreen`) to capture a
  screen region to a PNG in your worktree, then view it with the Read tool.
- `cargo build` must pass with no errors and no new warnings in files you own when you finish
  (dead-code warnings for other modules' stubs are fine). Run `cargo clippy` on your files.
- Commit your work in your worktree with a clear message when done.
