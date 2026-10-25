# WinPager

A virtual-desktop pager for the Windows 11 taskbar, modelled on the KDE Plasma Pager.

It shows one miniature per virtual desktop, right inside the taskbar. Each miniature draws
the outlines of that desktop's windows at their real positions, with app icons. The current
desktop is highlighted in your accent color.

https://github.com/user-attachments/assets/b0ff6172-9d83-43af-8b94-488b317e436c

## Features

- **Switch desktops:** click a desktop, or scroll the mouse wheel over the pager.
- **Move windows:** drag a window outline onto another desktop to move that window there.
- **Tooltips:** hover to see the desktop name and the title of the window under the cursor.
- **Lives in the taskbar:** the pager is embedded in the taskbar, between the app buttons
  and the tray, and never overlaps either. It follows the button group as apps open and close.
- **Handles many desktops:** when desktops don't fit, the miniatures first get narrower.
  Past a minimum width, a sliding window of desktops is shown, with `<` / `>` chevrons for
  the hidden ones.
- **Floating mode:** an optional free-floating widget instead of the taskbar placement.
  Move it with **Alt + left-drag**. It can be always-on-top or a normal window, and its
  position is remembered.
- **Follows Windows:** light and dark theme, accent color, and per-monitor DPI.
- **Resilient:** survives Explorer restarts.

## Requirements

- Windows 11 24H2 or later (tested on build 26300).
- A [Rust](https://rustup.rs) toolchain (MSVC target) to build it.

Virtual desktops are controlled through the undocumented Windows COM interfaces, via the
[`winvd`](https://crates.io/crates/winvd) crate. A future Windows update may change those
interfaces and break desktop listing or switching until `winvd` is updated.

## Build and run

```sh
cargo build --release
target\release\winpager.exe
```

Only one instance runs at a time. To quit, right-click the pager → **Exit**.

To start it automatically, right-click → **Start with Windows**. This adds a
`HKCU\...\Run` entry pointing at the exe's current location, so move the exe to its final
location first.

## Right-click menu

| Item | What it does |
|---|---|
| Add desktop / Remove this desktop | Create a desktop at the end, or remove the one under the cursor. Removal asks first if the desktop still has windows. |
| Show icons / Show names | Toggle app icons and desktop labels in the miniatures. |
| Placement → Taskbar / Floating | Embed in the taskbar, or use a free widget. |
| Always on top | Floating mode only. |
| Reset floating position | Floating mode only. |
| Chevron click → Step / Jump | Whether a chevron moves one desktop, or jumps to the nearest hidden desktop. |
| Start with Windows | Launch at sign-in. |
| Open config file | Opens `config.toml`. |
| Exit | Quit WinPager. |

## Configuration

Settings live in `%APPDATA%\winpager\config.toml`. Changes are picked up live, and menu
changes are written back to this file. Every key is optional.

```toml
placement = "taskbar"      # "taskbar" | "floating"
anchor = "tray"            # taskbar mode: "tray" (right of the app buttons) | "left" (after Widgets)
offset = 8                 # gap from the anchor, in DIPs
wrap_scroll = false        # scrolling past the last desktop wraps to the first
invert_scroll = false
chevron = "step"           # "step" (one desktop per click) | "jump" (nearest hidden desktop)
floating_topmost = true
# floating_pos = [x, y]    # floating mode: top-left corner, physical pixels (set by Alt + drag)

[layout]
spacing = 0.0              # gap between miniatures, DIPs
vertical_margin = 7.0      # space above and below the miniatures; larger = smaller miniatures
# cell_height = 34.0       # fixed miniature height, DIPs (overrides vertical_margin; centered)
# cell_width = 84.0        # fixed miniature width, DIPs (default: follows the screen's shape)
corner_radius = 0.0        # rounded corners of the miniatures, DIPs; 0 = square
show_icons = true
show_names = false
min_cell_width = 0.8       # narrowest miniature (fraction of normal width) before sliding; 0.3–1.0
```

The log is at `%LOCALAPPDATA%\winpager\winpager.log`. It is recreated on every start.

## How it works

| Module | Role |
|---|---|
| `vd.rs` | Virtual-desktop operations and change events (`winvd`) |
| `enumerate.rs` | Alt-Tab-style window list per desktop: frame bounds, z-order, pinned windows |
| `icons.rs` | Window and app icons (including packaged/UWP apps), cached as premultiplied BGRA |
| `layout.rs` | Strip geometry: shrinking, the sliding window, and hit testing |
| `render.rs`, `theme.rs` | Direct2D drawing into a per-pixel-alpha layered window, with system theme colors |
| `taskbar.rs`, `taskbar_space.rs` | Taskbar geometry, and the free span between the app buttons and the tray, measured with UI Automation on a background thread |
| `host.rs`, `menu.rs`, `tooltip.rs`, `config.rs` | Windows, message loop, input, live updates, menu, and settings |

How the taskbar embedding works:

- In taskbar mode the pager is a layered child window of `Shell_TrayWnd`.
- A hidden top-level window receives the system broadcasts that the child can't
  (`TaskbarCreated`, `WM_SETTINGCHANGE` and so on).
- A child window of another process doesn't reliably receive wheel input. While the
  cursor is over the pager, a low-level mouse hook catches the wheel instead.
- Window changes are tracked with `SetWinEventHook` and coalesced into roughly 30 ms
  refreshes.

### Developer tools

```sh
cargo test
cargo run --example dump           # print the desktop/window snapshot and timings (--icons sheet.png)
cargo run --example render_demo    # render sample pagers to PNGs in target/render_demo
cargo run --example uia_dump       # print the taskbar's UI Automation tree
cargo run --example desk_info      # print the desktop count and current desktop; pass an index to switch
cargo run --example desk_test -- 5 8   # add 5 desktops at the end, wait 8 s, remove them
```

## Known limitations

- Only the primary monitor is shown.
- The taskbar is assumed to be horizontal (bottom or top).
- Memory use is about 65 MB. The shell icon loader, Direct2D and UI Automation account
  for most of it.
- Auto-hide taskbars and live DPI changes have had little testing.
