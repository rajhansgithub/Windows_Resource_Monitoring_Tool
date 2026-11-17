//! Settings window in the style of Windows 11 Settings: a navigation pane on
//! the left, a scrollable page of "setting cards" on the right and an
//! OK / Cancel / Apply footer.
//!
//! Almost everything visible is custom painted (double-buffered through
//! `render::Canvas`, so edges are anti-aliased): the navigation pane, the
//! cards and their text, toggle switches, colour swatches and the slider.
//! The real input controls (combo, edit, up-down, trackbar, push buttons) stay
//! native so keyboard, accessibility and IME behave as users expect.
//!
//! The window edits a *copy* of the settings. OK / Apply merge that copy into
//! the live settings via `App::apply_new_settings`; Cancel throws it away.
//!
//! Window tree:
//! ```text
//! main (class ResourceMonitor.Settings)      title, footer strip, OK/Cancel/Apply
//!   nav  (ResourceMonitor.SettingsNav)       owner-painted page list
//!   page (ResourceMonitor.SettingsPage)      scrolling container, paints the cards
//!     ...controls of every page (only the current page is visible)
//! ```
//! The page container forwards its children's notifications (WM_COMMAND,
//! WM_NOTIFY, WM_CTLCOLOR*, WM_DRAWITEM, WM_HSCROLL) to the main window, so
//! all behaviour lives in one window procedure.
//!
//! Re-entrancy rules (single UI thread, raw Win32):
//! * `ui()` borrows are short; none is held across a call that can pump
//!   messages (`ChooseColorW`, `DestroyWindow`, `apply_new_settings`).
//! * Flags that message handlers read while we are inside a Win32 call that
//!   re-enters the window procedure (`LOADING`, `READY`) are atomics.
//! * Controls are looked up by id (`GetDlgItem`), not stored.

use crate::app::{app, default_accent, effective_dark};
use crate::fmt::SpeedUnit;
use crate::popup::style_window;
use crate::render::{create_font, ui_face, Canvas, Rgba};
use crate::settings::{Metric, Settings, Theme};
use crate::util::*;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering::Relaxed};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::{DwmGetColorizationColor, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::Dialogs::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetCapture, GetFocus, GetKeyState, IsWindowEnabled, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL, VK_DOWN, VK_END,
    VK_HOME, VK_LEFT, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "ResourceMonitor.Settings";
const NAV_CLASS: &str = "ResourceMonitor.SettingsNav";
const PAGE_CLASS: &str = "ResourceMonitor.SettingsPage";

// ------------------------------------------------------------------ layout
// All values are device-independent pixels (96 dpi); `Ui::sc` scales them.

const CLIENT_W: i32 = 760;
const CLIENT_H: i32 = 560;
const NAV_W: i32 = 200;
const TITLE_H: i32 = 64; // page title strip above the cards
const FOOTER_H: i32 = 64;
const PAD: i32 = 24; // page side padding
const CARD_PAD: i32 = 16; // inside a card
const ROW_H: i32 = 60; // card with a description
const ROW_H_SLIM: i32 = 52; // card without
const ROW_GAP: i32 = 4;
const HEAD_H: i32 = 44; // section heading
const NOTE_H: i32 = 34; // intro line under the title
const CELL_GAP: i32 = 8;
const NAV_ITEM_H: i32 = 40;
const COMBO_H: i32 = 24;
const EDIT_W: i32 = 72;
const SPIN_W: i32 = 18;

// ------------------------------------------------------------------ ids

const IDOK: i32 = 1;
const IDCANCEL: i32 = 2;
const ID_APPLY: i32 = 3;
const ID_NAV: i32 = 40;
const ID_PAGE: i32 = 41;

const ID_INTERVAL: i32 = 100;
const ID_UNIT: i32 = 101;
const ID_SCALE: i32 = 102;
const ID_THEME: i32 = 103;
const ID_OPACITY: i32 = 104;
const ID_FONT: i32 = 105;
const ID_SPARK: i32 = 106;
const ID_LABELS: i32 = 107;
const ID_STACKED: i32 = 108;
const ID_AUTOSTACK: i32 = 109;
const ID_FULLSCREEN: i32 = 110;
const ID_CLICKTHRU: i32 = 111;
const ID_AUTOUPDATE: i32 = 112;
const ID_AUTOSTART: i32 = 113;

const ID_SWATCH: i32 = 300; // + metric index
const ID_CRESET: i32 = 330; // + metric index
const ID_WINACCENT: i32 = 360;
const ID_RESETALL: i32 = 361;

const ID_ALERTS: i32 = 400;
const ID_A_CPU: i32 = 401;
const ID_A_RAM: i32 = 402;
const ID_A_SECS: i32 = 403;
const ID_A_DISK: i32 = 404;
const ID_A_BAT: i32 = 405;
const ID_A_CAP: i32 = 406;
const ID_WARN: i32 = 407;
const ID_CRIT: i32 = 408;

const ID_CAP: i32 = 600;
const ID_DAY: i32 = 601;
const ID_ADAPTER: i32 = 602;
const ID_REPORT: i32 = 603;

// Metrics page: controls are addressed by list *position*, not by metric.
const ID_MTOG: i32 = 700;
const ID_MUP: i32 = 720;
const ID_MDN: i32 = 740;

/// Up-down control for edit `id` has id `id + SPIN`.
const SPIN: i32 = 1000;

const PAGES: [&str; 5] = ["General", "Metrics", "Colours", "Alerts", "Data"];
/// Segoe Fluent Icons / Segoe MDL2 Assets glyphs of the navigation items.
const NAV_GLYPHS: [char; 5] = ['\u{E713}', '\u{E9D9}', '\u{E790}', '\u{EA8F}', '\u{E8F1}'];

const INTERVALS: [(&str, u32); 4] = [("0.5 seconds", 500), ("1 second", 1000), ("2 seconds", 2000), ("5 seconds", 5000)];
const THEMES: [(&str, Theme); 3] = [("Match Windows", Theme::Auto), ("Dark", Theme::Dark), ("Light", Theme::Light)];
const FONTS: [&str; 5] = ["Default (Segoe UI Variable)", "Segoe UI", "Cascadia Mono", "Consolas", "Bahnschrift"];
/// Toggle switches (besides the per-metric ones); their clicks mark the window as changed.
const TOGGLE_IDS: [i32; 11] = [
    ID_SPARK,
    ID_LABELS,
    ID_STACKED,
    ID_AUTOSTACK,
    ID_FULLSCREEN,
    ID_CLICKTHRU,
    ID_AUTOUPDATE,
    ID_AUTOSTART,
    ID_ALERTS,
    ID_A_CAP,
    ID_WINACCENT,
];

// Misc constants missing from windows-sys.
const DC_HASDEFID: u32 = 0x534B;
const TBM_GETPOS: u32 = 0x0400;
const CBS_AUTOHSCROLL: u32 = 0x0040;
const WM_APP_ENSURE: u32 = WM_APP + 1; // scroll the focused control into view

// ------------------------------------------------------------------ state

static READY: AtomicBool = AtomicBool::new(false);
static LOADING: AtomicBool = AtomicBool::new(false);
static PAGE_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static mut UI: *mut Ui = std::ptr::null_mut();

#[allow(static_mut_refs)]
fn ui() -> Option<&'static mut Ui> {
    unsafe { UI.as_mut() }
}

#[allow(static_mut_refs)]
fn free_ui() {
    unsafe {
        let p = std::ptr::replace(std::ptr::addr_of_mut!(UI), std::ptr::null_mut());
        if !p.is_null() {
            let u = Box::from_raw(p);
            u.fonts.free();
            DeleteObject(u.br_bg);
            DeleteObject(u.br_card);
            DeleteObject(u.br_input);
        }
    }
    PAGE_HWND.store(std::ptr::null_mut(), Relaxed);
    READY.store(false, Relaxed);
}

// ------------------------------------------------------------------ palette

const fn hx(v: u32) -> Rgba {
    Rgba::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// Colours of the window (same family as `popup::palette`).
#[derive(Clone, Copy)]
struct Pal {
    bg: Rgba,
    nav_sel: Rgba,
    nav_hover: Rgba,
    card: Rgba,
    border: Rgba,
    input: Rgba,
    fg: Rgba,
    /// Secondary text.
    dim: Rgba,
    /// Disabled text and outlines.
    off: Rgba,
    footer: Rgba,
    line: Rgba,
    /// Fill of small glyph buttons.
    chip: Rgba,
    /// Outline of an "off" toggle, and its knob.
    toggle_edge: Rgba,
    knob_off: Rgba,
    accent: Rgba,
    on_accent: Rgba,
}

impl Pal {
    fn new(dark: bool, accent: Rgba) -> Pal {
        let lum = (accent.0 as u32 * 299 + accent.1 as u32 * 587 + accent.2 as u32 * 114) / 1000;
        let on_accent = if lum > 150 { hx(0x000000) } else { hx(0xFFFFFF) };
        if dark {
            Pal {
                bg: hx(0x202020),
                nav_sel: hx(0x2D2D2D),
                nav_hover: hx(0x282828),
                card: hx(0x2B2B2B),
                border: hx(0x383838),
                input: hx(0x323232),
                fg: hx(0xFFFFFF),
                dim: hx(0xC5C5C5),
                off: hx(0x717171),
                footer: hx(0x1B1B1B),
                line: hx(0x303030),
                chip: hx(0x363636),
                toggle_edge: hx(0x9D9D9D),
                knob_off: hx(0xD0D0D0),
                accent,
                on_accent,
            }
        } else {
            Pal {
                bg: hx(0xF3F3F3),
                nav_sel: hx(0xE6E6E6),
                nav_hover: hx(0xECECEC),
                card: hx(0xFBFBFB),
                border: hx(0xE5E5E5),
                input: hx(0xFFFFFF),
                fg: hx(0x1A1A1A),
                dim: hx(0x5D5D5D),
                off: hx(0xA3A3A3),
                footer: hx(0xEDEDED),
                line: hx(0xDDDDDD),
                chip: hx(0xF5F5F5),
                toggle_edge: hx(0x6B6B6B),
                knob_off: hx(0x5A5A5A),
                accent,
                on_accent,
            }
        }
    }
}

/// The Windows accent colour (lightened a little on dark backgrounds, as the
/// system's own controls do), with the stock blues as a fallback.
fn accent_color(dark: bool) -> Rgba {
    let (mut c, mut opaque) = (0u32, 0i32);
    if unsafe { DwmGetColorizationColor(&mut c, &mut opaque) } == 0 {
        let a = hx(c & 0xFF_FFFF);
        return if dark { a.mix(hx(0xFFFFFF), 0.28) } else { a };
    }
    if dark {
        hx(0x60CDFF)
    } else {
        hx(0x0078D4)
    }
}

// ------------------------------------------------------------------ dark mode

/// Undocumented uxtheme ordinals every dark-mode Win32 app (Explorer,
/// Notepad++, ...) uses. All of it is optional: when the functions are missing
/// (Windows before 1809) nothing happens and the light theme is used.
mod darkmode {
    use super::*;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

    type SetMode = unsafe extern "system" fn(i32) -> i32;
    type AllowWindow = unsafe extern "system" fn(HWND, BOOL) -> BOOL;
    type Flush = unsafe extern "system" fn();

    struct Api {
        build: u32,
        set_mode: Option<SetMode>,
        allow_window: Option<AllowWindow>,
        flush: Option<Flush>,
    }

    fn build_number() -> u32 {
        unsafe {
            let lib = GetModuleHandleW(wide("ntdll.dll").as_ptr());
            if lib.is_null() {
                return 0;
            }
            let Some(f) = GetProcAddress(lib, b"RtlGetVersion\0".as_ptr()) else { return 0 };
            let f: unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32 = std::mem::transmute(f);
            let mut v: OSVERSIONINFOW = std::mem::zeroed();
            v.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
            if f(&mut v) == 0 {
                v.dwBuildNumber
            } else {
                0
            }
        }
    }

    fn api() -> &'static Api {
        static A: OnceLock<Api> = OnceLock::new();
        A.get_or_init(|| unsafe {
            let build = build_number();
            let mut a = Api { build, set_mode: None, allow_window: None, flush: None };
            if build < 17763 {
                return a;
            }
            let lib = LoadLibraryW(wide("uxtheme.dll").as_ptr());
            if lib.is_null() {
                return a;
            }
            // MAKEINTRESOURCE(ordinal)
            a.allow_window = GetProcAddress(lib, 133usize as *const u8).map(|f| std::mem::transmute::<_, AllowWindow>(f));
            a.set_mode = GetProcAddress(lib, 135usize as *const u8).map(|f| std::mem::transmute::<_, SetMode>(f));
            a.flush = GetProcAddress(lib, 136usize as *const u8).map(|f| std::mem::transmute::<_, Flush>(f));
            a
        })
    }

    /// Process-wide: also makes the tray context menu follow the theme.
    pub fn set_app_mode(theme: Theme) {
        let a = api();
        let Some(set) = a.set_mode else { return };
        // 1 = AllowDark (follow the system), 2 = ForceDark, 3 = ForceLight
        // (ForceLight exists from 1903; before it any non-zero value means "dark").
        let mode = match theme {
            Theme::Auto => 1,
            Theme::Dark => 2,
            Theme::Light => {
                if a.build >= 18362 {
                    3
                } else {
                    0
                }
            }
        };
        unsafe {
            set(mode);
            if let Some(flush) = a.flush {
                flush();
            }
        }
    }

    pub fn allow_window(h: HWND, on: bool) {
        if let Some(f) = api().allow_window {
            unsafe { f(h, on as BOOL) };
        }
    }
}

