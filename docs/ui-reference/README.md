# UI design reference

Mood board for Resource Monitor's panels (details popup, data usage report).
Drop the original reference images in this folder; the notes below capture what
we take from each so the direction survives without them.

## References

| # | Reference | What we take |
|---|---|---|
| 1 | "Hyper Design" studio controls / menu popups | Compact dark panels, 1px hairline borders, small caps section labels, tight menus with icons |
| 2 | DashBurger NPS dashboard | Semantic colours (red / amber / green) for thresholds, big numbers next to small labels |
| 3 | Epicor "Customer Maintenance" | Rounded pill bars, lime accent on near-black, heavy radius cards |
| 4 | Attendance / mood tracking board | Card grid, soft rounded corners, one hot accent per card |
| 5 | MyPlanner dashboard | Big KPI numbers at top, segmented radial gauge, lime + violet on black |
| 6 | Robot "System Overview" | Ring gauge with % in the centre ("77% OPTIMAL"), 2×2 stat tiles under it, teal accent |
| 7 | Oscars dashboard | Gold accent used sparingly, list rows with thumbnails |
| 8 | "Overview" fintech (dark/light split) | Same layout in both themes, monospace numerals, striped progress bars |
| 9 | Windows 8 Alarms | Thick concentric rings, one colour per item |
| 10 | Journal app (brown) | Warm palette alternative, pill tags |
| 11 | Misty launcher | Violet accent, glassy dark overlay |
| 12 | meely agents | Teal-on-black, dotted/segmented meters, status chips (RUNNING / FAILED) |
| 13 | Windows 11 Focus sessions (Clock app) | Native Win11 look: Mica-dark cards, ring progress, Segoe UI Variable — closest to our target |

## Rules we apply

- **Background** `#1C1C1C`; **cards** `#262626` with 10–12 px radius and a 1 px `#333` hairline. Light theme mirrors it (`#F3F3F3` / `#FFFFFF`).
- **One accent per metric** (CPU blue, memory violet, GPU orange, network cyan/orange, disk yellow, data green). Amber / red only for warnings.
- **Ring gauges** for percentages (CPU, memory, GPU): thick track, rounded caps, value in the centre.
- **Big number, small label**: value ~20 px semibold, label 11 px muted, uppercase section headers with letter spacing.
- **Pills** for progress (data cap) and chips for status (uptime, process count).
- **Charts**: soft area fill + 1 px bright edge, no axes inside small cards.
- Keep it light: everything is drawn with our own anti-aliased software renderer, no images, no GPU.
