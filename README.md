# Resource Monitor

Tiny, native taskbar monitor for Windows 10 and 11. Written in Rust on raw Win32.
No .NET, no driver, no admin rights, no background service.

- **~0.5 MB** single exe, no runtime, no installer required
- **A few MB of RAM**, near-zero CPU (one sample per second, coalesced timers, below-normal priority, EcoQoS)
- Works on current Windows 11 (24H2/25H2) — no WinRing0 / kernel driver that Defender blocks

<p align="center">
  <img src="docs/screenshots/taskbar-widget.png" alt="Taskbar widget" width="100%"><br>
  <img src="docs/screenshots/details-panel.png" alt="Details panel on hover" width="49%">
  <img src="docs/screenshots/data-usage-report.png" alt="Data usage report" width="49%">
</p>

<sub>Screenshots taken from the real app (dark theme). Panel design notes: <a href="docs/ui-reference">docs/ui-reference</a>.</sub>

## What it shows

Docked beside the tray, on the taskbar:

| Metric | Source |
|---|---|
| CPU % (matches Task Manager) | PDH `% Processor Utility` |
| Memory % | `GlobalMemoryStatusEx` |
| GPU % and GPU memory (NVIDIA / AMD / Intel) | PDH `GPU Engine`, same as Task Manager |
| Disk read / write / active time | PDH `PhysicalDisk` |
| Upload / download speed | `GetIfTable2` 64-bit counters |
| Data used today | exact byte counters, saved per day |
| Top process | `NtQuerySystemInformation` |
| Battery | `GetSystemPowerStatus` |

Pick any combination from **right-click → Metrics**. Default: CPU, RAM, GPU, ▲, ▼, today's download.

## Smart overlay

- **Auto-docks** next to the tray. Follows the taskbar: bottom, top, left, right, any DPI, small/large taskbar, Explorer restarts.
- **Multi-monitor**: a widget on every taskbar, or only the ones you pick (right-click → *Displays*). Needs Windows' "Show my taskbar on all displays". Each display keeps its own drag position; mixed DPI is handled per monitor.
- **Never covers your app icons**: at startup it asks the taskbar (UI Automation) where its buttons end and keeps the widget in the free space. Too many metrics? It compacts step by step — one line → pairs stacked (▲ over ▼, CPU over RAM) → groups share columns → no sparklines — and anything that still doesn't fit moves to the hover panel (shown as “+N”). Opened lots of apps? Right-click → Appearance → *Re-check taskbar icons*.
- **No background polling**: besides reading the metrics each interval, everything is event-driven — taskbar moves, auto-hide, full-screen apps, theme and settings-file changes all arrive as Windows notifications.
- **Part of the taskbar**: the docked widget is a child window of its taskbar (like a deskband), so it moves with it, hides with it behind full-screen apps, and clicks on another monitor never affect it.
- **Hides itself** for full-screen games, videos and presentations — decided per monitor from the window's real (DWM) bounds, ignoring shell surfaces like Start, Search, Alt-Tab and flyouts.
- **GPU temperature & fan** straight from the display driver (the same source as Task Manager), and **system drive space**.
- **Alerts**: CPU/RAM staying high, drive almost full, battery low, data cap at 90%/100%.
- **Stays on top of the taskbar without flicker**: the taskbar owns the widget window, so Windows keeps it directly above the taskbar — clicking the taskbar can never cover it.
- **Hover** → details panel: ring gauges for CPU / memory / GPU, live network and disk charts, data usage (today, month, cap), uptime, battery, top 5 processes by CPU, memory and disk I/O.
- **Click** → pin the panel. **Double-click** → Task Manager. **Drag** → move along the taskbar (or anywhere in floating mode).
- **Theme-aware, usage-coloured**: matches light/dark taskbar; colours ramp smoothly with usage — muted when idle, more saturated as load rises, amber at `warn_pct` (75%) and red at `crit_pct` (90%).
- **Sensors your PC lacks are hidden** automatically (no GPU counter → no GPU row; no battery → no battery row).
- **Display off** → samples every 5 s. Data usage stays exact anyway (cumulative counters).

## Data usage