// ------------------------------------------------------------------ page model

/// What a control cell hosts. Sizes are in DIP.
#[derive(Clone, Copy)]
enum Ctl {
    /// Owner-drawn switch.
    Toggle,
    Combo { w: i32, rows: i32, edit: bool },
    Slider { w: i32, lo: i32, hi: i32, line: u32, page: u32 },
    /// Numeric edit with an up-down control (id + SPIN).
    Number { lo: i32, hi: i32 },
    Edit { w: i32 },
    Button { text: &'static str, w: i32 },
    /// Colour swatch (owner-drawn).
    Swatch,
    /// Text-only "Reset" button (owner-drawn).
    Link,
    /// Small glyph button (owner-drawn).
    Glyph,
}

impl Ctl {
    fn dip(&self) -> (i32, i32) {
        match *self {
            Ctl::Toggle => (88, 32),
            Ctl::Combo { w, .. } => (w, COMBO_H),
            Ctl::Slider { w, .. } => (w, 32),
            Ctl::Number { .. } => (EDIT_W + SPIN_W, COMBO_H),
            Ctl::Edit { w } => (w, COMBO_H),
            Ctl::Button { w, .. } => (w, 32),
            Ctl::Swatch => (44, 32),
            Ctl::Link => (56, 32),
            Ctl::Glyph => (32, 32),
        }
    }
}

/// One slot on the right-hand side of a card, laid out left to right.
enum Cell {
    Ctl(i32, Ctl),
    /// Static text (a unit), painted by the page.
    Text(&'static str, i32),
    /// Live value of the slider with this id ("100 %"), painted by the page.
    Value(i32, i32),
}

enum Title {
    Text(&'static str),
    /// Metric at this position of the (reorderable) list.
    Pos(usize),
    Metric(Metric),
}

struct RowDef {
    title: Title,
    desc: &'static str,
    /// Id of a toggle that must be on for this row to be enabled; 0 = always.
    gate: i32,
    cells: Vec<Cell>,
}

enum Entry {
    Heading(&'static str),
    Note(&'static str),
    Row(RowDef),
}

fn row(title: &'static str, desc: &'static str, cells: Vec<Cell>) -> Entry {
    Entry::Row(RowDef { title: Title::Text(title), desc, gate: 0, cells })
}

fn toggle_row(title: &'static str, desc: &'static str, id: i32) -> Entry {
    row(title, desc, vec![Cell::Ctl(id, Ctl::Toggle)])
}

fn gated(mut e: Entry, gate: i32) -> Entry {
    if let Entry::Row(r) = &mut e {
        r.gate = gate;
    }
    e
}

fn number_row(title: &'static str, desc: &'static str, id: i32, unit: &'static str, lo: i32, hi: i32) -> Entry {
    let mut cells = vec![Cell::Ctl(id, Ctl::Number { lo, hi })];
    if !unit.is_empty() {
        cells.push(Cell::Text(unit, if unit.len() <= 2 { 24 } else { 60 }));
    }
    row(title, desc, cells)
}

fn combo_cell(id: i32, w: i32, rows: i32, edit: bool) -> Cell {
    Cell::Ctl(id, Ctl::Combo { w, rows, edit })
}

fn pages_def() -> Vec<Vec<Entry>> {
    let general = vec![
        row("Update interval", "How often the widget refreshes", vec![combo_cell(ID_INTERVAL, 200, 5, false)]),
        row(
            "Network speed unit",
            "Show speeds in bits or bytes per second",
            vec![combo_cell(ID_UNIT, 200, SpeedUnit::ALL.len() as i32, false)],
        ),
        row(
            "Text size",
            "Scale of the text on the widget",
            vec![Cell::Ctl(ID_SCALE, Ctl::Slider { w: 170, lo: 70, hi: 200, line: 5, page: 10 }), Cell::Value(ID_SCALE, 48)],
        ),
        row("Theme", "Colours of the widget and this window", vec![combo_cell(ID_THEME, 200, 3, false)]),
        row(
            "Background opacity",
            "How solid the widget background is",
            vec![Cell::Ctl(ID_OPACITY, Ctl::Slider { w: 170, lo: 0, hi: 255, line: 5, page: 25 }), Cell::Value(ID_OPACITY, 48)],
        ),
        row("Font", "Typeface used on the widget", vec![combo_cell(ID_FONT, 200, FONTS.len() as i32, true)]),
        Entry::Heading("Behaviour"),
        toggle_row("Sparklines", "Draw a small history graph behind each value", ID_SPARK),
        toggle_row("Metric labels", "Show the name next to each value", ID_LABELS),
        toggle_row("Stacked rows", "Two metrics per column on the taskbar", ID_STACKED),
        toggle_row("Auto-stack when crowded", "Switch to stacked rows when the widget would not fit", ID_AUTOSTACK),
        toggle_row("Hide in full-screen apps", "Keep the widget out of the way of games and videos", ID_FULLSCREEN),
        toggle_row("Click-through", "Mouse clicks pass through the widget to the taskbar", ID_CLICKTHRU),
        toggle_row("Update automatically", "Install new versions when they are released", ID_AUTOUPDATE),
        toggle_row("Start with Windows", "Launch Resource Monitor when you sign in", ID_AUTOSTART),
    ];

    let mut metrics = vec![Entry::Note("Choose which metrics appear on the widget, and move them to change their order.")];
    for pos in 0..Metric::ALL.len() {
        let p = pos as i32;
        metrics.push(Entry::Row(RowDef {
            title: Title::Pos(pos),
            desc: "",
            gate: 0,
            cells: vec![Cell::Ctl(ID_MUP + p, Ctl::Glyph), Cell::Ctl(ID_MDN + p, Ctl::Glyph), Cell::Ctl(ID_MTOG + p, Ctl::Toggle)],
        }));
    }

    let mut colours = vec![
        toggle_row(
            "Use Windows accent colour",
            "Metrics without a custom colour follow your Windows accent colour",
            ID_WINACCENT,
        ),
        row(
            "Reset all colours",
            "Remove every custom colour",
            vec![Cell::Ctl(ID_RESETALL, Ctl::Button { text: "Reset all colours", w: 140 })],
        ),
        Entry::Heading("Metric colours"),
    ];
    for (i, m) in Metric::ALL.iter().enumerate() {
        colours.push(Entry::Row(RowDef {
            title: Title::Metric(*m),
            desc: "",
            gate: 0,
            cells: vec![Cell::Ctl(ID_SWATCH + i as i32, Ctl::Swatch), Cell::Ctl(ID_CRESET + i as i32, Ctl::Link)],
        }));
    }

    let alerts = vec![
        toggle_row("Alert notifications", "Show a tray notification when a limit is crossed", ID_ALERTS),
        Entry::Heading("Limits"),
        gated(number_row("CPU usage above", "Percent of total CPU. 0 turns this alert off", ID_A_CPU, "%", 0, 100), ID_ALERTS),
        gated(number_row("Memory usage above", "Percent of installed memory. 0 turns this alert off", ID_A_RAM, "%", 0, 100), ID_ALERTS),
        gated(number_row("For at least", "How long CPU or memory must stay above its limit", ID_A_SECS, "seconds", 5, 3600), ID_ALERTS),
        gated(number_row("System drive free space below", "0 turns this alert off", ID_A_DISK, "%", 0, 100), ID_ALERTS),
        gated(number_row("Battery at or below", "Only while not charging. 0 turns this alert off", ID_A_BAT, "%", 0, 100), ID_ALERTS),
        gated(toggle_row("Data cap alerts", "Alert at 90% and 100% of the monthly data cap", ID_A_CAP), ID_ALERTS),
        Entry::Heading("Colour thresholds"),
        number_row("Amber from", "Values turn amber from this percentage", ID_WARN, "%", 1, 100),
        number_row("Red from", "Values turn red from this percentage", ID_CRIT, "%", 1, 100),
    ];

    let data = vec![
        row(
            "Monthly data cap",
            "In gigabytes. 0 turns the cap off",
            vec![Cell::Ctl(ID_CAP, Ctl::Edit { w: 88 }), Cell::Text("GB", 24)],
        ),
        number_row("Billing cycle resets on day", "Data is counted since the last reset day", ID_DAY, "", 1, 28),
        row("Network adapter", "Which adapter the data counters follow", vec![combo_cell(ID_ADAPTER, 280, 8, false)]),
        row(
            "Data usage report",
            "Daily and monthly totals",
            vec![Cell::Ctl(ID_REPORT, Ctl::Button { text: "Open report\u{2026}", w: 140 })],
        ),
    ];

    vec![general, metrics, colours, alerts, data]
}

// ------------------------------------------------------------------ fonts

struct Fonts {
    /// ClearType, for the native controls.
    ctl: HFONT,
    // Grayscale AA, for the canvas.
    body: HFONT,
    small: HFONT,
    semi: HFONT,
    title: HFONT,
    icon: HFONT,
    icon_sm: HFONT,
}

/// `want` if the system has it, else `fallback`.
fn pick_face(want: &'static str, fallback: &'static str) -> &'static str {
    unsafe {
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let f = create_font(12, 400, want, true);
        let old = SelectObject(dc, f);
        let mut buf = [0u16; 64];
        let n = GetTextFaceW(dc, buf.len() as i32, buf.as_mut_ptr());
        SelectObject(dc, old);
        DeleteObject(f);
        DeleteDC(dc);
        let got = String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize]);
        if got.eq_ignore_ascii_case(want) {
            want
        } else {
            fallback
        }
    }
}

impl Fonts {
    fn new(dpi: u32) -> Fonts {
        let px = |pt: i32| ((pt * dpi as i32 + 36) / 72).max(1);
        let text = ui_face();
        let display = pick_face("Segoe UI Variable Display", text);
        let icons = pick_face("Segoe Fluent Icons", "Segoe MDL2 Assets");
        let sc = |v: i32| (v * dpi as i32 + 48) / 96;
        Fonts {
            ctl: create_font(px(10), 400, text, false),
            body: create_font(px(10), 400, text, true),
            small: create_font(px(9), 400, text, true),
            semi: create_font(px(10), 600, text, true),
            title: create_font(px(20), 600, display, true),
            icon: create_font(sc(16), 400, icons, true),
            icon_sm: create_font(sc(14), 400, icons, true),
        }
    }

