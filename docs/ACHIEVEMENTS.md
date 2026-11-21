# Resource Monitor Achievements

Built with the `achievements` skill (rajhansgithub/Coding-Harness-Plugins, v0.3.0).
Live preview (the one living artifact): https://claude.ai/artifact/LwqFbEA6tyBFGF5jUNpCNX
Source of that page: `docs/achievements-preview.html`.

## Mode and theme

- **Mode: FREE.** Resource Monitor has no paid plan. No paid tags, no Welcome badge. Emulsion is earned (gate below).
- **Theme: Case Badges** (picked 2026-11-21 over Uptime Medals and Hall of Fame Plaques).
  Why: Resource Monitor's users build, tune and watch their own PCs. In that world people show off with the small
  metal badges stuck on the front of the case (the CPU sticker, the GPU sticker, "Designed for Windows").
  - Award = a case badge: landscape rounded-rect sticker (4:3), raised polished chrome rim, brushed metal face,
    domed clear resin coat, the badge icon raised in the middle, "RESOURCE MONITOR" etched small along the bottom.
  - Diamond / Ruby = a gunmetal badge with a border carved from one piece of the stone and a radiant-cut stone set in it.
  - Emulsion = the shared soap bubble with the Resource Monitor mark (monitor + pulse line) inside.
  - Set reward = a **Certified Build** plate: a case front panel with 4 badge slots. "Certified Gold · The Watchtower".
  - Theme contract: `{ id: 'case-badge', shape: 'badge' (3), metalLabel: 'Certified', back: 'card' (spec sheet) }`.

## Shared scoring (identical in every tool)

- XP to next level = round(60 + 40 × level^1.25).
- Flat XP (max 40 a day): +2 per finished active hour watched, +1 per alert handled or task ended, +5 for a clean day
  (a day of real use with no alert), once a day.
- Ladder XP per tier: 25, 50, 100, 250, 500, 1000, 2000. Record XP per tier: 100, 200, 400, 800, 1500, 3000, 6000.
- Titles at levels 1, 3, 6, 8, 12, 16, 20: Rookie, Tinkerer, Builder, Tuner, Overclocker, Engineer, Architect.
- Rank: Bronze 1-4, Silver 5-9, Gold 10-14, Platinum 15-19, Diamond 20-29, Ruby 30-39, Emulsion 40+ (gate open).

## What counts (quality only)

Only real use counts. Opening windows, clicking, settings changes never count.
- **Active minute**: a sample minute where the widget is shown on a taskbar AND the user gave input in the last 5 minutes.
- **Day of real use**: ≥ 30 active minutes that day. Totals only, no streaks.
- Counters are fed by the observer from the app's own events (samples, alerts, End task, updates). No timers of its own.

## Catalog (33 badges)

Thresholds are per tier: Bronze, Silver, Gold, Platinum, Diamond, Ruby, Emulsion.

### Ladders (13)
| id | Name | Counts | Tiers |
|---|---|---|---|
| watch | Watchkeeper | active hours watched | 5, 50, 250, 1000, 2500, 6000, 12000 |
| heavy | Heavy Lifting | hours with CPU ≥ 80% (active) | 1, 10, 50, 200, 600, 1500, 3000 |
| render | Render Farm | hours with GPU ≥ 50% (active) | 1, 10, 50, 200, 600, 1500, 3000 |
| mover | Data Mover | GB read + written to disk | 50, 500, 2000, 10000, 40000, 100000, 250000 |
| bandwidth | Bandwidth | GB of network traffic tracked | 10, 100, 500, 2000, 8000, 20000, 50000 |
| down | Downloader | GB downloaded | 10, 100, 400, 1500, 6000, 15000, 40000 |
| up | Uploader | GB uploaded | 1, 10, 50, 250, 1000, 3000, 8000 |
| gameon | Game On | hours of full-screen apps | 5, 50, 200, 600, 1500, 3500, 7000 |
| responder | First Responder | alerts handled (fired, then cleared within 30 min) | 1, 5, 25, 100, 300, 800, 2000 |
| taskmaster | Task Master | tasks ended from the panel (succeeded) | 1, 5, 25, 100, 300, 800, 2000 |
| regular | Regular | days of real use | 3, 15, 50, 150, 365, 730, 1460 |
| ritual | Weekly Ritual | weeks (Mon-Sun) with 3+ days of real use | 2, 8, 26, 52, 104, 208, 416 |
| stillhere | Still Here | months with a day of real use | 2, 6, 12, 24, 36, 60, 120 |

### Sets → Certified Build plates (3 × 4)
- **The Watchtower**: watch, heavy, render, mover
- **The Pipeline**: bandwidth, down, up, gameon
- **The Caretaker**: responder, taskmaster, regular, ritual
(Still Here sits outside the sets.)

### Getting started (25 XP each)
- power_on · Power On: first active hour watched.
- second_screen · Second Screen: widgets on 2+ taskbars for a full active hour.
- up_to_date · Up to Date: an automatic update installed.
- under_cap · Under the Cap: a whole billing cycle finished under your data cap.
- first_gb · First Gigabyte: 1 GB of traffic tracked.

### Mastery
- marathon · Marathon (250): 24 hours of continuous active watching in one session.
- cool_week · Cool Customer (250): 7 days of real use in a row (counting only days of real use) with no alert.
- tb_month · Terabyte Club (300): 1 TB tracked in one calendar month.
- completionist · Completionist (500): every badge you can earn in this build.

### Happy accidents (10 XP each)
- running_hot · Running Hot: GPU at 90 °C or more.
- red_line · Red Line: memory at 98% or more.
- full_house · Full House: system drive 98% full.
- gone_dark · Gone Dark: no network traffic for 10 active minutes.
- access_denied · Access Denied: End task refused by Windows.
- fumes · Running on Fumes: battery at 5% or less, unplugged.

### Secrets ("?" until earned)
- night_owl · Night Owl: real use between 2 and 4 am.
- one_year · One Year: 365 days since the first day of real use.
- spring_clean · Spring Cleaning: system drive use drops 10 points within a day.
- five_nines · Five Nines: Windows uptime of 30 days seen.

## Emulsion gate (FREE MODE)

All must hold:
1. Level ≥ 30.
2. ≥ 180 days of real use, in ≥ 6 different months.
3. ≥ 3 ladders at Ruby.
4. Tool factor: **2,000 active hours watched** and **5 TB of network traffic tracked**.

Per badge: Emulsion when the gate is open and its count ≥ its Emulsion threshold. Rank Emulsion: gate open and level ≥ 40.
Records: gate open and all 4 pieces at Emulsion. Checked by the observer after real events only.

## Storage

`%APPDATA%\ResourceMonitor\achievements.json` (roaming = follows the user) mirrored to
`%LOCALAPPDATA%\ResourceMonitor\achievements.json`. Under 4 KB. Merge = union / max (earliest dates, max tiers/counters/XP).
Pending moments in `achievements-new.json` (local). State version `v: 1`.