- Counted from interface byte counters, so totals are exact regardless of the update interval.
- Physical adapters only — no double counting through VPN/filter/virtual adapters. Or choose one adapter.
- **Report** (right-click → *Data usage report*, double-click the tray icon, or `ResourceMonitor.exe --report`):
  today, yesterday, this/last month, year, all time, last-30-days chart, monthly and yearly tables.
- **Monthly data cap**: set `data_cap_gb` and `cap_reset_day` in the settings file → progress bar, and the widget turns amber/red as you approach it.
- Stored in `%LOCALAPPDATA%\ResourceMonitor\usage.csv` (plain CSV, atomic writes, saved every minute and on sleep/shutdown).

## Updates

The app updates itself: once at startup it asks GitHub for the latest release, downloads the new `ResourceMonitor.exe`, checks its SHA-256 against the digest GitHub publishes, swaps it in and restarts (you get a “Updated to vX” notification). Nothing runs on a timer. Right-click → *Check for updates now* any time, or untick *Update automatically*. If the app lives in a folder it can't write to (e.g. Program Files), it tells you and opens the download page instead.

## Install

1. Download `ResourceMonitor.exe` (portable) or `Setup_ResourceMonitor.exe` (per-user install, no admin) from [Releases](../../releases).
2. Run it. Right-click the widget or tray icon → **Start with Windows**.

Upgrading from the old .NET version: settings and data usage history are imported automatically on first run, and the old autostart entry is removed.

## Settings

Most options are in the right-click menu. Everything is also in `%LOCALAPPDATA%\ResourceMonitor\settings.ini` (menu → *Edit settings file*); edits apply within a few seconds.

| Key | Meaning |
|---|---|
| `show.<metric> = 1/0` | cpu, ram, gpu, vram, disk_read, disk_write, disk_active, net_up, net_down, today_up, today_down, top_process, battery |
| `speed_unit` | `auto`, `autobits`, `KB/s`, `MB/s`, `Kbps`, `Mbps` |
| `adapter` | empty = all physical adapters, or a name such as `Wi-Fi` |
| `interval_ms` | 250–10000 |
| `max_width_pct` | 0 = auto (stay clear of app icons), or a fixed % of the taskbar width |
| `stacked` | default for every display: 1 = metrics stacked in 2 rows, 0 = one line (menu: Appearance → Stacked) |
| `stacked.DISPLAY2` | per-display override (menu: Appearance → Stacked → *Display 2*) |
| `auto_stack` | 1 = a one-line widget may switch to stacked rows when it doesn't fit (default 0: extra metrics go to the hover panel) |
| `mode` | `dock` or `float` |
| `dock_offset` | gap from the tray on the main taskbar (set by dragging) |
| `displays` | `primary`, `all`, or a list such as `DISPLAY1,DISPLAY3` |
| `dock_offset.DISPLAY2` | gap from the clock on another monitor's taskbar |
| `hide_fullscreen`, `click_through` | 1/0 |
| `text_scale` | 70–200 (%) |
| `theme` | `auto`, `dark`, `light` |
| `bg_opacity` | 0 (invisible) – 255 (solid) |
| `warn_pct`, `crit_pct` | colour thresholds |
| `auto_update` | 1 = check for a new version once at startup and install it |
| `data_cap_gb`, `cap_reset_day` | monthly cap (0 = off) and billing day |

## Build

```bash
cargo build --release
```

Output: `target\release\ResourceMonitor.exe`. `publish.bat` also builds the installer when Inno Setup (`ISCC`) is on PATH. CI builds both on every push and attaches them to `v*` tags.

## What changed from 1.x

The .NET/WPF app was rewritten from scratch. Fixed along the way:

- Required admin + a kernel driver (LibreHardwareMonitor/WinRing0), now blocked by Windows Defender's vulnerable-driver list → replaced by user-mode APIs.
- 100 ms timer forcing z-order and position 10×/s → event-driven, 1 sample/s.
- Data usage summed rounded per-second *speeds* (drifted, and double-counted virtual adapters) → exact byte counters.
- Overlay covered full-screen games → auto-hides.
- Tray icon vanished after Explorer restarts → re-created.
- Prompted to "install" itself on every launch from a different folder → gone; plain portable exe.
- Overlay position broke with DPI scaling, top/side taskbars, auto-hide → handled.
- 70 MB self-contained .NET bundle → ~0.5 MB.

## License

MIT — see [LICENSE](LICENSE).