    fn free(&self) {
        for f in [self.ctl, self.body, self.small, self.semi, self.title, self.icon, self.icon_sm] {
            unsafe { DeleteObject(f) };
        }
    }
}

// ------------------------------------------------------------------ items

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Owner-drawn button.
    Owner,
    /// Standard push button.
    Button,
    Combo,
    Edit,
    Spin,
    Slider,
}

struct Item {
    hwnd: HWND,
    id: i32,
    kind: Kind,
    /// Page this control lives on; -1 = always visible (footer).
    page: i32,
}

struct Ui {
    hinst: HINSTANCE,
    main: HWND,
    nav: HWND,
    page_wnd: HWND,
    items: Vec<Item>,
    pages: Vec<Vec<Entry>>,
    /// Per page and entry: top and height in px, content coordinates.
    geo: Vec<Vec<(i32, i32)>>,
    content_h: Vec<i32>,
    fonts: Fonts,
    dpi: u32,
    dark: bool,
    /// The live theme setting (drives the process-wide app mode).
    theme: Theme,
    pal: Pal,
    br_bg: HBRUSH,
    br_card: HBRUSH,
    br_input: HBRUSH,
    page: usize,
    scroll: i32,
    page_w: i32,
    page_h: i32,
    nav_hover: Option<usize>,
    nav_tracking: bool,
    cv_main: Canvas,
    cv_nav: Canvas,
    cv_page: Canvas,
    /// The copy being edited.
    draft: Settings,
    /// Adapter aliases offered in the combo (index + 1; 0 = automatic).
    adapters: Vec<String>,
    adapter_labels: Vec<String>,
    autostart: bool,
    /// Custom colours of the colour picker, kept for the session.
    cust: [COLORREF; 16],
}

// ------------------------------------------------------------------ helpers

fn send(h: HWND, msg: u32, w: usize, l: isize) -> isize {
    unsafe { SendMessageW(h, msg, w, l) }
}

/// A control of any page, or of the footer, by id.
fn ctl(main: HWND, id: i32) -> HWND {
    unsafe {
        let page = PAGE_HWND.load(Relaxed);
        if !page.is_null() {
            let h = GetDlgItem(page, id);
            if !h.is_null() {
                return h;
            }
        }
        GetDlgItem(main, id)
    }
}

fn page_hwnd() -> HWND {
    PAGE_HWND.load(Relaxed)
}

fn set_text(h: HWND, s: &str) {
    let w = wide(s);
    unsafe { SetWindowTextW(h, w.as_ptr()) };
}

fn get_text(h: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Toggle switches keep their state in the window's user data.
fn toggle_get(h: HWND) -> bool {
    !h.is_null() && unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) } != 0
}

fn toggle_set(h: HWND, on: bool) {
    if !h.is_null() {
        unsafe {
            SetWindowLongPtrW(h, GWLP_USERDATA, on as isize);
            InvalidateRect(h, std::ptr::null(), 0);
        }
    }
}

fn is_toggle(id: i32) -> bool {
    TOGGLE_IDS.contains(&id) || (ID_MTOG..ID_MTOG + Metric::ALL.len() as i32).contains(&id)
}

fn combo_add(h: HWND, text: &str, data: isize) {
    let w = wide(text);
    let i = send(h, CB_ADDSTRING, 0, w.as_ptr() as isize);
    if i >= 0 {
        send(h, CB_SETITEMDATA, i as usize, data);
    }
}

fn parse_f(h: HWND) -> Option<f64> {
    get_text(h).trim().replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Percent edits show whole numbers; keep a hand-edited fraction (87.5) unless
/// the user actually changed the field.
fn pct_field(h: HWND, old: f32, lo: f32, hi: f32) -> f32 {
    match parse_f(h) {
        Some(v) if (v - old as f64).abs() < 0.5 => old.clamp(lo, hi),
        Some(v) => (v as f32).clamp(lo, hi),
        None => old.clamp(lo, hi),
    }
}

fn int_field(h: HWND, old: u32, lo: u32, hi: u32) -> u32 {
    match parse_f(h) {
        Some(v) => (v.round().max(0.0) as u32).clamp(lo, hi),
        None => old.clamp(lo, hi),
    }
}

fn rgb_to_cr(rgb: u32) -> COLORREF {
    ((rgb >> 16) & 0xFF) | (rgb & 0xFF00) | ((rgb & 0xFF) << 16)
}

fn cr_to_rgb(cr: COLORREF) -> u32 {
    rgb_to_cr(cr) // the swap is its own inverse
}

fn theme_name(h: HWND, name: Option<&str>, sub: Option<&str>) {
    let n = name.map(wide);
    let s = sub.map(wide);
    unsafe {
        SetWindowTheme(
            h,
            n.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
            s.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
        )
    };
}

fn brush(c: Rgba) -> HBRUSH {
    unsafe { CreateSolidBrush(c.colorref()) }
}

fn inflate(r: Rect, d: i32) -> Rect {
    Rect { l: r.l - d, t: r.t - d, r: r.r + d, b: r.b + d }
}

/// Filled circle of diameter `d`.
fn disc(cv: &mut Canvas, cx: i32, cy: i32, d: i32, c: Rgba) {
    let r = d / 2;
    cv.fill_round(Rect { l: cx - r, t: cy - r, r: cx - r + d, b: cy - r + d }, d as f32 / 2.0, c);
}

/// Two-pixel focus ring just inside `r`.
fn ring(cv: &mut Canvas, r: Rect, rad: f32, c: Rgba) {
    cv.stroke_round(r, rad, c);
    cv.stroke_round(inflate(r, -1), (rad - 1.0).max(0.0), c);
}

/// Text with its vertical centre at `cy`.
fn text_mid(cv: &mut Canvas, font: HFONT, s: &str, x: i32, cy: i32, c: Rgba) -> i32 {
    let (_, th) = cv.measure(font, s);
    cv.text(font, s, x, cy - th / 2, c)
}

fn fits(cv: &Canvas, font: HFONT, s: &str, max_w: i32) -> String {
    cv.ellipsize(font, s, max_w.max(8))
}

impl Cell {
    fn dip_w(&self) -> i32 {
        match self {
            Cell::Ctl(_, c) => c.dip().0,
            Cell::Text(_, w) | Cell::Value(_, w) => *w,
        }
    }
}

impl Ui {
    fn new(hinst: HINSTANCE, dpi: u32, dark: bool, theme: Theme, draft: Settings, adapters: Vec<String>, labels: Vec<String>) -> Ui {
        let pal = Pal::new(dark, accent_color(dark));
        Ui {
            hinst,
            main: std::ptr::null_mut(),
            nav: std::ptr::null_mut(),
            page_wnd: std::ptr::null_mut(),
            items: Vec::new(),
            pages: Vec::new(),
            geo: Vec::new(),
            content_h: Vec::new(),
            fonts: Fonts::new(dpi),
            dpi,
            dark,
            theme,
            pal,
            br_bg: brush(pal.bg),
            br_card: brush(pal.card),
            br_input: brush(pal.input),
            page: 0,
            scroll: 0,
            page_w: 0,
            page_h: 0,
            nav_hover: None,
            nav_tracking: false,
            cv_main: Canvas::new(),
            cv_nav: Canvas::new(),
            cv_page: Canvas::new(),
            draft,
            adapters,
            adapter_labels: labels,
            autostart: crate::autostart::enabled(),
            cust: [0x00FF_FFFF; 16],
        }
    }

    fn sc(&self, v: i32) -> i32 {
        (v * self.dpi as i32 + 48) / 96
    }

    /// Effective colour of a metric as the widget would show it.
    fn swatch_rgb(&self, m: Metric) -> u32 {
        if let Some((_, c)) = self.draft.colors.iter().find(|(x, _)| *x == m) {
            return *c & 0xFF_FFFF;
        }
        if self.draft.windows_accent {
            let (mut c, mut opaque) = (0u32, 0i32);
            if unsafe { DwmGetColorizationColor(&mut c, &mut opaque) } == 0 {
                return c & 0xFF_FFFF;
            }
        }
        let c = default_accent(m);
        (c.0 as u32) << 16 | (c.1 as u32) << 8 | c.2 as u32
    }

    fn title_text(&self, t: &Title) -> &'static str {
        match t {
            Title::Text(s) => s,
            Title::Pos(i) => self.draft.order.get(*i).map_or("", |m| m.menu_label()),
            Title::Metric(m) => m.menu_label(),
        }
    }

    // ---------------------------------------------------------------- build

    fn make(&mut self, parent: HWND, class: &str, text: &str, style: u32, ex: u32, id: i32, page: i32, kind: Kind) -> HWND {
        let vis = page < 0 || page as usize == self.page;
        let cls = wide(class);
        let txt = wide(text);
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                cls.as_ptr(),
                txt.as_ptr(),
                WS_CHILD | style | if vis { WS_VISIBLE } else { 0 },
                0,
                0,
                self.sc(40),
                self.sc(24),
                parent,
                id as usize as HMENU,
                self.hinst,
                std::ptr::null(),
            )
        };
        if !hwnd.is_null() {
            send(hwnd, WM_SETFONT, self.fonts.ctl as usize, 1);
            self.items.push(Item { hwnd, id, kind, page });
        }
        hwnd
    }

    fn make_ctl(&mut self, page: usize, id: i32, c: Ctl, name: &str) {
        let pw = self.page_wnd;
        let p = page as i32;
        let tab = WS_TABSTOP;
        match c {
            Ctl::Toggle | Ctl::Swatch | Ctl::Link | Ctl::Glyph => {
                self.make(pw, "BUTTON", name, BS_OWNERDRAW as u32 | tab, 0, id, p, Kind::Owner);
            }
            Ctl::Button { text, .. } => {
                self.make(pw, "BUTTON", text, BS_PUSHBUTTON as u32 | tab, 0, id, p, Kind::Button);
            }
            Ctl::Combo { edit, .. } => {
                let style = if edit { CBS_DROPDOWN as u32 | CBS_AUTOHSCROLL } else { CBS_DROPDOWNLIST as u32 };
                self.make(pw, "COMBOBOX", "", style | tab | WS_VSCROLL, 0, id, p, Kind::Combo);
            }
            Ctl::Slider { lo, hi, line, page: pg, .. } => {
                let h = self.make(pw, "msctls_trackbar32", "", TBS_HORZ | TBS_NOTICKS | tab, 0, id, p, Kind::Slider);
                send(h, TBM_SETRANGE, 1, ((hi << 16) | lo) as isize);
                send(h, TBM_SETLINESIZE, 0, line as isize);
                send(h, TBM_SETPAGESIZE, 0, pg as isize);
                unsafe { SetWindowSubclass(h, Some(slider_sub), 1, 0) };
            }
            Ctl::Edit { .. } => {
                self.make(pw, "EDIT", "", ES_AUTOHSCROLL as u32 | tab, WS_EX_CLIENTEDGE, id, p, Kind::Edit);
            }
            Ctl::Number { lo, hi } => {
                let e = self.make(pw, "EDIT", "", ES_AUTOHSCROLL as u32 | ES_NUMBER as u32 | tab, WS_EX_CLIENTEDGE, id, p, Kind::Edit);
                let u = self.make(
                    pw,
                    "msctls_updown32",
                    "",
                    UDS_SETBUDDYINT | UDS_ARROWKEYS | UDS_NOTHOUSANDS,
                    0,
                    id + SPIN,
                    p,
                    Kind::Spin,
                );
                // No UDS_ALIGN*: the up-down is positioned by `place_page`.
                send(u, UDM_SETBUDDY, e as usize, 0);
                send(u, UDM_SETRANGE32, lo as usize, hi as isize);
            }
        }
    }

    fn build(&mut self) {
        let main = self.main;
        unsafe {
            let nav_cls = wide(NAV_CLASS);
            self.nav = CreateWindowExW(
                0,
                nav_cls.as_ptr(),
                wide("Settings pages").as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                0,
                0,
                self.sc(NAV_W),
                self.sc(CLIENT_H),
                main,
                ID_NAV as usize as HMENU,
                self.hinst,
                std::ptr::null(),
            );
            let page_cls = wide(PAGE_CLASS);
            self.page_wnd = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                page_cls.as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_VSCROLL,
                self.sc(NAV_W),
                self.sc(TITLE_H),
                self.sc(CLIENT_W - NAV_W),
                self.sc(CLIENT_H - TITLE_H - FOOTER_H),
                main,
                ID_PAGE as usize as HMENU,
                self.hinst,
                std::ptr::null(),
            );
        }
        PAGE_HWND.store(self.page_wnd, Relaxed);

        let defs = pages_def();
        for (p, entries) in defs.iter().enumerate() {
            for e in entries {
                let Entry::Row(r) = e else { continue };
                let name = self.title_text(&r.title);
                for c in &r.cells {
                    if let Cell::Ctl(id, k) = c {
                        // Accessible name of owner-drawn controls.
                        let label = match k {
                            Ctl::Swatch => format!("{} colour", name),
                            Ctl::Link => format!("Reset {} colour", name),
                            Ctl::Glyph if *id < ID_MDN => "Move up".to_string(),
                            Ctl::Glyph => "Move down".to_string(),
                            _ => name.to_string(),
                        };
                        self.make_ctl(p, *id, *k, &label);
                    }
                }
            }
        }
        self.pages = defs;

        self.make(main, "BUTTON", "OK", BS_OWNERDRAW as u32 | WS_TABSTOP, 0, IDOK, -1, Kind::Owner);
        self.make(main, "BUTTON", "Cancel", BS_PUSHBUTTON as u32 | WS_TABSTOP, 0, IDCANCEL, -1, Kind::Button);
        self.make(main, "BUTTON", "Apply", BS_PUSHBUTTON as u32 | WS_TABSTOP, 0, ID_APPLY, -1, Kind::Button);
        self.fill_combos();
    }

    fn fill_combos(&mut self) {
        let h = |id| ctl(self.main, id);
        let (iv, un, th, ft, ad) = (h(ID_INTERVAL), h(ID_UNIT), h(ID_THEME), h(ID_FONT), h(ID_ADAPTER));
        for (t, ms) in INTERVALS {
            combo_add(iv, t, ms as isize);
        }
        let cur = self.draft.interval_ms;
        if !INTERVALS.iter().any(|(_, ms)| *ms == cur) {
            combo_add(iv, &format!("{:.2} seconds", cur as f32 / 1000.0), cur as isize);
        }
        for u in SpeedUnit::ALL {
            combo_add(un, u.label(), 0);
        }
        for (t, _) in THEMES {
            combo_add(th, t, 0);
        }
        for f in FONTS {
            combo_add(ft, f, 0);
        }
        combo_add(ad, "Automatic (all connected adapters)", 0);
        for l in &self.adapter_labels {
            combo_add(ad, l, 0);
        }
    }

    // --------------------------------------------------------------- layout

    /// Top and height of every entry of every page (px, content coordinates).
    fn compute_geo(&mut self) {
        let mut geo = Vec::new();
        let mut totals = Vec::new();
        for entries in &self.pages {
            let mut y = self.sc(4);
            let mut v = Vec::with_capacity(entries.len());
            for e in entries {
                let (h, gap) = match e {
                    Entry::Heading(_) => (self.sc(HEAD_H), 0),
                    Entry::Note(_) => (self.sc(NOTE_H), 0),
                    Entry::Row(r) => (self.sc(if r.desc.is_empty() { ROW_H_SLIM } else { ROW_H }), self.sc(ROW_GAP)),
                };
                v.push((y, h));
                y += h + gap;
            }
            geo.push(v);
            totals.push(y + self.sc(16));
        }
        self.geo = geo;
        self.content_h = totals;
    }

    /// Rectangles of the cells of a card, right-aligned inside `card`.
    fn cell_rects(&self, cells: &[Cell], card: Rect) -> Vec<Rect> {
        let gap = self.sc(CELL_GAP);
        let sizes: Vec<(i32, i32)> = cells
            .iter()
            .map(|c| match c {
                Cell::Ctl(_, k) => {
                    let (w, h) = k.dip();
                    (self.sc(w), self.sc(h))
                }
                Cell::Text(..) | Cell::Value(..) => (self.sc(c.dip_w()), self.sc(24)),
            })
            .collect();
        // Units and values hug the control before them.
        let gap_before = |i: usize| -> i32 {
            if i == 0 {
                0
            } else if matches!(cells[i], Cell::Text(..) | Cell::Value(..)) {
                self.sc(4)
            } else {
                gap
            }
        };
        let total: i32 = sizes.iter().enumerate().map(|(i, s)| s.0 + gap_before(i)).sum();
        let mut x = card.r - self.sc(CARD_PAD) - total;
        let cy = (card.t + card.b) / 2;
        let mut out = Vec::with_capacity(cells.len());
        for (i, &(w, h)) in sizes.iter().enumerate() {
            x += gap_before(i);
            let t = cy - h / 2;
            out.push(Rect { l: x, t, r: x + w, b: t + h });
            x += w;
        }
        out
    }

    fn card_rect(&self, y: i32, h: i32, scroll: i32) -> Rect {
        Rect { l: self.sc(PAD), t: y - scroll, r: self.page_w - self.sc(PAD), b: y - scroll + h }
    }

    fn max_scroll(&self) -> i32 {
        (self.content_h.get(self.page).copied().unwrap_or(0) - self.page_h).max(0)
    }

    /// Move every control of page `p` to where its card is.
    fn place_page(&self, p: usize) {
        let scroll = if p == self.page { self.scroll } else { 0 };
        let flags = SWP_NOZORDER | SWP_NOACTIVATE;
        let (Some(entries), Some(geo)) = (self.pages.get(p), self.geo.get(p)) else { return };
        let count = self.items.iter().filter(|i| i.page == p as i32).count() as i32;
        unsafe {
            let mut dwp = BeginDeferWindowPos(count.max(1));
            for (i, e) in entries.iter().enumerate() {
                let Entry::Row(r) = e else { continue };
                let (y, h) = geo[i];
                let rects = self.cell_rects(&r.cells, self.card_rect(y, h, scroll));
                for (cell, rc) in r.cells.iter().zip(rects) {
                    let Cell::Ctl(id, k) = cell else { continue };
                    let hw = GetDlgItem(self.page_wnd, *id);
                    if hw.is_null() || dwp.is_null() {
                        continue;
                    }
                    match k {
                        Ctl::Number { .. } => {
                            let ew = self.sc(EDIT_W);
                            dwp = DeferWindowPos(dwp, hw, std::ptr::null_mut(), rc.l, rc.t, ew, rc.h(), flags);
                            let sp = GetDlgItem(self.page_wnd, *id + SPIN);
                            if !sp.is_null() && !dwp.is_null() {
                                dwp = DeferWindowPos(dwp, sp, std::ptr::null_mut(), rc.l + ew, rc.t, rc.w() - ew, rc.h(), flags);
                            }
                        }
                        Ctl::Combo { rows, .. } => {
                            // A combo's height includes its drop-down list.
                            let drop = (*rows).min(10) * self.sc(COMBO_H) + self.sc(8);
                            dwp = DeferWindowPos(dwp, hw, std::ptr::null_mut(), rc.l, rc.t, rc.w(), rc.h() + drop, flags);
                        }
                        _ => {
                            dwp = DeferWindowPos(dwp, hw, std::ptr::null_mut(), rc.l, rc.t, rc.w(), rc.h(), flags);
                        }
                    }
                }
            }
            if !dwp.is_null() {
                EndDeferWindowPos(dwp);
            }
        }
    }

    fn sync_scrollbar(&self) {
        let si = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: (self.content_h.get(self.page).copied().unwrap_or(0) - 1).max(0),
            nPage: self.page_h.max(1) as u32,
            nPos: self.scroll,
            nTrackPos: 0,
        };
        unsafe { SetScrollInfo(self.page_wnd, SB_VERT, &si, 1) };
    }

    /// Re-measure the page, fix the scroll bar and re-place all controls.
    fn refresh_page(&mut self) {
        unsafe {
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(self.page_wnd, &mut rc);
            self.page_h = rc.bottom;
            self.scroll = self.scroll.clamp(0, self.max_scroll());
            self.sync_scrollbar();
            // The scroll bar may have appeared or gone: take the width after.
            GetClientRect(self.page_wnd, &mut rc);
            self.page_w = rc.right;
            InvalidateRect(self.page_wnd, std::ptr::null(), 0);
        }
        for p in 0..self.pages.len() {
            self.place_page(p);
        }
    }

    /// Position everything for the current client size and dpi.
    fn relayout(&mut self) {
        let was = LOADING.swap(true, Relaxed);
        unsafe {
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(self.main, &mut rc);
            let (w, h) = (rc.right, rc.bottom);
            let (nav_w, title, footer) = (self.sc(NAV_W), self.sc(TITLE_H), self.sc(FOOTER_H));
            let flags = SWP_NOZORDER | SWP_NOACTIVATE;
            SetWindowPos(self.nav, std::ptr::null_mut(), 0, 0, nav_w, (h - footer).max(1), flags);
            SetWindowPos(self.page_wnd, std::ptr::null_mut(), nav_w, title, (w - nav_w).max(1), (h - footer - title).max(1), flags);

            // Footer buttons, right-aligned.
            let (bw, bh, gap) = (self.sc(96), self.sc(32), self.sc(8));
            let by = h - footer + (footer - bh) / 2;
            let right = w - self.sc(PAD);
            for (id, i) in [(ID_APPLY, 0), (IDCANCEL, 1), (IDOK, 2)] {
                let hw = ctl(self.main, id);
                if !hw.is_null() {
                    SetWindowPos(hw, std::ptr::null_mut(), right - bw - i * (bw + gap), by, bw, bh, flags);
                }
            }
        }
        for it in &self.items {
            send(it.hwnd, WM_SETFONT, self.fonts.ctl as usize, 1);
            match it.kind {
                Kind::Combo => {
                    // Selection field and list item heights follow the dpi.
                    send(it.hwnd, CB_SETITEMHEIGHT, usize::MAX, self.sc(COMBO_H - 4) as isize);
                    send(it.hwnd, CB_SETITEMHEIGHT, 0, self.sc(COMBO_H) as isize);
                    if it.id == ID_ADAPTER {
                        send(it.hwnd, CB_SETDROPPEDWIDTH, self.sc(440) as usize, 0);
                    }
                }
                _ => {}
            }
        }
        self.compute_geo();
        self.refresh_page();
        LOADING.store(was, Relaxed);
        unsafe { InvalidateRect(self.main, std::ptr::null(), 0) };
    }

    fn set_dpi(&mut self, dpi: u32) -> Fonts {
        self.dpi = dpi;
        std::mem::replace(&mut self.fonts, Fonts::new(dpi))
    }

    fn show_page(&mut self, p: usize) {
        if p == self.page || p >= self.pages.len() {
            return;
        }
        let old = self.page as i32;
        for it in &self.items {
            if it.page == old {
                unsafe { ShowWindow(it.hwnd, SW_HIDE) };
            }
        }
        self.page = p;
        self.scroll = 0;
        for it in &self.items {
            if it.page == p as i32 {
                unsafe { ShowWindow(it.hwnd, SW_SHOWNA) };
            }
        }
        self.refresh_page();
        unsafe {
            InvalidateRect(self.nav, std::ptr::null(), 0);
            InvalidateRect(self.main, std::ptr::null(), 0);
        }
    }

    fn set_scroll(&mut self, pos: i32) {
        let pos = pos.clamp(0, self.max_scroll());
        if pos == self.scroll {
            return;
        }
        self.scroll = pos;
        self.sync_scrollbar();
        self.place_page(self.page);
        unsafe { InvalidateRect(self.page_wnd, std::ptr::null(), 0) };
    }

    fn scroll_cmd(&mut self, code: i32) {
        let line = self.sc(40);
        let max = self.max_scroll();
        let pos = match code {
            SB_LINEUP => self.scroll - line,
            SB_LINEDOWN => self.scroll + line,
            SB_PAGEUP => self.scroll - (self.page_h - line).max(line),
            SB_PAGEDOWN => self.scroll + (self.page_h - line).max(line),
            SB_TOP => 0,
            SB_BOTTOM => max,
            SB_THUMBTRACK | SB_THUMBPOSITION => {
                let mut si = SCROLLINFO {
                    cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_TRACKPOS,
                    nMin: 0,
                    nMax: 0,
                    nPage: 0,
                    nPos: 0,
                    nTrackPos: 0,
                };
                unsafe { GetScrollInfo(self.page_wnd, SB_VERT, &mut si) };
                si.nTrackPos
            }
            _ => return,
        };
        self.set_scroll(pos);
    }

    /// Scroll so the focused control (if on this page) is fully visible.
    fn ensure_focus_visible(&mut self) {
        unsafe {
            let f = GetFocus();
            if f.is_null() || f == self.page_wnd || IsChild(self.page_wnd, f) == 0 {
                return;
            }
            let mut rc: RECT = std::mem::zeroed();
            GetWindowRect(f, &mut rc);
            let mut pts = [POINT { x: rc.left, y: rc.top }, POINT { x: rc.right, y: rc.bottom }];
            MapWindowPoints(std::ptr::null_mut(), self.page_wnd, pts.as_mut_ptr(), 2);
            // A combo's window includes its drop-down list: only the field counts.
            let bottom = pts[1].y.min(pts[0].y + self.sc(40));
            let m = self.sc(10);
            if pts[0].y - m < 0 {
                self.set_scroll(self.scroll + pts[0].y - m);
            } else if bottom + m > self.page_h {
                self.set_scroll(self.scroll + bottom + m - self.page_h);
            }
        }
    }

    /// Rectangle (page client coordinates) of the value label of slider `id`.
    fn value_rect(&self, id: i32) -> Option<RECT> {
        let (entries, geo) = (self.pages.get(self.page)?, self.geo.get(self.page)?);
        for (i, e) in entries.iter().enumerate() {
            let Entry::Row(r) = e else { continue };
            if r.cells.iter().any(|c| matches!(c, Cell::Value(v, _) if *v == id)) {
                let (y, h) = geo[i];
                let rects = self.cell_rects(&r.cells, self.card_rect(y, h, self.scroll));
                let rc = r.cells.iter().zip(rects).find(|(c, _)| matches!(c, Cell::Value(v, _) if *v == id))?.1;
                return Some(RECT { left: rc.l - 2, top: rc.t - 2, right: rc.r + 2, bottom: rc.b + 2 });
            }
        }
        None
    }

    // ------------------------------------------------------------- painting

    fn draw_main(&self, cv: &mut Canvas) {
        let p = &self.pal;
        cv.clear(p.bg);
        let name = PAGES[self.page];
        let th = cv.measure(self.fonts.title, name).1;
        cv.text(self.fonts.title, name, self.sc(NAV_W + PAD), (self.sc(TITLE_H) - th) / 2 + self.sc(4), p.fg);
        let fy = cv.h - self.sc(FOOTER_H);
        cv.fill_rect(Rect { l: 0, t: fy, r: cv.w, b: cv.h }, p.footer);
        cv.fill_rect(Rect { l: 0, t: fy, r: cv.w, b: fy + 1 }, p.line);
    }

    fn nav_rect(&self, w: i32, i: usize) -> Rect {
        let t = self.sc(12) + i as i32 * (self.sc(NAV_ITEM_H) + self.sc(4));
        Rect { l: self.sc(8), t, r: w - self.sc(8), b: t + self.sc(NAV_ITEM_H) }
    }

    fn draw_nav(&self, cv: &mut Canvas, focused: bool) {
        let p = &self.pal;
        cv.clear(p.bg);
        let rad = self.sc(4) as f32;
        for i in 0..PAGES.len() {
            let r = self.nav_rect(cv.w, i);
            let selected = i == self.page;
            if selected {
                cv.fill_round(r, rad, p.nav_sel);
                let ph = self.sc(16);
                let (pl, pt) = (r.l + self.sc(2), (r.t + r.b) / 2 - ph / 2);
                cv.fill_round(Rect { l: pl, t: pt, r: pl + self.sc(3), b: pt + ph }, self.sc(3) as f32 / 2.0, p.accent);
            } else if self.nav_hover == Some(i) {
                cv.fill_round(r, rad, p.nav_hover);
            }
            let cy = (r.t + r.b) / 2;
            let mut glyph = [0u8; 4];
            let g = NAV_GLYPHS[i].encode_utf8(&mut glyph);
            let (gw, _) = cv.measure(self.fonts.icon, g);
            text_mid(cv, self.fonts.icon, g, r.l + self.sc(16) + (self.sc(16) - gw) / 2, cy, p.fg);
            text_mid(cv, self.fonts.body, PAGES[i], r.l + self.sc(44), cy, p.fg);
            if focused && selected {
                ring(cv, r, rad, p.fg);
            }
        }
    }

    fn value_text(&self, id: i32) -> String {
        let pos = send(ctl(self.main, id), TBM_GETPOS, 0, 0);
        match id {
            ID_SCALE => format!("{} %", pos),
            _ => format!("{} %", (pos * 100 + 127) / 255),
        }
    }

    fn draw_page(&self, cv: &mut Canvas, clip: Rect) {
        let p = &self.pal;
        let f = &self.fonts;
        cv.clear(p.bg);
        let (Some(entries), Some(geo)) = (self.pages.get(self.page), self.geo.get(self.page)) else { return };
        let rad = self.sc(6) as f32;
        for (i, e) in entries.iter().enumerate() {
            let (y, h) = geo[i];
            let top = y - self.scroll;
            if top + h < clip.t || top > clip.b {
                continue;
            }
            let x0 = self.sc(PAD);
            match e {
                Entry::Heading(t) => {
                    let th = cv.measure(f.semi, t).1;
                    cv.text(f.semi, t, x0 + self.sc(2), top + h - th - self.sc(8), p.fg);
                }
                Entry::Note(t) => {
                    let s = fits(cv, f.body, t, self.page_w - 2 * x0);
                    cv.text(f.body, &s, x0 + self.sc(2), top + self.sc(2), p.dim);
                }
                Entry::Row(r) => {
                    let card = self.card_rect(y, h, self.scroll);
                    cv.fill_round(card, rad, p.card);
                    cv.stroke_round(card, rad, p.border);
                    let on = r.gate == 0 || toggle_get(unsafe { GetDlgItem(self.page_wnd, r.gate) });
                    let (c_main, c_dim) = if on { (p.fg, p.dim) } else { (p.off, p.off) };
                    let rects = self.cell_rects(&r.cells, card);
                    let tx = card.l + self.sc(CARD_PAD);
                    let max_w = rects.first().map_or(card.r, |c| c.l) - self.sc(CARD_PAD) - tx;
                    let title = fits(cv, f.body, self.title_text(&r.title), max_w);
                    let cy = (card.t + card.b) / 2;
                    if r.desc.is_empty() {
                        text_mid(cv, f.body, &title, tx, cy, c_main);
                    } else {
                        let (_, th) = cv.measure(f.body, &title);
                        let (_, dh) = cv.measure(f.small, r.desc);
                        let block = th + self.sc(2) + dh;
                        let ty = cy - block / 2;
                        cv.text(f.body, &title, tx, ty, c_main);
                        let desc = fits(cv, f.small, r.desc, max_w);
                        cv.text(f.small, &desc, tx, ty + th + self.sc(2), c_dim);
                    }
                    for (cell, rc) in r.cells.iter().zip(&rects) {
                        match cell {
                            Cell::Text(s, _) => {
                                text_mid(cv, f.body, s, rc.l, cy, c_main);
                            }
                            Cell::Value(id, _) => {
                                let s = self.value_text(*id);
                                text_mid(cv, f.body, &s, rc.l, cy, c_dim);
                            }
                            Cell::Ctl(..) => {}
                        }
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ theme

fn apply_theme(hwnd: HWND) {
    let Some(u) = ui() else { return };
    let dark = u.dark;
    u.pal = Pal::new(dark, accent_color(dark));
    unsafe {
        DeleteObject(u.br_bg);
        DeleteObject(u.br_card);
        DeleteObject(u.br_input);
    }
    u.br_bg = brush(u.pal.bg);
    u.br_card = brush(u.pal.card);
    u.br_input = brush(u.pal.input);

    // Process-wide (tray menu too), then per window.
    darkmode::set_app_mode(u.theme);
    for h in [hwnd, u.nav, u.page_wnd] {
        darkmode::allow_window(h, dark);
    }
    let dark_explorer = if dark { Some("DarkMode_Explorer") } else { None };
    theme_name(u.page_wnd, dark_explorer, None); // dark scroll bar
    for it in &u.items {
        darkmode::allow_window(it.hwnd, dark);
        match it.kind {
            Kind::Combo => {
                theme_name(it.hwnd, if dark { Some("DarkMode_CFD") } else { None }, None);
                let mut cbi: COMBOBOXINFO = unsafe { std::mem::zeroed() };
                cbi.cbSize = std::mem::size_of::<COMBOBOXINFO>() as u32;
                if unsafe { GetComboBoxInfo(it.hwnd, &mut cbi) } != 0 && !cbi.hwndList.is_null() {
                    darkmode::allow_window(cbi.hwndList, dark);
                    theme_name(cbi.hwndList, dark_explorer, None);
                }
            }
            Kind::Edit => theme_name(it.hwnd, if dark { Some("DarkMode_CFD") } else { None }, None),
            Kind::Spin | Kind::Button => theme_name(it.hwnd, dark_explorer, None),
            Kind::Owner | Kind::Slider => {}
        }
    }
    style_window(hwnd, dark);
    // Windows 11: Mica backdrop (DWMWA_SYSTEMBACKDROP_TYPE = 38, DWMSBT_MAINWINDOW = 2).
    let mica: i32 = 2;
    unsafe {
        DwmSetWindowAttribute(hwnd, 38, &mica as *const i32 as _, 4);
        RedrawWindow(hwnd, std::ptr::null(), std::ptr::null_mut(), RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME);
    }
}

// ------------------------------------------------------------------ values

fn sync_metric_rows(hwnd: HWND) {
    let Some(u) = ui() else { return };
    let n = u.draft.order.len();
    for (pos, m) in u.draft.order.iter().enumerate() {
        let p = pos as i32;
        let tog = ctl(hwnd, ID_MTOG + p);
        toggle_set(tog, u.draft.show[m.idx()]);
        set_text(tog, m.menu_label());
        unsafe {
            EnableWindow(ctl(hwnd, ID_MUP + p), (pos > 0) as i32);
            EnableWindow(ctl(hwnd, ID_MDN + p), (pos + 1 < n) as i32);
            InvalidateRect(ctl(hwnd, ID_MUP + p), std::ptr::null(), 0);
            InvalidateRect(ctl(hwnd, ID_MDN + p), std::ptr::null(), 0);
        }
    }
    unsafe { InvalidateRect(u.page_wnd, std::ptr::null(), 0) };
}

fn move_metric(hwnd: HWND, pos: usize, dir: isize) {
    let to = pos as isize + dir;
    let moved = match ui() {
        Some(u) if to >= 0 && (to as usize) < u.draft.order.len() => {
            u.draft.order.swap(pos, to as usize);
            true
        }
        _ => false,
    };
    if !moved {
        return;
    }
    sync_metric_rows(hwnd);
    mark_dirty(hwnd);
    // Keep the focus on the row that moved.
    let (up, down) = (ctl(hwnd, ID_MUP + to as i32), ctl(hwnd, ID_MDN + to as i32));
    let target = if dir < 0 { [up, down] } else { [down, up] };
    for t in target {
        if unsafe { IsWindowEnabled(t) } != 0 {
            unsafe { SetFocus(t) };
            break;
        }
    }
    if let Some(u) = ui() {
        u.ensure_focus_visible();
    }
}

fn update_slider_labels(_hwnd: HWND) {
    let Some(u) = ui() else { return };
    for id in [ID_SCALE, ID_OPACITY] {
        if let Some(rc) = u.value_rect(id) {
            unsafe { InvalidateRect(u.page_wnd, &rc, 0) };
        }
    }
}

/// Enable or disable the controls of every row gated by a toggle, and repaint.
fn apply_gates(hwnd: HWND) {
    let Some(u) = ui() else { return };
    for entries in &u.pages {
        for e in entries {
            let Entry::Row(r) = e else { continue };
            if r.gate == 0 {
                continue;
            }
            let on = toggle_get(ctl(hwnd, r.gate)) as i32;
            for c in &r.cells {
                if let Cell::Ctl(id, k) = c {
                    unsafe {
                        EnableWindow(ctl(hwnd, *id), on);
                        if matches!(k, Ctl::Number { .. }) {
                            EnableWindow(ctl(hwnd, *id + SPIN), on);
                        }
                    }
                }
            }
        }
    }
    unsafe { InvalidateRect(u.page_wnd, std::ptr::null(), 0) };
}

fn update_colour_rows(hwnd: HWND) {
    let Some(u) = ui() else { return };
    for (i, m) in Metric::ALL.iter().enumerate() {
        let has = u.draft.colors.iter().any(|(x, _)| x == m);
        unsafe {
            EnableWindow(ctl(hwnd, ID_CRESET + i as i32), has as i32);
            InvalidateRect(ctl(hwnd, ID_CRESET + i as i32), std::ptr::null(), 0);
            InvalidateRect(ctl(hwnd, ID_SWATCH + i as i32), std::ptr::null(), 0);
        }
    }
}

/// Push the draft into the controls.
fn populate(hwnd: HWND) {
    let Some(u) = ui() else { return };
    let d = u.draft.clone();
    let auto = u.autostart;
    let adapter_pos = d.adapter.is_empty().then_some(0).unwrap_or_else(|| {
        u.adapters.iter().position(|a| a.eq_ignore_ascii_case(&d.adapter)).map_or(0, |p| p + 1)
    });
    let was = LOADING.swap(true, Relaxed);
    let h = |id| ctl(hwnd, id);

    let iv = h(ID_INTERVAL);
    let n = send(iv, CB_GETCOUNT, 0, 0);
    for i in 0..n {
        if send(iv, CB_GETITEMDATA, i as usize, 0) == d.interval_ms as isize {
            send(iv, CB_SETCURSEL, i as usize, 0);
        }
    }
    send(h(ID_UNIT), CB_SETCURSEL, SpeedUnit::ALL.iter().position(|u| *u == d.unit).unwrap_or(0), 0);
    send(h(ID_THEME), CB_SETCURSEL, THEMES.iter().position(|(_, t)| *t == d.theme).unwrap_or(0), 0);
    send(h(ID_SCALE), TBM_SETPOS, 1, d.text_scale as isize);
    send(h(ID_OPACITY), TBM_SETPOS, 1, d.bg_opacity as isize);
    let fc = h(ID_FONT);
    match FONTS.iter().position(|f| f.eq_ignore_ascii_case(&d.font)) {
        _ if d.font.is_empty() => {
            send(fc, CB_SETCURSEL, 0, 0);
        }
        Some(i) => {
            send(fc, CB_SETCURSEL, i, 0);
        }
        None => set_text(fc, &d.font),
    }
    for (id, on) in [
        (ID_SPARK, d.sparklines),
        (ID_LABELS, d.labels),
        (ID_STACKED, d.stacked),
        (ID_AUTOSTACK, d.auto_stack),
        (ID_FULLSCREEN, d.hide_fullscreen),
        (ID_CLICKTHRU, d.click_through),
        (ID_AUTOUPDATE, d.auto_update),
        (ID_AUTOSTART, auto),
        (ID_ALERTS, d.alerts),
        (ID_A_CAP, d.alert_cap),
        (ID_WINACCENT, d.windows_accent),
    ] {
        toggle_set(h(id), on);
    }
    let pct = |v: f32| format!("{:.0}", v);
    set_text(h(ID_A_CPU), &pct(d.alert_cpu_pct));
    set_text(h(ID_A_RAM), &pct(d.alert_ram_pct));
    set_text(h(ID_A_SECS), &d.alert_secs.to_string());
    set_text(h(ID_A_DISK), &pct(d.alert_disk_free_pct));
    set_text(h(ID_A_BAT), &d.alert_battery_pct.to_string());
    set_text(h(ID_WARN), &pct(d.warn_pct));
    set_text(h(ID_CRIT), &pct(d.crit_pct));
    set_text(h(ID_CAP), &format!("{}", d.data_cap_gb));
    set_text(h(ID_DAY), &d.cap_reset_day.to_string());
    send(h(ID_ADAPTER), CB_SETCURSEL, adapter_pos, 0);
    LOADING.store(was, Relaxed);

    update_slider_labels(hwnd);
    apply_gates(hwnd);
    update_colour_rows(hwnd);
    sync_metric_rows(hwnd);
}

/// Read the controls into the draft, clamping every numeric field.
fn collect(hwnd: HWND) {
    let Some(u) = ui() else { return };
    let mut d = u.draft.clone();
    let h = |id| ctl(hwnd, id);

    let sel = send(h(ID_INTERVAL), CB_GETCURSEL, 0, 0);
    if sel >= 0 {
        let ms = send(h(ID_INTERVAL), CB_GETITEMDATA, sel as usize, 0);
        d.interval_ms = (ms as u32).clamp(250, 10_000);
    }
    let sel = send(h(ID_UNIT), CB_GETCURSEL, 0, 0);
    if let Some(un) = (sel >= 0).then(|| SpeedUnit::ALL.get(sel as usize)).flatten() {
        d.unit = *un;
    }
    let sel = send(h(ID_THEME), CB_GETCURSEL, 0, 0);
    if let Some((_, t)) = (sel >= 0).then(|| THEMES.get(sel as usize)).flatten() {
        d.theme = *t;
    }
    d.text_scale = (send(h(ID_SCALE), TBM_GETPOS, 0, 0) as u32).clamp(70, 200);
    d.bg_opacity = (send(h(ID_OPACITY), TBM_GETPOS, 0, 0) as u32).min(255) as u8;
    let f = get_text(h(ID_FONT)).trim().to_string();
    d.font = if f == FONTS[0] { String::new() } else { f };

    d.sparklines = toggle_get(h(ID_SPARK));
    d.labels = toggle_get(h(ID_LABELS));
    d.stacked = toggle_get(h(ID_STACKED));
    d.auto_stack = toggle_get(h(ID_AUTOSTACK));
    d.hide_fullscreen = toggle_get(h(ID_FULLSCREEN));
    d.click_through = toggle_get(h(ID_CLICKTHRU));
    d.auto_update = toggle_get(h(ID_AUTOUPDATE));
    d.alerts = toggle_get(h(ID_ALERTS));
    d.alert_cap = toggle_get(h(ID_A_CAP));
    d.windows_accent = toggle_get(h(ID_WINACCENT));
    let autostart = toggle_get(h(ID_AUTOSTART));

    d.alert_cpu_pct = pct_field(h(ID_A_CPU), d.alert_cpu_pct, 0.0, 100.0);
    d.alert_ram_pct = pct_field(h(ID_A_RAM), d.alert_ram_pct, 0.0, 100.0);
    d.alert_secs = int_field(h(ID_A_SECS), d.alert_secs, 5, 3600);
    d.alert_disk_free_pct = pct_field(h(ID_A_DISK), d.alert_disk_free_pct, 0.0, 100.0);
    d.alert_battery_pct = int_field(h(ID_A_BAT), d.alert_battery_pct, 0, 100);
    d.warn_pct = pct_field(h(ID_WARN), d.warn_pct, 1.0, 100.0);
    d.crit_pct = pct_field(h(ID_CRIT), d.crit_pct, 1.0, 100.0);
    if d.crit_pct < d.warn_pct {
        d.crit_pct = d.warn_pct;
    }
    d.data_cap_gb = parse_f(h(ID_CAP)).unwrap_or(d.data_cap_gb).clamp(0.0, 1_000_000.0);
    d.cap_reset_day = int_field(h(ID_DAY), d.cap_reset_day, 1, 28);

    let sel = send(h(ID_ADAPTER), CB_GETCURSEL, 0, 0);
    if sel == 0 {
        d.adapter.clear();
    } else if sel > 0 {
        if let Some(a) = u.adapters.get(sel as usize - 1) {
            d.adapter = a.clone();
        }
    }

    u.draft = d;
    u.autostart = autostart;
}

/// Copy the settings this window owns into `dst`; everything else (widget
/// position, displays, per-display stacking, ...) keeps its live value.
fn merge(dst: &mut Settings, src: &Settings) {
    dst.show = src.show;
    dst.order = src.order.clone();
    dst.colors = src.colors.clone();
    dst.windows_accent = src.windows_accent;
    dst.sparklines = src.sparklines;
    dst.labels = src.labels;
    dst.stacked = src.stacked;
    dst.auto_stack = src.auto_stack;
    dst.auto_update = src.auto_update;
    dst.unit = src.unit;
    dst.adapter = src.adapter.clone();
    dst.interval_ms = src.interval_ms;
    dst.hide_fullscreen = src.hide_fullscreen;
    dst.click_through = src.click_through;
    dst.text_scale = src.text_scale;
    dst.theme = src.theme;
    dst.bg_opacity = src.bg_opacity;
    dst.font = src.font.clone();
    dst.warn_pct = src.warn_pct;
    dst.crit_pct = src.crit_pct;
    dst.data_cap_gb = src.data_cap_gb;
    dst.cap_reset_day = src.cap_reset_day;
    dst.alerts = src.alerts;
    dst.alert_cpu_pct = src.alert_cpu_pct;
    dst.alert_ram_pct = src.alert_ram_pct;
    dst.alert_secs = src.alert_secs;
    dst.alert_disk_free_pct = src.alert_disk_free_pct;
    dst.alert_battery_pct = src.alert_battery_pct;
    dst.alert_cap = src.alert_cap;
}

fn mark_dirty(hwnd: HWND) {
    if LOADING.load(Relaxed) || !READY.load(Relaxed) {
        return;
    }
    unsafe { EnableWindow(ctl(hwnd, ID_APPLY), 1) };
}

/// Validate, apply to the app, save. Returns false if the app is gone.
fn commit(hwnd: HWND) -> bool {
    collect(hwnd);
    let Some((draft, autostart)) = ui().map(|u| (u.draft.clone(), u.autostart)) else { return false };
    if autostart != crate::autostart::enabled() {
        crate::autostart::set(autostart);
    }
    let Some(a) = app() else { return false };
    let mut live = a.m.s.clone();
    merge(&mut live, &draft);
    a.apply_new_settings(live);
    let (theme, dark) = (a.m.s.theme, effective_dark(a.m.s.theme));

    // Show the clamped values and the new baseline.
    populate(hwnd);
    unsafe {
        SetFocus(ctl(hwnd, IDOK));
        EnableWindow(ctl(hwnd, ID_APPLY), 0);
    }
    if let Some(u) = ui() {
        if u.dark != dark || u.theme != theme {
            u.dark = dark;
            u.theme = theme;
            apply_theme(hwnd);
        }
    }
    true
}

fn reset_colour(hwnd: HWND, i: usize) {
    if let Some(u) = ui() {
        let m = Metric::ALL[i];
        u.draft.colors.retain(|(x, _)| *x != m);
    }
    update_colour_rows(hwnd);
    mark_dirty(hwnd);
}

fn pick_colour(hwnd: HWND, i: usize) {
    let m = Metric::ALL[i];
    let Some((cur, mut cust)) = ui().map(|u| (u.swatch_rgb(m), u.cust)) else { return };
    let mut cc: CHOOSECOLORW = unsafe { std::mem::zeroed() };
    cc.lStructSize = std::mem::size_of::<CHOOSECOLORW>() as u32;
    cc.hwndOwner = hwnd;
    cc.rgbResult = rgb_to_cr(cur);
    cc.lpCustColors = cust.as_mut_ptr();
    cc.Flags = CC_RGBINIT | CC_FULLOPEN | CC_ANYCOLOR;
    // Modal: runs a nested message loop. No `ui()` borrow is held here.
    let ok = unsafe { ChooseColorW(&mut cc) } != 0;
    let Some(u) = ui() else { return };
    u.cust = cust;
    if ok {
        let rgb = cr_to_rgb(cc.rgbResult) & 0xFF_FFFF;
        u.draft.colors.retain(|(x, _)| *x != m);
        u.draft.colors.push((m, rgb));
        update_colour_rows(hwnd);
        mark_dirty(hwnd);
    }
}

fn on_toggle(hwnd: HWND, id: i32) {
    let h = ctl(hwnd, id);
    let on = !toggle_get(h);
    toggle_set(h, on);
    match id {
        ID_ALERTS => apply_gates(hwnd),
        ID_WINACCENT => {
            if let Some(u) = ui() {
                u.draft.windows_accent = on;
            }
            update_colour_rows(hwnd);
        }
        _ if (ID_MTOG..ID_MTOG + Metric::ALL.len() as i32).contains(&id) => {
            if let Some(u) = ui() {
                if let Some(m) = u.draft.order.get((id - ID_MTOG) as usize).copied() {
                    u.draft.show[m.idx()] = on;
                }
            }
        }
        _ => {}
    }
    mark_dirty(hwnd);
}

// ------------------------------------------------------------------ owner draw

fn draw_toggle(cv: &mut Canvas, u: &Ui, on: bool, pressed: bool, disabled: bool, focus: bool) {
    let p = &u.pal;
    let sc = |v| u.sc(v);
    let (w, h) = (cv.w, cv.h);
    let (tw, th) = (sc(40), sc(20));
    let track = Rect { l: w - tw - sc(2), t: (h - th) / 2, r: w - sc(2), b: (h - th) / 2 + th };
    let rad = th as f32 / 2.0;
    let cy = (track.t + track.b) / 2;
    let kd = if pressed { sc(14) } else { sc(12) };
    if on {
        let (fill, knob) = if disabled { (p.accent.mix(p.card, 0.6), p.on_accent.mix(p.card, 0.4)) } else { (p.accent, p.on_accent) };
        cv.fill_round(track, rad, fill);
        disc(cv, track.r - sc(10), cy, kd, knob);
    } else {
        let (edge, knob) = if disabled { (p.off, p.off) } else { (p.toggle_edge, p.knob_off) };
        let b = sc(1).max(1);
        cv.fill_round(track, rad, edge);
        cv.fill_round(inflate(track, -b), rad - b as f32, p.card);
        disc(cv, track.l + sc(10), cy, kd, knob);
    }
    let label = if on { "On" } else { "Off" };
    let lw = cv.measure(u.fonts.body, label).0;
    text_mid(cv, u.fonts.body, label, track.l - sc(12) - lw, cy, if disabled { p.off } else { p.fg });
    if focus {
        ring(cv, inflate(track, sc(3)), rad + sc(3) as f32, p.fg);
    }
}

fn draw_swatch(cv: &mut Canvas, u: &Ui, m: Metric, pressed: bool, focus: bool) {
    let p = &u.pal;
    let (w, h) = (cv.w, cv.h);
    let (sw, sh) = (u.sc(32), u.sc(20));
    let r = Rect { l: (w - sw) / 2, t: (h - sh) / 2, r: (w - sw) / 2 + sw, b: (h - sh) / 2 + sh };
    let rgb = u.swatch_rgb(m);
    let mut c = hx(rgb);
    if pressed {
        c = c.mix(hx(0x000000), 0.18);
    }
    let rad = u.sc(4) as f32;
    cv.fill_round(r, rad, c);
    cv.stroke_round(r, rad, p.fg.with_a(90));
    if focus {
        ring(cv, inflate(r, u.sc(3)), rad + u.sc(3) as f32, p.fg);
    }
}

fn draw_link(cv: &mut Canvas, u: &Ui, text: &str, pressed: bool, disabled: bool, focus: bool) {
    let p = &u.pal;
    let f = u.fonts.body;
    let (tw, th) = cv.measure(f, text);
    let (x, y) = ((cv.w - tw) / 2, (cv.h - th) / 2);
    let c = if disabled {
        p.off
    } else if pressed {
        p.accent.mix(p.card, 0.45)
    } else {
        p.accent
    };
    cv.text(f, text, x, y, c);
    if focus {
        ring(cv, inflate(Rect { l: x, t: y, r: x + tw, b: y + th }, u.sc(4)), u.sc(4) as f32, p.fg);
    }
}

fn draw_glyph_button(cv: &mut Canvas, u: &Ui, glyph: char, pressed: bool, disabled: bool, focus: bool) {
    let p = &u.pal;
    let (w, h) = (cv.w, cv.h);
    let r = Rect { l: u.sc(2), t: u.sc(2), r: w - u.sc(2), b: h - u.sc(2) };
    let rad = u.sc(4) as f32;
    let fill = if pressed { p.nav_sel } else { p.chip };
    cv.fill_round(r, rad, fill);
    cv.stroke_round(r, rad, p.border);
    let mut buf = [0u8; 4];
    let g = glyph.encode_utf8(&mut buf);
    let (gw, gh) = cv.measure(u.fonts.icon_sm, g);
    cv.text(u.fonts.icon_sm, g, (w - gw) / 2, (h - gh) / 2, if disabled { p.off } else { p.fg });
    if focus {
        ring(cv, r, rad, p.fg);
    }
}

fn draw_ok(cv: &mut Canvas, u: &Ui, pressed: bool, focus: bool) {
    let p = &u.pal;
    let r = Rect { l: 0, t: 0, r: cv.w, b: cv.h };
    let rad = u.sc(4) as f32;
    let fill = if pressed { p.accent.mix(hx(0x000000), 0.15) } else { p.accent };
    cv.fill_round(r, rad, fill);
    let (tw, th) = cv.measure(u.fonts.semi, "OK");
    cv.text(u.fonts.semi, "OK", (r.w() - tw) / 2, (r.h() - th) / 2, p.on_accent);
    if focus {
        ring(cv, r, rad, p.fg);
    }
}

fn draw_owner(dis: &DRAWITEMSTRUCT) {
    let Some(u) = ui() else { return };
    let u: &Ui = u;
    let rc = dis.rcItem;
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    if w <= 0 || h <= 0 {
        return;
    }
    let id = dis.CtlID as i32;
    let st = dis.itemState;
    let (pressed, disabled) = (st & ODS_SELECTED != 0, st & ODS_DISABLED != 0);
    let focus = st & ODS_FOCUS != 0 && st & ODS_NOFOCUSRECT == 0;

    let mut cv = Canvas::new();
    cv.resize(w, h);
    cv.clear(if id == IDOK { u.pal.footer } else { u.pal.card });
    let metrics = Metric::ALL.len() as i32;
    if is_toggle(id) {
        draw_toggle(&mut cv, u, toggle_get(dis.hwndItem), pressed, disabled, focus);
    } else if id == IDOK {
        draw_ok(&mut cv, u, pressed, focus);
    } else if (ID_SWATCH..ID_SWATCH + metrics).contains(&id) {
        draw_swatch(&mut cv, u, Metric::ALL[(id - ID_SWATCH) as usize], pressed, focus);
    } else if (ID_CRESET..ID_CRESET + metrics).contains(&id) {
        draw_link(&mut cv, u, "Reset", pressed, disabled, focus);
    } else if (ID_MUP..ID_MUP + metrics).contains(&id) {
        draw_glyph_button(&mut cv, u, '\u{E70E}', pressed, disabled, focus);
    } else if (ID_MDN..ID_MDN + metrics).contains(&id) {
        draw_glyph_button(&mut cv, u, '\u{E70D}', pressed, disabled, focus);
    }
    cv.blit(dis.hDC, rc.left, rc.top);
}

/// Custom-draw of the trackbar: thin rounded channel filled with the accent up
/// to a round thumb with an accent dot. Painted in one buffered pass.
fn draw_slider(nm: &NMCUSTOMDRAW) -> LRESULT {
    if nm.dwDrawStage != CDDS_PREPAINT {
        return CDRF_DODEFAULT as LRESULT;
    }
    let Some(u) = ui() else { return CDRF_DODEFAULT as LRESULT };
    let u: &Ui = u;
    let p = &u.pal;
    let hw = nm.hdr.hwndFrom;
    let (w, h) = (nm.rc.right - nm.rc.left, nm.rc.bottom - nm.rc.top);
    if w <= 0 || h <= 0 {
        return CDRF_DODEFAULT as LRESULT;
    }
    let sc = |v| u.sc(v);
    let mut cv = Canvas::new();
    cv.resize(w, h);
    cv.clear(p.card);

    let mut thumb: RECT = unsafe { std::mem::zeroed() };
    send(hw, TBM_GETTHUMBRECT, 0, &mut thumb as *mut RECT as isize);
    let r = sc(10);
    let cx = ((thumb.left + thumb.right) / 2).clamp(r, (w - r).max(r));
    let cy = h / 2;
    let th = sc(4).max(2);
    let enabled = unsafe { IsWindowEnabled(hw) } != 0;
    let (edge, accent) = (sc(4), if enabled { p.accent } else { p.off });
    let track = Rect { l: edge, t: cy - th / 2, r: w - edge, b: cy - th / 2 + th };
    let rail = if p.card.0 < 128 { hx(0x777777) } else { hx(0x8A8A8A) };
    cv.fill_round(track, th as f32 / 2.0, rail.mix(p.card, 0.35));
    cv.fill_round(Rect { r: cx, ..track }, th as f32 / 2.0, accent);

    let pressed = unsafe { GetCapture() } == hw;
    let outer = Rect { l: cx - r, t: cy - r, r: cx + r, b: cy + r };
    let (face, rim) = if p.card.0 < 128 { (hx(0x454545), hx(0x5C5C5C)) } else { (hx(0xFFFFFF), hx(0xCFCFCF)) };
    cv.fill_round(outer, r as f32, rim);
    cv.fill_round(inflate(outer, -1), (r - 1) as f32, face);
    disc(&mut cv, cx, cy, if pressed { sc(10) } else { sc(12) }, accent);
    if unsafe { GetFocus() } == hw {
        ring(&mut cv, inflate(outer, sc(2)), (r + sc(2)) as f32, p.fg);
    }
    cv.blit(nm.hdc, nm.rc.left, nm.rc.top);
    CDRF_SKIPDEFAULT as LRESULT
}

/// The trackbar erases its own background on paint; we paint all of it, so
/// swallow the erase to avoid flicker.
unsafe extern "system" fn slider_sub(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM, _id: usize, _data: usize) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return 1;
    }
    DefSubclassProc(hwnd, msg, w, l)
}

// ------------------------------------------------------------------ window

pub fn register(hinst: HINSTANCE) {
    unsafe {
        let icon = LoadIconW(hinst, 1 as _);
        let cursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        for (name, proc, ic) in [
            (CLASS, Some(wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT), icon),
            (NAV_CLASS, Some(nav_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT), std::ptr::null_mut()),
            (PAGE_CLASS, Some(page_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT), std::ptr::null_mut()),
        ] {
            let cls = wide(name);
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: proc,
                hInstance: hinst,
                hIcon: ic,
                hCursor: cursor,
                lpszClassName: cls.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&wc);
        }
    }
}

const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN;

/// Size the window for `dpi` (never larger than the work area) and centre it.
fn fit(hwnd: HWND, dpi: u32) {
    unsafe {
        let sc = |v: i32| (v * dpi as i32 + 48) / 96;
        let mut rc = RECT { left: 0, top: 0, right: sc(CLIENT_W), bottom: sc(CLIENT_H) };
        AdjustWindowRectExForDpi(&mut rc, STYLE, 0, WS_EX_CONTROLPARENT, dpi);
        let mut work: RECT = std::mem::zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut _ as _, 0);
        let (ww, wh) = (work.right - work.left, work.bottom - work.top);
        let (w, h) = ((rc.right - rc.left).min(ww), (rc.bottom - rc.top).min(wh));
        let x = work.left + (ww - w) / 2;
        let y = work.top + ((wh - h) / 2).max(0);
        SetWindowPos(hwnd, std::ptr::null_mut(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Open the Settings window, or bring the existing one to the front.
pub fn open(a: &mut crate::app::App) {
    unsafe {
        if !a.settings_win.is_null() {
            ShowWindow(a.settings_win, SW_RESTORE);
            SetForegroundWindow(a.settings_win);
            return;
        }
        let hinst = GetModuleHandleW(std::ptr::null());
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES | ICC_UPDOWN_CLASS | ICC_STANDARD_CLASSES,
        };
        InitCommonControlsEx(&icc);
        // Process-wide, and before any window exists so menus pick it up too.
        darkmode::set_app_mode(a.m.s.theme);

        let mut draft = a.m.s.clone();
        // Make sure the order lists every metric exactly once.
        let mut order: Vec<Metric> = Vec::new();
        for m in draft.order.iter().chain(Metric::ALL.iter()) {
            if !order.contains(m) {
                order.push(*m);
            }
        }
        draft.order = order;

        // Adapters present now, plus the configured one if it is not.
        let mut adapters: Vec<String> = Vec::new();
        let mut labels: Vec<String> = Vec::new();
        for ad in a.adapter_list() {
            let l = if ad.description.is_empty() || ad.description == ad.alias {
                ad.alias.clone()
            } else {
                format!("{} \u{2013} {}", ad.alias, ad.description)
            };
            labels.push(if ad.connected { l } else { format!("{} (not connected)", l) });
            adapters.push(ad.alias);
        }
        if !draft.adapter.is_empty() && !adapters.iter().any(|x| x.eq_ignore_ascii_case(&draft.adapter)) {
            adapters.push(draft.adapter.clone());
            labels.push(format!("{} (not available)", draft.adapter));
        }

        let dpi = GetDpiForSystem().max(96);
        let ui = Box::new(Ui::new(hinst, dpi, a.m.dark, a.m.s.theme, draft, adapters, labels));
        free_ui();
        UI = Box::into_raw(ui);

        let cls = wide(CLASS);
        let title = wide("Resource Monitor Settings");
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            cls.as_ptr(),
            title.as_ptr(),
            STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            100,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            free_ui();
            return;
        }
        a.settings_win = hwnd;
        fit(hwnd, ui_dpi());
        ShowWindow(hwnd, SW_SHOWNORMAL);
        SetForegroundWindow(hwnd);
    }
}

fn ui_dpi() -> u32 {
    ui().map_or(96, |u| u.dpi)
}

/// Keyboard hook for the message loop (called before `IsDialogMessageW`):
/// Ctrl+Tab / Ctrl+Shift+Tab switch pages, and after Tab or an arrow key the
/// newly focused control is scrolled into view. Returns true if the message was used.
pub fn pre_translate(win: HWND, msg: &MSG) -> bool {
    if win.is_null() || msg.message != WM_KEYDOWN {
        return false;
    }
    let key = msg.wParam as u16;
    if !matches!(key, VK_TAB | VK_UP | VK_DOWN | VK_LEFT | VK_RIGHT) {
        return false;
    }
    unsafe {
        if msg.hwnd != win && IsChild(win, msg.hwnd) == 0 {
            return false;
        }
        if key == VK_TAB && GetKeyState(VK_CONTROL as i32) < 0 {
            let back = GetKeyState(VK_SHIFT as i32) < 0;
            if let Some(u) = ui() {
                let n = PAGES.len();
                let next = if back { (u.page + n - 1) % n } else { (u.page + 1) % n };
                u.show_page(next);
            }
            return true;
        }
        PostMessageW(page_hwnd(), WM_APP_ENSURE, 0, 0);
    }
    false
}

/// Forward a message of the page container's child to the main window.
unsafe fn to_main(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    SendMessageW(GetParent(hwnd), msg, w, l)
}

unsafe extern "system" fn page_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            if let (true, Some(u)) = (READY.load(Relaxed), ui()) {
                let mut cv = std::mem::replace(&mut u.cv_page, Canvas::new());
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                cv.resize(rc.right, rc.bottom);
                let clip: Rect = ps.rcPaint.into();
                u.draw_page(&mut cv, clip);
                cv.blit(dc, 0, 0);
                u.cv_page = cv;
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_COMMAND | WM_NOTIFY | WM_HSCROLL | WM_DRAWITEM | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            to_main(hwnd, msg, w, l)
        }
        WM_VSCROLL => {
            // l != 0: from an up-down control, not our scroll bar.
            if l == 0 {
                if let Some(u) = ui() {
                    u.scroll_cmd((w & 0xFFFF) as i32);
                }
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((w >> 16) & 0xFFFF) as u16 as i16 as i32;
            if let Some(u) = ui() {
                let by = -delta * u.sc(60) / 120;
                u.set_scroll(u.scroll + by);
            }
            0
        }
        WM_APP_ENSURE => {
            if let Some(u) = ui() {
                u.ensure_focus_visible();
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

fn nav_hit(u: &Ui, hwnd: HWND, x: i32, y: i32) -> Option<usize> {
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(hwnd, &mut rc) };
    (0..PAGES.len()).find(|&i| u.nav_rect(rc.right, i).contains(x, y))
}

unsafe extern "system" fn nav_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            if let (true, Some(u)) = (READY.load(Relaxed), ui()) {
                let mut cv = std::mem::replace(&mut u.cv_nav, Canvas::new());
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                cv.resize(rc.right, rc.bottom);
                u.draw_nav(&mut cv, GetFocus() == hwnd);
                cv.blit(dc, 0, 0);
                u.cv_nav = cv;
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_GETDLGCODE => DLGC_WANTARROWS as LRESULT,
        WM_SETFOCUS | WM_KILLFOCUS => {
            InvalidateRect(hwnd, std::ptr::null(), 0);
            0
        }
        WM_MOUSEMOVE => {
            if let Some(u) = ui() {
                let hit = nav_hit(u, hwnd, get_x(l), get_y(l));
                if hit != u.nav_hover {
                    u.nav_hover = hit;
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
                if !u.nav_tracking {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    u.nav_tracking = TrackMouseEvent(&mut t) != 0;
                }
            }
            0
        }
        WM_MOUSELEAVE => {
            if let Some(u) = ui() {
                u.nav_tracking = false;
                if u.nav_hover.take().is_some() {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            SetFocus(hwnd);
            if let Some(u) = ui() {
                if let Some(i) = nav_hit(u, hwnd, get_x(l), get_y(l)) {
                    u.show_page(i);
                }
            }
            0
        }
        WM_KEYDOWN => {
            if let Some(u) = ui() {
                let n = PAGES.len();
                let cur = u.page;
                let next = match w as u16 {
                    VK_UP => Some(cur.saturating_sub(1)),
                    VK_DOWN => Some((cur + 1).min(n - 1)),
                    VK_HOME => Some(0),
                    VK_END => Some(n - 1),
                    _ => None,
                };
                if let Some(i) = next {
                    u.show_page(i);
                }
            }
            0
        }
        WM_MOUSEWHEEL => SendMessageW(page_hwnd(), msg, w, l),
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

/// The string a WM_SETTINGCHANGE `lParam` points to (at most 63 units).
unsafe fn setting_name(l: LPARAM) -> String {
    let p = l as *const u16;
    if p.is_null() {
        return String::new();
    }
    let mut v = Vec::new();
    for i in 0..63 {
        let c = *p.add(i);
        if c == 0 {
            break;
        }
        v.push(c);
    }
    String::from_utf16_lossy(&v)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let dpi = GetDpiForWindow(hwnd);
            let Some(u) = ui() else { return -1 };
            if dpi != 0 && dpi != u.dpi {
                let old = u.set_dpi(dpi);
                old.free();
            }
            u.main = hwnd;
            u.build();
            apply_theme(hwnd);
            populate(hwnd);
            EnableWindow(ctl(hwnd, ID_APPLY), 0);
            READY.store(true, Relaxed);
            0
        }
        WM_SIZE => {
            if let (true, Some(u)) = (READY.load(Relaxed), ui()) {
                u.relayout();
            }
            0
        }
        WM_SETFOCUS => {
            // Nothing inside had the focus yet: start on the page list.
            if let Some(u) = ui() {
                SetFocus(u.nav);
            }
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            if let (true, Some(u)) = (READY.load(Relaxed), ui()) {
                let mut cv = std::mem::replace(&mut u.cv_main, Canvas::new());
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                cv.resize(rc.right, rc.bottom);
                u.draw_main(&mut cv);
                cv.blit(dc, 0, 0);
                u.cv_main = cv;
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORDLG | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let (true, Some(u)) = (READY.load(Relaxed), ui()) else { return DefWindowProcW(hwnd, msg, w, l) };
            let hdc = w as HDC;
            let p = u.pal;
            match msg {
                WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
                    SetTextColor(hdc, p.fg.colorref());
                    SetBkColor(hdc, p.input.colorref());
                    u.br_input as LRESULT
                }
                WM_CTLCOLORSTATIC => {
                    // A disabled edit reports itself as "static".
                    if class_name(l as HWND).eq_ignore_ascii_case("Edit") {
                        SetTextColor(hdc, p.off.colorref());
                        SetBkColor(hdc, p.input.colorref());
                        return u.br_input as LRESULT;
                    }
                    SetTextColor(hdc, p.fg.colorref());
                    SetBkMode(hdc, TRANSPARENT as i32);
                    u.br_card as LRESULT
                }
                WM_CTLCOLORDLG => u.br_bg as LRESULT,
                _ => DefWindowProcW(hwnd, msg, w, l),
            }
        }
        WM_DRAWITEM => {
            draw_owner(&*(l as *const DRAWITEMSTRUCT));
            1
        }
        DM_GETDEFID => ((DC_HASDEFID << 16) | IDOK as u32) as LRESULT,
        WM_COMMAND => {
            let id = (w & 0xFFFF) as i32;
            let code = ((w >> 16) & 0xFFFF) as u32;
            let clicked = code == BN_CLICKED || code == BN_DOUBLECLICKED;
            let metrics = Metric::ALL.len() as i32;
            match id {
                IDOK => {
                    if commit(hwnd) {
                        DestroyWindow(hwnd);
                    }
                }
                IDCANCEL => {
                    DestroyWindow(hwnd);
                }
                ID_APPLY => {
                    commit(hwnd);
                }
                ID_RESETALL => {
                    if let Some(u) = ui() {
                        u.draft.colors.clear();
                    }
                    update_colour_rows(hwnd);
                    mark_dirty(hwnd);
                }
                ID_REPORT => {
                    if let Some(a) = app() {
                        crate::report::open(a);
                    }
                }
                _ if is_toggle(id) => {
                    if clicked {
                        on_toggle(hwnd, id);
                    }
                }
                _ if (ID_MUP..ID_MUP + metrics).contains(&id) => {
                    if clicked {
                        move_metric(hwnd, (id - ID_MUP) as usize, -1);
                    }
                }
                _ if (ID_MDN..ID_MDN + metrics).contains(&id) => {
                    if clicked {
                        move_metric(hwnd, (id - ID_MDN) as usize, 1);
                    }
                }
                _ if (ID_SWATCH..ID_SWATCH + metrics).contains(&id) => {
                    if clicked {
                        pick_colour(hwnd, (id - ID_SWATCH) as usize);
                    }
                }
                _ if (ID_CRESET..ID_CRESET + metrics).contains(&id) => {
                    if clicked {
                        reset_colour(hwnd, (id - ID_CRESET) as usize);
                    }
                }
                _ => {
                    if matches!(code, EN_CHANGE | CBN_SELCHANGE | CBN_EDITCHANGE) && l != 0 {
                        mark_dirty(hwnd);
                    }
                }
            }
            0
        }
        WM_HSCROLL => {
            update_slider_labels(hwnd);
            mark_dirty(hwnd);
            0
        }
        WM_NOTIFY => {
            let nm = &*(l as *const NMHDR);
            if nm.code == NM_CUSTOMDRAW && (nm.idFrom == ID_SCALE as usize || nm.idFrom == ID_OPACITY as usize) {
                return draw_slider(&*(l as *const NMCUSTOMDRAW));
            }
            0
        }
        WM_MOUSEWHEEL => SendMessageW(page_hwnd(), msg, w, l),
        WM_SETTINGCHANGE => {
            // The user switched Windows dark/light mode or accent colour.
            if setting_name(l) == "ImmersiveColorSet" {
                if let Some(u) = ui() {
                    u.dark = effective_dark(u.theme);
                    apply_theme(hwnd);
                }
            }
            DefWindowProcW(hwnd, msg, w, l)
        }
        WM_DPICHANGED => {
            let dpi = ((w >> 16) & 0xFFFF) as u32;
            let r = &*(l as *const RECT);
            let old = ui().map(|u| u.set_dpi(dpi));
            SetWindowPos(hwnd, std::ptr::null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            if let Some(u) = ui() {
                u.relayout();
            }
            if let Some(old) = old {
                old.free();
            }
            InvalidateRect(hwnd, std::ptr::null(), 1);
            0
        }
        WM_DESTROY => {
            if let Some(a) = app() {
                a.settings_win = std::ptr::null_mut();
            }
            0
        }
        WM_NCDESTROY => {
            free_ui();
            DefWindowProcW(hwnd, msg, w, l)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
