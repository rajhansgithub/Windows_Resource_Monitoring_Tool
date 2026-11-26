//! The Resource Monitor main window, in the style of Windows 11 Settings and
//! the Fluent NavigationView: a navigation pane on the left (Overview,
//! Achievements, the five Insights pages, the settings pages, About pinned in
//! the footer) and the selected page on the right under a fixed page header,
//! all on the Mica backdrop (Windows 11). One window: every entry point (tray
//! menu, `--insights`, a second launch, a click on an Insights notification)
//! navigates this window instead of opening another one.
//!
//! The pane follows WinUI's display modes: expanded (icons + labels) when the
//! window is at least 1008 epx wide, compact (icons only, with tooltips) below;
//! in compact mode the hamburger opens the full pane as an overlay over the
//! content. Back (Alt+Left, the mouse back button, the header arrow) walks a
//! short history of visited pages. Size, position, maximized state and the
//! last page are remembered in settings.ini.
//!
//! There are no child windows. The whole client area is one surface rendered
//! into a `render::Canvas` (premultiplied BGRA) and copied to the window in a
//! single `BitBlt`, so nothing flickers and nothing shows up as white blocks
//! while scrolling. Every control (toggle switch, drop-down, slider, number
//! stepper, buttons, colour swatches, navigation items) is painted and
//! hit-tested here; Insights panels are laid out and painted by
//! `insights::view`.
//!
//! With Mica the background is painted with alpha 0 and the frame is extended
//! into the client area, so DWM shows the backdrop through it; cards and hover
//! fills are translucent white / black like the system's own. On Windows 10
//! (or if the backdrop cannot be enabled) opaque fallback colours are used.
//!
//! The settings pages edit a *copy* of the settings; each change is merged
//! into the live settings via `App::apply_new_settings` ~150 ms later
//! (debounced), and anything pending is applied when the window closes.
//!
//! Cost rules: nothing runs while the window is idle. Insights data is rebuilt
//! at most every 2 s, only while an Insights page or Overview is showing, and
//! the window repaints only when the visible fingerprint changed. The one
//! animation timer (smooth scroll, page transition, selection indicator, pane
//! overlay) runs only while something is moving.
//!
//! Re-entrancy rules (single UI thread, raw Win32):
//! * `ui()` borrows are short; none is used after a call that runs a nested
//!   message loop (`TrackPopupMenuEx`, `ChooseColorW`, `apply_new_settings`,
//!   `DestroyWindow`): such calls are made from free functions that fetch
//!   `ui()` again afterwards.
//! * The layout (controls and cards of the current page) is rebuilt from the
//!   draft after every change; it is a few dozen rectangles.

use crate::app::{app, default_accent, effective_dark, Cmd, C_CRIT, C_WARN};
use crate::fmt::SpeedUnit;
use crate::insights::view::{self, View};
use crate::insights::{unix_ms, Panel};
use crate::nav::{self, Entry, History, Page, Placement};
use crate::popup::style_window;
use crate::render::{create_font, measure_dc, ui_face, Canvas, Rgba};
use crate::settings::{Metric, Mode, Settings, Theme};
use crate::util::*;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Instant;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmGetColorizationColor, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::UI::Controls::Dialogs::*;
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_HOVER, TME_LEAVE, TRACKMOUSEEVENT, VK_BROWSER_BACK, VK_CONTROL,
    VK_DOWN, VK_END, VK_ESCAPE, VK_F4, VK_F6, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
    VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Window class of the main window (CI finds the window by it). Must differ
/// from `app::MAIN_CLASS` (the hidden message window): with the same name our
/// RegisterClassExW fails and CreateWindowExW makes a second app window that
/// never paints.
pub const CLASS: &str = "ResourceMonitor.Window";
const TITLE: &str = "Resource Monitor";

// ------------------------------------------------------------------ layout
// All values are device-independent pixels (96 dpi); `Ui::sc` scales them.

/// Default and minimum client size.
const DEF_W: i32 = 1080;
const DEF_H: i32 = 720;
const MIN_W: i32 = 720;
const MIN_H: i32 = 520;
/// Navigation pane width: expanded (icons + labels) and compact (icons only).
const PANE_W: i32 = 296;
const PANE_COMPACT: i32 = 48;
/// Inset of the pane's items from its edges.
const NAV_INSET: i32 = 4;
const NAV_ITEM_H: i32 = 36;
const NAV_GAP: i32 = 4;
/// Top of the first menu item (the hamburger button sits above it).
const MENU_TOP: i32 = 48;
const NAV_HEAD_H: i32 = 36;
const NAV_SEP_H: i32 = 9;
/// Page header (back button + title) above the scrolling content.
const HEADER_H: i32 = 76;
/// Margin of the content from the pane and the window edge; max content width.
const PAGE_PAD: i32 = 24;
const CONTENT_MAX: i32 = 1000;
const CARD_PAD: i32 = 16;
const ROW_H: i32 = 64;
const ROW_GAP: i32 = 4;
const HEAD_TOP: i32 = 24;
const HEAD_H: i32 = 28;
const NOTE_H: i32 = 36;
const SUMMARY_H: i32 = 92;
const CTL_H: i32 = 32;
const CELL_GAP: i32 = 8;
const WHEEL_STEP: i32 = 48;
const BTN_W: i32 = 112;
const STEPPER_W: i32 = 132;

/// Smooth scroll and every transition; runs only while something moves.
const T_ANIM: usize = 1;
const T_REPEAT: usize = 2;
/// Debounced live apply: changes take effect ~150 ms after the last edit.
const T_LIVE: usize = 3;
/// Durations of the transitions (Fluent "fast" ~150 ms, pane ~200 ms).
const PAGE_ANIM_S: f32 = 0.15;
const IND_ANIM_S: f32 = 0.15;
const PANE_ANIM_S: f32 = 0.2;
/// Page content slides up this far while it fades in.
const PAGE_SLIDE: i32 = 8;
/// Rebuild the Insights panels at most this often while they are showing.
const REBUILD_MS: u64 = 2000;

// ------------------------------------------------------------------ ids

const ID_HAMBURGER: i32 = 20;
const ID_BACK: i32 = 21;
/// + `Page::index`.
const ID_NAV: i32 = 40;
/// + panel index (Overview summary cards).
const ID_SUMMARY: i32 = 70;

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
const ID_MODE: i32 = 114;
const ID_MAXW: i32 = 115;
const ID_RESETPOS: i32 = 116;
const ID_RECHECK: i32 = 117;
const ID_SHOWON: i32 = 118;
const ID_ACHIEVE: i32 = 199;

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

const ID_WEBSITE: i32 = 650;
const ID_UPDATES: i32 = 651;
const ID_EDITFILE: i32 = 652;
const ID_DATADIR: i32 = 653;

// Insights (Overview, Alerts, Sleep Verdict) and Achievements.
const ID_INSIGHTS: i32 = 900;
const ID_INS_TOASTS: i32 = 901;
const ID_BAGGUARD: i32 = 902;
const ID_ACH_OPEN: i32 = 903;
const ID_INS_CLEAR: i32 = 904;

// Per-display toggles, + index in `Ui::displays`.
const ID_DSHOW: i32 = 800;
const ID_DSTACK: i32 = 820;
const MAX_DISPLAYS: i32 = 16;

// Metrics page: controls are addressed by list *position*, not by metric.
const ID_MTOG: i32 = 700;
const ID_MUP: i32 = 720;
const ID_MDN: i32 = 740;

const G_CHEVRON_DOWN: char = '\u{E70D}';
const G_CHEVRON_UP: char = '\u{E70E}';
const G_CHEVRON_RIGHT: char = '\u{E76C}';
const G_MINUS: char = '\u{E738}';
const G_PLUS: char = '\u{E710}';
const G_HAMBURGER: char = '\u{E700}';
const G_BACK: char = '\u{E72B}';

const INTERVALS: [(&str, u32); 4] = [("0.5 seconds", 500), ("1 second", 1000), ("2 seconds", 2000), ("5 seconds", 5000)];
const MODES: [(&str, Mode); 2] = [("Dock to taskbar", Mode::Dock), ("Floating (drag anywhere)", Mode::Float)];
const MAX_WIDTHS: [(&str, u32); 5] =
    [("Auto (don't cover app icons)", 0), ("20% of taskbar", 20), ("30% of taskbar", 30), ("40% of taskbar", 40), ("50% of taskbar", 50)];
const THEMES: [(&str, Theme); 3] = [("Match Windows", Theme::Auto), ("Dark", Theme::Dark), ("Light", Theme::Light)];
/// Offered widget fonts; the first means "default" (empty setting).
const FONTS: [&str; 5] = ["Default", "Segoe UI", "Cascadia Mono", "Consolas", "Bahnschrift"];

// ------------------------------------------------------------------ state

static READY: AtomicBool = AtomicBool::new(false);
static mut UI: *mut Ui = std::ptr::null_mut();

#[allow(static_mut_refs)]
fn ui() -> Option<&'static mut Ui> {
    // SAFETY: single UI thread; the pointer is null or a live `Box<Ui>` owned here.
    unsafe { UI.as_mut() }
}

#[allow(static_mut_refs)]
fn free_ui() {
    // SAFETY: single UI thread; the box is swapped out before it is freed, so once.
    unsafe {
        let p = std::ptr::replace(std::ptr::addr_of_mut!(UI), std::ptr::null_mut());
        if !p.is_null() {
            let u = Box::from_raw(p);
            u.fonts.free();
            if !u.mdc.is_null() {
                DeleteDC(u.mdc);
            }
        }
    }
    READY.store(false, Relaxed);
}

// ------------------------------------------------------------------ palette

const fn hx(v: u32) -> Rgba {
    Rgba::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// White at alpha `a`.
const fn wa(a: u8) -> Rgba {
    Rgba(255, 255, 255, a)
}

/// Black at alpha `a`.
const fn ka(a: u8) -> Rgba {
    Rgba(0, 0, 0, a)
}

/// Colours of the window (Windows 11 / WinUI values). Fills are translucent
/// so the Mica backdrop shows through; text is always opaque.
#[derive(Clone, Copy)]
struct Pal {
    dark: bool,
    bg: Rgba,
    card: Rgba,
    card_border: Rgba,
    /// Opaque fills: chart value bubble, tooltip, the overlay pane.
    bubble: Rgba,
    pane: Rgba,
    nav_sel: Rgba,
    nav_hover: Rgba,
    nav_press: Rgba,
    /// Standard control (button, drop-down, number box) fills and outline.
    ctl: Rgba,
    ctl_hover: Rgba,
    ctl_press: Rgba,
    ctl_dis: Rgba,
    ctl_border: Rgba,
    /// Glyph buttons and other "subtle" controls.
    subtle_hover: Rgba,
    subtle_press: Rgba,
    fg: Rgba,
    dim: Rgba,
    off: Rgba,
    line: Rgba,
    chart_line: Rgba,
    tog_stroke: Rgba,
    tog_fill: Rgba,
    tog_fill_hover: Rgba,
    knob_off: Rgba,
    tog_dis: Rgba,
    knob_dis: Rgba,
    rail: Rgba,
    thumb: Rgba,
    thumb_rim: Rgba,
    swatch_rim: Rgba,
    sb: Rgba,
    sb_track: Rgba,
    good: Rgba,
    learning: Rgba,
    accent: Rgba,
    accent_hover: Rgba,
    accent_press: Rgba,
    on_accent: Rgba,
}

impl Pal {
    fn new(dark: bool, mica: bool, accent: Rgba) -> Pal {
        let lum = (accent.0 as u32 * 299 + accent.1 as u32 * 587 + accent.2 as u32 * 114) / 1000;
        let on_accent = if lum > 150 { hx(0x000000) } else { hx(0xFFFFFF) };
        let shade = if dark { hx(0x000000) } else { hx(0xFFFFFF) };
        let (accent_hover, accent_press) = (accent.mix(shade, 0.1), accent.mix(shade, 0.2));
        if dark {
            Pal {
                dark,
                bg: if mica { Rgba(0, 0, 0, 0) } else { hx(0x202020) },
                card: if mica { wa(0x0D) } else { hx(0x2B2B2B) },
                card_border: if mica { wa(0x0F) } else { ka(0x33) },
                bubble: hx(0x2C2C2C),
                pane: hx(0x2C2C2C),
                nav_sel: wa(0x0F),
                nav_hover: wa(0x0A),
                nav_press: wa(0x07),
                ctl: wa(0x0F),
                ctl_hover: wa(0x15),
                ctl_press: wa(0x08),
                ctl_dis: wa(0x0B),
                ctl_border: wa(0x12),
                subtle_hover: wa(0x0F),
                subtle_press: wa(0x08),
                fg: hx(0xFFFFFF),
                dim: hx(0xC5C5C5),
                off: hx(0x787878),
                line: wa(0x0F),
                chart_line: wa(0x17),
                tog_stroke: wa(0x9A),
                tog_fill: ka(0x19),
                tog_fill_hover: wa(0x0B),
                knob_off: wa(0xC8),
                tog_dis: wa(0x28),
                knob_dis: wa(0x5D),
                rail: wa(0x8B),
                thumb: hx(0x454545),
                thumb_rim: wa(0x14),
                swatch_rim: wa(0x40),
                sb: wa(0x8B),
                sb_track: wa(0x0B),
                good: hx(0x6CCB5F),
                learning: hx(0x9D9D9D),
                accent,
                accent_hover,
                accent_press,
                on_accent,
            }
        } else {
            let f9 = |a: u8| Rgba(0xF9, 0xF9, 0xF9, a);
            Pal {
                dark,
                bg: if mica { Rgba(0, 0, 0, 0) } else { hx(0xF3F3F3) },
                card: if mica { wa(0xB3) } else { hx(0xFBFBFB) },
                card_border: ka(0x0F),
                bubble: hx(0xFFFFFF),
                pane: hx(0xF9F9F9),
                nav_sel: ka(0x0A),
                nav_hover: ka(0x06),
                nav_press: ka(0x04),
                ctl: wa(0xB3),
                ctl_hover: f9(0x80),
                ctl_press: f9(0x4D),
                ctl_dis: f9(0x4D),
                ctl_border: ka(0x17),
                subtle_hover: ka(0x09),
                subtle_press: ka(0x06),
                fg: hx(0x1A1A1A),
                dim: hx(0x5D5D5D),
                off: hx(0xA0A0A0),
                line: ka(0x0F),
                chart_line: ka(0x14),
                tog_stroke: ka(0x72),
                tog_fill: ka(0x06),
                tog_fill_hover: ka(0x0F),
                knob_off: ka(0x9B),
                tog_dis: ka(0x37),
                knob_dis: wa(0xFF),
                rail: ka(0x72),
                thumb: hx(0xFFFFFF),
                thumb_rim: ka(0x0F),
                swatch_rim: ka(0x30),
                sb: ka(0x72),
                sb_track: ka(0x06),
                good: hx(0x0F7B0F),
                learning: hx(0x8A8A8A),
                accent,
                accent_hover,
                accent_press,
                on_accent,
            }
        }
    }

    /// The colours `insights::view` paints with.
    fn view(&self) -> view::Colors {
        view::Colors {
            dark: self.dark,
            card: self.card,
            card_border: self.card_border,
            bubble: self.bubble,
            fg: self.fg,
            dim: self.dim,
            off: self.off,
            line: self.chart_line,
            good: self.good,
            learning: self.learning,
            accent: self.accent,
            warn: C_WARN,
            crit: C_CRIT,
        }
    }
}

/// The Windows accent colour (lightened a little on dark backgrounds, as the
/// system's own controls do), with the stock blues as a fallback.
pub(crate) fn accent_color(dark: bool) -> Rgba {
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
/// Notepad++, ...) uses; they make the native popup menus (drop-down lists,
/// tray menu) dark. All of it is optional: when the functions are missing
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

    /// Windows build number (0 when unknown).
    pub fn build() -> u32 {
        api().build
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

/// Make the app's native menus (tray / right-click menu) follow the theme from
/// the very first time they open. Call at startup and when the theme changes.
pub fn apply_menu_theme(theme: Theme, owner: HWND) {
    darkmode::set_app_mode(theme);
    darkmode::allow_window(owner, true);
}


// ------------------------------------------------------------------ fonts

struct Fonts {
    body: HFONT,
    small: HFONT,
    tiny: HFONT,
    semi: HFONT,
    /// Navigation section headers.
    cap: HFONT,
    /// Insights headlines.
    head: HFONT,
    title: HFONT,
    icon: HFONT,
    icon_sm: HFONT,
}


/// `want` if the system has it, else `fallback`.
pub(crate) fn pick_face(want: &'static str, fallback: &'static str) -> &'static str {
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
    /// Grayscale anti-aliased fonts (the canvas needs a coverage mask).
    fn new(dpi: u32) -> Fonts {
        let sc = |v: i32| ((v * dpi as i32 + 48) / 96).max(1);
        let text = ui_face();
        let display = pick_face("Segoe UI Variable Display", text);
        let icons = pick_face("Segoe Fluent Icons", "Segoe MDL2 Assets");
        Fonts {
            body: create_font(sc(14), 400, text, true),
            small: create_font(sc(12), 400, text, true),
            tiny: create_font(sc(11), 400, text, true),
            semi: create_font(sc(14), 600, text, true),
            cap: create_font(sc(12), 600, text, true),
            head: create_font(sc(20), 600, display, true),
            title: create_font(sc(28), 600, display, true),
            icon: create_font(sc(16), 400, icons, true),
            icon_sm: create_font(sc(12), 400, icons, true),
        }
    }

    fn view(&self) -> view::Fonts {
        view::Fonts { body: self.body, small: self.small, tiny: self.tiny, semi: self.semi, head: self.head }
    }

    fn free(&self) {
        for f in [self.body, self.small, self.tiny, self.semi, self.cap, self.head, self.title, self.icon, self.icon_sm] {
            unsafe { DeleteObject(f) };
        }
    }
}

// ------------------------------------------------------------------ model

/// What a control is; drives painting, hit-testing and input.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Navigation pane item.
    Nav(Page),
    /// Overview card that opens an Insights page.
    Summary(Page),
    Toggle,
    Dropdown,
    Slider,
    Stepper,
    /// Text, accent (filled) style.
    Button(&'static str, bool),
    /// Text-only button.
    Link(&'static str),
    Swatch(Metric),
    /// Small icon button.
    Glyph(char),
    /// Icon button at full icon size (hamburger, back).
    Icon(char),
}

/// Sub-area of a control under the mouse.
#[derive(Clone, Copy, PartialEq)]
enum Part {
    Body,
    Minus,
    Plus,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Slider(i32),
    /// Scroll bar thumb; offset of the grab point from the thumb top.
    Thumb(i32),
}

/// Where a control lives (which coordinates its rect is in).
#[derive(Clone, Copy, PartialEq)]
enum Layer {
    /// Page content: x in client, y in content coordinates (scrolls).
    Page,
    /// Scrolling part of the navigation pane: y in menu coordinates.
    Menu,
    /// Client coordinates (hamburger, footer items, back button).
    Fixed,
}

struct Control {
    id: i32,
    kind: Kind,
    rect: Rect,
    layer: Layer,
    enabled: bool,
}

/// One slot on the right-hand side of a card (page definition).
enum Cell {
    /// `w` in DIP; 0 = sized to its content.
    Ctl { id: i32, kind: Kind, w: i32, enabled: bool },
    /// Unit text after a control: shown text, text whose width is reserved.
    Unit(&'static str, &'static str),
    /// Live value of a slider ("100 %").
    Value(i32),
}

enum Def {
    Heading(&'static str),
    Note(&'static str),
    Card { title: String, desc: &'static str, enabled: bool, cells: Vec<Cell> },
    /// Overview card summarising panel `i` (opens its page).
    Summary(usize),
    /// The full Insights panel `i` (headline, chart, items).
    Panel(usize),
}

/// Laid-out non-interactive parts of a page (content coordinates).
enum Block {
    Heading(Rect, &'static str),
    Note(Rect, &'static str),
    Card {
        rect: Rect,
        /// Right limit of the title / description.
        text_r: i32,
        title: String,
        desc: &'static str,
        enabled: bool,
        labels: Vec<(Rect, String)>,
    },
}

/// A row of the navigation pane, laid out (menu coordinates for the menu).
#[derive(Clone, Copy)]
enum NavRow {
    Header(Rect, &'static str),
    Separator(Rect),
}

fn card(title: &str, desc: &'static str, cells: Vec<Cell>) -> Def {
    Def::Card { title: title.to_string(), desc, enabled: true, cells }
}

fn cell(id: i32, kind: Kind, w: i32) -> Cell {
    Cell::Ctl { id, kind, w, enabled: true }
}

fn toggle_card(title: &str, desc: &'static str, id: i32) -> Def {
    card(title, desc, vec![cell(id, Kind::Toggle, 0)])
}

fn gated(mut d: Def, on: bool) -> Def {
    if let Def::Card { enabled, .. } = &mut d {
        *enabled = on;
    }
    d
}

/// (lo, hi, step, whether 0 means "off") of a number stepper.
fn num_spec(id: i32) -> (f64, f64, f64, bool) {
    match id {
        ID_A_CPU | ID_A_RAM | ID_A_DISK | ID_A_BAT => (0.0, 100.0, 1.0, true),
        ID_A_SECS => (5.0, 3600.0, 5.0, false),
        ID_WARN | ID_CRIT => (1.0, 100.0, 1.0, false),
        ID_CAP => (0.0, 1_000_000.0, 1.0, true),
        _ => (0.0, 100.0, 1.0, false),
    }
}

/// (lo, hi, keyboard step, drag step) of a slider, in displayed units.
fn slider_spec(id: i32) -> (i32, i32, i32, i32) {
    match id {
        ID_SCALE => (70, 200, 5, 5),
        _ => (0, 100, 5, 1), // opacity, in percent
    }
}

fn nav_id(p: Page) -> i32 {
    ID_NAV + p.index() as i32
}

/// Ease-out cubic of a 0..1 progress.
fn ease(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

/// Windows "Animate controls and elements inside windows" (Settings >
/// Accessibility > Visual effects > Animation effects).
fn animations_on() -> bool {
    let mut on: BOOL = 1;
    // SAFETY: valid out pointer of the documented size.
    unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, &mut on as *mut BOOL as _, 0) };
    on != 0
}

// ------------------------------------------------------------------ drawing helpers

fn inflate(r: Rect, d: i32) -> Rect {
    Rect { l: r.l - d, t: r.t - d, r: r.r + d, b: r.b + d }
}

fn shift(r: Rect, dy: i32) -> Rect {
    Rect { t: r.t + dy, b: r.b + dy, ..r }
}

/// Pill of size `w`×`h` centred on (cx, cy).
fn pill(cv: &mut Canvas, cx: i32, cy: i32, w: i32, h: i32, c: Rgba) {
    let r = Rect { l: cx - w / 2, t: cy - h / 2, r: cx - w / 2 + w, b: cy - h / 2 + h };
    cv.fill_round(r, h.min(w) as f32 / 2.0, c);
}

/// Two-pixel focus ring just inside `r`.
fn ring(cv: &mut Canvas, r: Rect, rad: f32, c: Rgba) {
    cv.stroke_round(r, rad, c);
    cv.stroke_round(inflate(r, -1), (rad - 1.0).max(0.0), c);
}

/// Text with its vertical centre at `cy`.
fn text_mid(cv: &mut Canvas, font: HFONT, s: &str, x: i32, cy: i32, c: Rgba) {
    let (_, th) = cv.measure(font, s);
    cv.text(font, s, x, cy - th / 2, c);
}

/// A glyph centred on (cx, cy).
fn glyph(cv: &mut Canvas, font: HFONT, g: char, cx: i32, cy: i32, c: Rgba) {
    let mut buf = [0u8; 4];
    let s = g.encode_utf8(&mut buf);
    let (w, h) = cv.measure(font, s);
    cv.text(font, s, cx - w / 2, cy - h / 2, c);
}

fn rgb_to_cr(rgb: u32) -> COLORREF {
    ((rgb >> 16) & 0xFF) | (rgb & 0xFF00) | ((rgb & 0xFF) << 16)
}

fn cr_to_rgb(cr: COLORREF) -> u32 {
    rgb_to_cr(cr) // the swap is its own inverse
}

fn invalidate(hwnd: HWND) {
    if !hwnd.is_null() {
        unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
    }
}

/// Make sure the order lists every metric exactly once.
fn full_order(order: &[Metric]) -> Vec<Metric> {
    let mut v: Vec<Metric> = Vec::new();
    for m in order.iter().chain(Metric::ALL.iter()) {
        if !v.contains(m) {
            v.push(*m);
        }
    }
    v
}

// ------------------------------------------------------------------ ui

/// Running transitions (start times); None = not running.
struct Anim {
    /// Page content fading / sliding in.
    page: Option<Instant>,
    /// Selection indicator sliding from this centre y (menu coordinates).
    ind: Option<(i32, Instant)>,
    /// Overlay pane width animating from .0 to .1.
    pane: Option<(i32, i32, Instant)>,
    /// The timer is running.
    timer: bool,
}

struct Ui {
    hwnd: HWND,
    dpi: u32,
    dark: bool,
    /// The live theme setting (drives the process-wide app mode).
    theme: Theme,
    /// The Mica backdrop is on: the background is painted transparent.
    mica: bool,
    pal: Pal,
    fonts: Fonts,
    /// Memory DC for measuring text during layout.
    mdc: HDC,
    cw: i32,
    ch: i32,
    page: Page,
    history: History,
    /// Pane display mode from the window width.
    mode: nav::Mode,
    /// Expanded mode, pane collapsed to icons with the hamburger.
    pane_closed: bool,
    /// Compact mode, the full pane is open over the content.
    overlay: bool,
    /// Scroll offset of the pane's menu (when it does not fit), px.
    nav_scroll: i32,
    nav_h: i32,
    rows: Vec<NavRow>,
    controls: Vec<Control>,
    blocks: Vec<Block>,
    content_h: i32,
    /// Current (animated) and target scroll offsets, px.
    scroll: f32,
    target: i32,
    hover: Option<(i32, Part)>,
    pressed: Option<(i32, Part)>,
    focus: Option<i32>,
    /// Show the focus ring (keyboard was used since the last click).
    focus_visible: bool,
    drag: Drag,
    sb_hover: bool,
    tracking: bool,
    /// Compact pane: tooltip of this item is showing (after the hover time).
    tip: Option<Page>,
    /// Auto-repeat ticks of the stepper button held down.
    repeat_n: u32,
    /// Drop-down whose menu is open (drawn pressed).
    open_dd: Option<i32>,
    cv: Canvas,
    anim: Anim,
    animate: bool,
    /// Last normal (not minimized / maximized) window rectangle, and maximized.
    normal: Rect,
    maximized: bool,
    /// The first WM_PAINT happened (logged once).
    painted: bool,
    // ---- Insights
    ins_on: bool,
    panels: Vec<Panel>,
    sig: u64,
    /// GetTickCount64 of the last rebuild (0 = stale).
    built: u64,
    /// Laid-out panel of the current page, and which one.
    pview: View,
    panel_idx: Option<usize>,
    /// Client x over the chart plot.
    chart_hover: Option<i32>,
    // ---- settings
    /// The copy being edited.
    draft: Settings,
    /// Adapter aliases offered in the drop-down (index + 1; 0 = automatic).
    adapters: Vec<String>,
    adapter_labels: Vec<String>,
    /// Taskbars present when the window opened: (settings key, label, primary).
    displays: Vec<(String, String, bool)>,
    autostart: bool,
    /// Something changed since the last apply.
    dirty: bool,
    /// Custom colours of the colour picker, kept for the session.
    cust: [COLORREF; 16],
}

impl Ui {
    #[allow(clippy::too_many_arguments)]
    fn new(
        dpi: u32,
        dark: bool,
        theme: Theme,
        draft: Settings,
        adapters: Vec<String>,
        labels: Vec<String>,
        displays: Vec<(String, String, bool)>,
        page: Page,
    ) -> Ui {
        Ui {
            hwnd: std::ptr::null_mut(),
            dpi,
            dark,
            theme,
            mica: false,
            pal: Pal::new(dark, false, accent_color(dark)),
            fonts: Fonts::new(dpi),
            mdc: unsafe { CreateCompatibleDC(std::ptr::null_mut()) },
            cw: 0,
            ch: 0,
            page,
            history: History::new(),
            mode: nav::Mode::Expanded,
            pane_closed: false,
            overlay: false,
            nav_scroll: 0,
            nav_h: 0,
            rows: Vec::new(),
            controls: Vec::new(),
            blocks: Vec::new(),
            content_h: 0,
            scroll: 0.0,
            target: 0,
            hover: None,
            pressed: None,
            focus: None,
            focus_visible: false,
            drag: Drag::None,
            sb_hover: false,
            tracking: false,
            tip: None,
            repeat_n: 0,
            open_dd: None,
            cv: Canvas::new(),
            anim: Anim { page: None, ind: None, pane: None, timer: false },
            animate: animations_on(),
            normal: Rect { l: 0, t: 0, r: 0, b: 0 },
            maximized: false,
            painted: false,
            ins_on: false,
            panels: Vec::new(),
            sig: 0,
            built: 0,
            pview: View::new(),
            panel_idx: None,
            chart_hover: None,
            draft,
            adapters,
            adapter_labels: labels,
            displays,
            autostart: crate::autostart::enabled(),
            dirty: false,
            cust: [0x00FF_FFFF; 16],
        }
    }

    fn sc(&self, v: i32) -> i32 {
        (v * self.dpi as i32 + 48) / 96
    }

    fn measure(&self, font: HFONT, s: &str) -> i32 {
        measure_dc(self.mdc, font, s).0
    }

    fn set_dpi(&mut self, dpi: u32) {
        let old = self.dpi.max(1);
        self.dpi = dpi;
        let f = std::mem::replace(&mut self.fonts, Fonts::new(dpi));
        f.free();
        self.target = self.target * dpi as i32 / old as i32;
        self.scroll = self.target as f32;
        self.nav_scroll = self.nav_scroll * dpi as i32 / old as i32;
    }

    /// Take fresh Insights data. Returns true when something visible changed.
    fn take_panels(&mut self, on: bool, panels: Vec<Panel>) -> bool {
        // SAFETY: no preconditions.
        self.built = unsafe { GetTickCount64() }.max(1);
        let sig = view::signature(on, &panels);
        if sig == self.sig {
            return false;
        }
        self.sig = sig;
        self.ins_on = on;
        self.panels = panels;
        true
    }

    // -------------------------------------------------------------- values

    fn metric_at(&self, pos: i32) -> Option<Metric> {
        (pos >= 0).then(|| self.draft.order.get(pos as usize).copied()).flatten()
    }

    fn flag(&self, id: i32) -> bool {
        let d = &self.draft;
        match id {
            ID_SPARK => d.sparklines,
            ID_LABELS => d.labels,
            ID_STACKED => d.stacked_for(self.displays.first().map_or("", |x| x.0.as_str())),
            ID_AUTOSTACK => d.auto_stack,
            ID_FULLSCREEN => d.hide_fullscreen,
            ID_CLICKTHRU => d.click_through,
            ID_AUTOUPDATE => d.auto_update,
            ID_ACHIEVE => d.achievements,
            ID_AUTOSTART => self.autostart,
            ID_ALERTS => d.alerts,
            ID_A_CAP => d.alert_cap,
            ID_WINACCENT => d.windows_accent,
            ID_INSIGHTS => d.insights,
            ID_INS_TOASTS => d.insight_toasts,
            ID_BAGGUARD => d.bag_guard,
            _ if (ID_DSHOW..ID_DSHOW + MAX_DISPLAYS).contains(&id) => {
                self.displays.get((id - ID_DSHOW) as usize).is_some_and(|(k, _, p)| d.wants_display(k, *p))
            }
            _ if (ID_DSTACK..ID_DSTACK + MAX_DISPLAYS).contains(&id) => {
                self.displays.get((id - ID_DSTACK) as usize).is_some_and(|(k, ..)| d.stacked_for(k))
            }
            _ => self.metric_at(id - ID_MTOG).is_some_and(|m| d.show[m.idx()]),
        }
    }

    fn flag_slot(&mut self, id: i32) -> Option<&mut bool> {
        let m = self.metric_at(id - ID_MTOG);
        let d = &mut self.draft;
        Some(match id {
            ID_SPARK => &mut d.sparklines,
            ID_LABELS => &mut d.labels,
            ID_AUTOSTACK => &mut d.auto_stack,
            ID_FULLSCREEN => &mut d.hide_fullscreen,
            ID_CLICKTHRU => &mut d.click_through,
            ID_AUTOUPDATE => &mut d.auto_update,
            ID_ACHIEVE => &mut d.achievements,
            ID_AUTOSTART => &mut self.autostart,
            ID_ALERTS => &mut d.alerts,
            ID_A_CAP => &mut d.alert_cap,
            ID_WINACCENT => &mut d.windows_accent,
            ID_INSIGHTS => &mut d.insights,
            ID_INS_TOASTS => &mut d.insight_toasts,
            ID_BAGGUARD => &mut d.bag_guard,
            _ => &mut d.show[m?.idx()],
        })
    }

    /// Flip a toggle switch. Returns true if the draft changed.
    fn toggle(&mut self, id: i32) -> bool {
        let on = !self.flag(id);
        let d = &mut self.draft;
        if id == ID_STACKED {
            // One display: this is the default for every display.
            d.stacked = on;
            d.stacked_on.clear();
        } else if (ID_DSTACK..ID_DSTACK + MAX_DISPLAYS).contains(&id) {
            let Some((k, ..)) = self.displays.get((id - ID_DSTACK) as usize) else { return false };
            d.set_stacked(k, on);
        } else if (ID_DSHOW..ID_DSHOW + MAX_DISPLAYS).contains(&id) {
            let Some((key, ..)) = self.displays.get((id - ID_DSHOW) as usize) else { return false };
            let mut keys: Vec<&str> =
                self.displays.iter().filter(|(k, _, p)| d.wants_display(k, *p)).map(|(k, ..)| k.as_str()).collect();
            if on {
                keys.push(key);
            } else {
                keys.retain(|k| k != key);
            }
            let all = self.displays.iter().all(|(k, ..)| keys.contains(&k.as_str()));
            let primary_only = keys.len() == 1 && self.displays.iter().any(|(k, _, p)| *p && k == keys[0]);
            d.displays = if keys.is_empty() || primary_only {
                "primary".into()
            } else if all {
                "all".into()
            } else {
                keys.join(",")
            };
        } else {
            match self.flag_slot(id) {
                Some(v) => *v = on,
                None => return false,
            }
        }
        true
    }

    fn num_get(&self, id: i32) -> f64 {
        let d = &self.draft;
        match id {
            ID_A_CPU => d.alert_cpu_pct as f64,
            ID_A_RAM => d.alert_ram_pct as f64,
            ID_A_SECS => d.alert_secs as f64,
            ID_A_DISK => d.alert_disk_free_pct as f64,
            ID_A_BAT => d.alert_battery_pct as f64,
            ID_WARN => d.warn_pct as f64,
            ID_CRIT => d.crit_pct as f64,
            ID_CAP => d.data_cap_gb,
            _ => 0.0,
        }
    }

    fn num_set(&mut self, id: i32, v: f64) {
        let (lo, hi, ..) = num_spec(id);
        let v = v.clamp(lo, hi);
        let d = &mut self.draft;
        match id {
            ID_A_CPU => d.alert_cpu_pct = v as f32,
            ID_A_RAM => d.alert_ram_pct = v as f32,
            ID_A_SECS => d.alert_secs = v.round() as u32,
            ID_A_DISK => d.alert_disk_free_pct = v as f32,
            ID_A_BAT => d.alert_battery_pct = v.round() as u32,
            ID_WARN => d.warn_pct = v as f32,
            ID_CRIT => d.crit_pct = v as f32,
            ID_CAP => d.data_cap_gb = v,
            _ => {}
        }
    }

    /// Step a number up or down to the next multiple of its step (so a
    /// hand-edited 87.5 goes to 88 / 87). Returns true if it changed.
    fn num_step(&mut self, id: i32, dir: i32, mult: f64) -> bool {
        let (lo, hi, step, _) = num_spec(id);
        let s = step * mult;
        let v = self.num_get(id);
        let n = if dir > 0 { ((v / s + 1e-9).floor() + 1.0) * s } else { ((v / s - 1e-9).ceil() - 1.0) * s };
        let n = n.clamp(lo, hi);
        if (n - v).abs() < 1e-9 {
            return false;
        }
        self.num_set(id, n);
        true
    }

    fn num_text(&self, id: i32) -> String {
        let v = self.num_get(id);
        let (.., zero_off) = num_spec(id);
        if zero_off && v == 0.0 {
            "Off".to_string()
        } else if (v - v.round()).abs() < 1e-6 {
            format!("{}", v.round() as i64)
        } else {
            format!("{:.1}", v)
        }
    }

    fn slider_get(&self, id: i32) -> i32 {
        match id {
            ID_SCALE => self.draft.text_scale as i32,
            _ => (self.draft.bg_opacity as i32 * 100 + 127) / 255,
        }
    }

    fn slider_set(&mut self, id: i32, v: i32) -> bool {
        let (lo, hi, ..) = slider_spec(id);
        let v = v.clamp(lo, hi);
        if v == self.slider_get(id) {
            return false;
        }
        match id {
            ID_SCALE => self.draft.text_scale = v as u32,
            _ => self.draft.bg_opacity = ((v * 255 + 50) / 100).min(255) as u8,
        }
        true
    }

    fn value_text(&self, id: i32) -> String {
        format!("{} %", self.slider_get(id))
    }

    /// Options of a drop-down and the index of the current one.
    fn dd_options(&self, id: i32) -> (Vec<String>, usize) {
        let d = &self.draft;
        match id {
            ID_INTERVAL => {
                let mut v: Vec<String> = INTERVALS.iter().map(|(t, _)| t.to_string()).collect();
                let cur = match INTERVALS.iter().position(|(_, ms)| *ms == d.interval_ms) {
                    Some(i) => i,
                    None => {
                        v.push(format!("{:.2} seconds", d.interval_ms as f32 / 1000.0));
                        v.len() - 1
                    }
                };
                (v, cur)
            }
            ID_UNIT => (
                SpeedUnit::ALL.iter().map(|u| u.label().to_string()).collect(),
                SpeedUnit::ALL.iter().position(|u| *u == d.unit).unwrap_or(0),
            ),
            ID_THEME => (
                THEMES.iter().map(|(t, _)| t.to_string()).collect(),
                THEMES.iter().position(|(_, t)| *t == d.theme).unwrap_or(0),
            ),
            ID_FONT => {
                let mut v: Vec<String> = FONTS.iter().map(|f| f.to_string()).collect();
                let cur = if d.font.is_empty() {
                    0
                } else {
                    match FONTS.iter().skip(1).position(|f| f.eq_ignore_ascii_case(&d.font)) {
                        Some(i) => i + 1,
                        None => {
                            v.push(d.font.clone());
                            v.len() - 1
                        }
                    }
                };
                (v, cur)
            }
            ID_ADAPTER => {
                let mut v = vec!["Automatic (all connected adapters)".to_string()];
                v.extend(self.adapter_labels.iter().cloned());
                let cur = if d.adapter.is_empty() {
                    0
                } else {
                    self.adapters.iter().position(|a| a.eq_ignore_ascii_case(&d.adapter)).map_or(0, |p| p + 1)
                };
                (v, cur)
            }
            ID_DAY => ((1..=28).map(|n: u32| n.to_string()).collect(), (d.cap_reset_day.clamp(1, 28) - 1) as usize),
            ID_MODE => (
                MODES.iter().map(|(t, _)| t.to_string()).collect(),
                MODES.iter().position(|(_, m)| *m == d.mode).unwrap_or(0),
            ),
            ID_MAXW => {
                let mut v: Vec<String> = MAX_WIDTHS.iter().map(|(t, _)| t.to_string()).collect();
                let cur = match MAX_WIDTHS.iter().position(|(_, p)| *p == d.max_width_pct) {
                    Some(i) => i,
                    None => {
                        v.push(format!("{}% of taskbar", d.max_width_pct));
                        v.len() - 1
                    }
                };
                (v, cur)
            }
            ID_SHOWON => {
                let mut v = vec!["Main taskbar only".to_string(), "All taskbars".to_string()];
                let cur = match d.displays.to_ascii_lowercase().as_str() {
                    "primary" => 0,
                    "all" => 1,
                    _ => {
                        v.push("Selected displays".to_string());
                        2
                    }
                };
                (v, cur)
            }
            _ => (Vec::new(), 0),
        }
    }

    /// Choose option `i` of a drop-down. Returns true if the draft changed.
    fn dd_select(&mut self, id: i32, i: usize) -> bool {
        if i == self.dd_options(id).1 {
            return false;
        }
        let d = &mut self.draft;
        match id {
            ID_INTERVAL => match INTERVALS.get(i) {
                Some((_, ms)) => d.interval_ms = (*ms).clamp(250, 10_000),
                None => return false,
            },
            ID_UNIT => match SpeedUnit::ALL.get(i) {
                Some(u) => d.unit = *u,
                None => return false,
            },
            ID_THEME => match THEMES.get(i) {
                Some((_, t)) => d.theme = *t,
                None => return false,
            },
            ID_FONT => match i {
                0 => d.font.clear(),
                _ => match FONTS.get(i) {
                    Some(f) => d.font = f.to_string(),
                    None => return false, // the custom face: already current
                },
            },
            ID_ADAPTER => match i {
                0 => d.adapter.clear(),
                _ => match self.adapters.get(i - 1) {
                    Some(a) => d.adapter = a.clone(),
                    None => return false,
                },
            },
            ID_DAY => d.cap_reset_day = (i as u32 + 1).clamp(1, 28),
            ID_MAXW => match MAX_WIDTHS.get(i) {
                Some((_, p)) => d.max_width_pct = *p,
                None => return false, // the custom width: already current
            },
            ID_SHOWON => match i {
                0 => d.displays = "primary".into(),
                1 => d.displays = "all".into(),
                _ => return false,
            },
            _ => return false,
        }
        true
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


    // -------------------------------------------------------------- pages

    fn number_card(&self, title: &str, desc: &'static str, id: i32, unit: &'static str) -> Def {
        let (.., zero_off) = num_spec(id);
        let shown = if zero_off && self.num_get(id) == 0.0 { "" } else { unit };
        card(title, desc, vec![cell(id, Kind::Stepper, STEPPER_W), Cell::Unit(shown, unit)])
    }

    /// Cards of a settings page (General .. Data) and About.
    fn settings_defs(&self, page: Page) -> Vec<Def> {
        let d = &self.draft;
        match page {
            Page::General => {
                let multi = self.displays.len() > 1;
                let mut v = vec![
                    card("Update interval", "How often the widget refreshes", vec![cell(ID_INTERVAL, Kind::Dropdown, 200)]),
                    card("Network speed unit", "Show speeds in bits or bytes per second", vec![cell(ID_UNIT, Kind::Dropdown, 200)]),
                    card(
                        "Text size",
                        "Scale of the text on the widget",
                        vec![cell(ID_SCALE, Kind::Slider, 180), Cell::Value(ID_SCALE)],
                    ),
                    card("Theme", "Colours of the widget and this window", vec![cell(ID_THEME, Kind::Dropdown, 200)]),
                    card(
                        "Background opacity",
                        "How solid the widget background is",
                        vec![cell(ID_OPACITY, Kind::Slider, 180), Cell::Value(ID_OPACITY)],
                    ),
                    card("Font", "Typeface used on the widget", vec![cell(ID_FONT, Kind::Dropdown, 200)]),
                    Def::Heading("Position"),
                    card("Placement", "Docked next to the tray, or floating anywhere", vec![cell(ID_MODE, Kind::Dropdown, 220)]),
                    card("Max width", "How much of the taskbar the widget may use", vec![cell(ID_MAXW, Kind::Dropdown, 220)]),
                    card(
                        "Reset position",
                        "Move the widget back to its starting place",
                        vec![cell(ID_RESETPOS, Kind::Button("Reset", false), BTN_W)],
                    ),
                    card(
                        "Taskbar icons",
                        "Measure the taskbar again if the widget covers app icons",
                        vec![cell(ID_RECHECK, Kind::Button("Re-check", false), BTN_W)],
                    ),
                    toggle_card("Hide in full-screen apps", "Keep the widget out of the way of games and videos", ID_FULLSCREEN),
                    toggle_card("Click-through", "Mouse clicks pass through the widget to the taskbar", ID_CLICKTHRU),
                ];
                if multi {
                    v.push(Def::Heading("Displays"));
                    v.push(card("Show the widget on", "Needs \u{201C}Show my taskbar on all displays\u{201D}", vec![cell(ID_SHOWON, Kind::Dropdown, 220)]));
                    for (i, (_, label, _)) in self.displays.iter().enumerate() {
                        v.push(card(label, "Show the widget on this taskbar", vec![cell(ID_DSHOW + i as i32, Kind::Toggle, 0)]));
                    }
                    for (i, (key, ..)) in self.displays.iter().enumerate() {
                        let label = self.displays[i].1.split("  (").next().unwrap_or(key);
                        v.push(card(
                            &format!("Stacked rows on {}", label),
                            "Two metrics per column on this taskbar",
                            vec![cell(ID_DSTACK + i as i32, Kind::Toggle, 0)],
                        ));
                    }
                }
                v.push(Def::Heading("Behaviour"));
                v.push(toggle_card("Sparklines", "Draw a small history graph behind each value", ID_SPARK));
                v.push(toggle_card("Metric labels", "Show the name next to each value", ID_LABELS));
                if !multi {
                    v.push(toggle_card("Stacked rows", "Two metrics per column on the taskbar", ID_STACKED));
                }
                v.extend([
                    toggle_card("Auto-stack when crowded", "Switch to stacked rows when the widget would not fit", ID_AUTOSTACK),
                    toggle_card("Update automatically", "Install new versions when they are released", ID_AUTOUPDATE),
                    toggle_card("Start with Windows", "Launch Resource Monitor when you sign in", ID_AUTOSTART),
                    toggle_card("Achievements", "Earn medals for real use. Nothing leaves this PC", ID_ACHIEVE),
                ]);
                v
            }
            Page::Metrics => {
                let mut v = vec![Def::Note("Choose which metrics appear on the widget, and move them to change their order.")];
                let n = d.order.len();
                for (pos, m) in d.order.iter().enumerate() {
                    let p = pos as i32;
                    v.push(card(
                        m.menu_label(),
                        "",
                        vec![
                            Cell::Ctl { id: ID_MUP + p, kind: Kind::Glyph(G_CHEVRON_UP), w: 32, enabled: pos > 0 },
                            Cell::Ctl { id: ID_MDN + p, kind: Kind::Glyph(G_CHEVRON_DOWN), w: 32, enabled: pos + 1 < n },
                            cell(ID_MTOG + p, Kind::Toggle, 0),
                        ],
                    ));
                }
                v
            }
            Page::Colours => {
                let mut v = vec![
                    toggle_card(
                        "Use Windows accent colour",
                        "Metrics without a custom colour follow your Windows accent colour",
                        ID_WINACCENT,
                    ),
                    card(
                        "Custom colours",
                        "Remove every custom colour",
                        vec![Cell::Ctl { id: ID_RESETALL, kind: Kind::Button("Reset all colours", false), w: 0, enabled: !d.colors.is_empty() }],
                    ),
                    Def::Heading("Metric colours"),
                ];
                for (i, m) in Metric::ALL.iter().enumerate() {
                    let custom = d.colors.iter().any(|(x, _)| x == m);
                    v.push(card(
                        m.menu_label(),
                        "",
                        vec![
                            Cell::Ctl { id: ID_CRESET + i as i32, kind: Kind::Link("Reset"), w: 0, enabled: custom },
                            cell(ID_SWATCH + i as i32, Kind::Swatch(*m), 48),
                        ],
                    ));
                }
                v
            }
            Page::Alerts => {
                let on = d.alerts;
                vec![
                    toggle_card("Alert notifications", "Show a tray notification when a limit is crossed", ID_ALERTS),
                    Def::Heading("Limits"),
                    gated(self.number_card("CPU usage above", "Percent of total CPU. 0 turns this alert off", ID_A_CPU, "%"), on),
                    gated(self.number_card("Memory usage above", "Percent of installed memory. 0 turns this alert off", ID_A_RAM, "%"), on),
                    gated(self.number_card("For at least", "How long CPU or memory must stay above its limit", ID_A_SECS, "seconds"), on),
                    gated(self.number_card("System drive free space below", "0 turns this alert off", ID_A_DISK, "%"), on),
                    gated(self.number_card("Battery at or below", "Only while not charging. 0 turns this alert off", ID_A_BAT, "%"), on),
                    gated(toggle_card("Data cap alerts", "Alert at 90% and 100% of the monthly data cap", ID_A_CAP), on),
                    Def::Heading("Colour thresholds"),
                    self.number_card("Amber from", "Values turn amber from this percentage", ID_WARN, "%"),
                    self.number_card("Red from", "Values turn red from this percentage", ID_CRIT, "%"),
                    Def::Heading("Insights"),
                    gated(toggle_card("Insight notifications", "Sleep verdict on wake, crashes and driver resets", ID_INS_TOASTS), d.insights),
                ]
            }
            Page::Data => vec![
                self.number_card("Monthly data cap", "In gigabytes. 0 turns the cap off", ID_CAP, "GB"),
                card("Billing cycle resets on day", "Data is counted since the last reset day", vec![cell(ID_DAY, Kind::Dropdown, 120)]),
                card("Network adapter", "Which adapter the data counters follow", vec![cell(ID_ADAPTER, Kind::Dropdown, 250)]),
                card(
                    "Data usage report",
                    "Daily and monthly totals",
                    vec![cell(ID_REPORT, Kind::Button("Open data usage report", false), 0)],
                ),
            ],
            _ => vec![
                Def::Note(concat!("Resource Monitor v", env!("CARGO_PKG_VERSION"), " by Emulsion Labs.")),
                card("Emulsion Labs", "Visit the website", vec![cell(ID_WEBSITE, Kind::Button("Open website", false), 0)]),
                card(
                    "Updates",
                    concat!("Version ", env!("CARGO_PKG_VERSION"), ". Look for a newer one now"),
                    vec![cell(ID_UPDATES, Kind::Button("Check for updates", false), 0)],
                ),
                card("Settings file", "Edit settings.ini in Notepad", vec![cell(ID_EDITFILE, Kind::Button("Edit", false), BTN_W)]),
                card("Data folder", "Settings, usage history and logs", vec![cell(ID_DATADIR, Kind::Button("Open", false), BTN_W)]),
            ],
        }
    }


    fn page_defs(&self, page: Page) -> Vec<Def> {
        match page {
            Page::Overview => self.overview_defs(),
            Page::Achievements => self.achievements_defs(),
            p if p.panel().is_some() => self.insight_defs(p),
            p => self.settings_defs(p),
        }
    }

    /// Cards at the very top of Overview, above the Insights switch. Empty for
    /// now; the place for a future "next goal" (achievements) card.
    fn overview_top(&self) -> Vec<Def> {
        Vec::new()
    }

    fn overview_defs(&self) -> Vec<Def> {
        let mut v = self.overview_top();
        v.push(toggle_card("Insights", "Explain spikes, sleep, throttling and memory. Nothing leaves this PC", ID_INSIGHTS));
        v.push(Def::Heading("This PC"));
        for i in 0..5 {
            v.push(Def::Summary(i));
        }
        v.push(Def::Heading("History"));
        v.push(card(
            "Clear history",
            "Forget every incident, night and daily baseline",
            vec![Cell::Ctl { id: ID_INS_CLEAR, kind: Kind::Button("Clear", false), w: BTN_W, enabled: self.draft.insights }],
        ));
        v
    }

    /// Placeholder until the Achievements view moves into this window: a line
    /// and a button that opens the Achievements window.
    fn achievements_defs(&self) -> Vec<Def> {
        let on = self.draft.achievements;
        let mut v = vec![
            Def::Note("Medals, XP and levels for real use of this PC. Nothing leaves this PC."),
            card(
                "Achievements",
                "Your medals, level and what to aim for next",
                vec![Cell::Ctl { id: ID_ACH_OPEN, kind: Kind::Button("Open Achievements", true), w: 0, enabled: on }],
            ),
        ];
        if !on {
            v.push(toggle_card("Track achievements", "Achievements are off. Turn them on to earn medals", ID_ACHIEVE));
        }
        v
    }

    fn insight_defs(&self, page: Page) -> Vec<Def> {
        let mut v = Vec::new();
        if page == Page::Sleep {
            v.push(gated(
                toggle_card("Bag Guard", "Hibernate a lid-shut laptop that keeps draining on battery", ID_BAGGUARD),
                self.draft.insights,
            ));
        }
        match page.panel() {
            Some(i) if self.ins_on && i < self.panels.len() => {
                if !v.is_empty() {
                    v.push(Def::Note(""));
                }
                v.push(Def::Panel(i));
            }
            _ => v.push(toggle_card("Insights is off", "Turn it on to explain spikes, sleep, throttling and memory", ID_INSIGHTS)),
        }
        v
    }

    // -------------------------------------------------------------- geometry

    /// The pane shows labels (expanded, or the compact overlay is open).
    fn labels(&self) -> bool {
        (self.mode == nav::Mode::Expanded && !self.pane_closed) || self.overlay
    }

    /// Width the pane's items are laid out for.
    fn pane_w(&self) -> i32 {
        if self.labels() {
            self.sc(PANE_W)
        } else {
            self.sc(PANE_COMPACT)
        }
    }

    /// Left edge of the content column (the overlay does not move it).
    fn content_x(&self) -> i32 {
        if self.mode == nav::Mode::Expanded && !self.pane_closed {
            self.sc(PANE_W)
        } else {
            self.sc(PANE_COMPACT)
        }
    }

    /// Left and right edges of the cards (max width, left-aligned).
    fn col(&self) -> (i32, i32) {
        let l = self.content_x() + self.sc(PAGE_PAD);
        let r = (self.cw - self.sc(PAGE_PAD)).min(l + self.sc(CONTENT_MAX)).max(l + self.sc(240));
        (l, r)
    }

    fn view(&self) -> (i32, i32) {
        let t = self.sc(HEADER_H);
        (t, self.ch.max(t + 1))
    }

    fn max_scroll(&self) -> i32 {
        let (vt, vb) = self.view();
        (self.content_h - (vb - vt)).max(0)
    }

    fn scroll_px(&self) -> i32 {
        self.scroll.round() as i32
    }

    /// Height of the footer slot above About for a level / XP chip (0 = none).
    /// Reserved for the in-window Achievements work; nothing is shown yet.
    fn footer_chip_h(&self) -> i32 {
        0
    }

    /// Top of the pane footer (About, the chip slot).
    fn footer_top(&self) -> i32 {
        let n = nav::FOOTER.len() as i32;
        self.ch - self.sc(NAV_INSET) - n * self.sc(NAV_ITEM_H) - (n - 1) * self.sc(NAV_GAP) - self.footer_chip_h()
    }

    /// The scrolling menu band of the pane (client y).
    fn menu_band(&self) -> (i32, i32) {
        let t = self.sc(MENU_TOP);
        (t, (self.footer_top() - self.sc(NAV_SEP_H)).max(t + 1))
    }

    fn nav_max_scroll(&self) -> i32 {
        let (t, b) = self.menu_band();
        (self.nav_h - (b - t)).max(0)
    }

    fn ctl_width(&self, kind: Kind, w: i32) -> i32 {
        match kind {
            Kind::Toggle => {
                let lw = self.measure(self.fonts.body, "On").max(self.measure(self.fonts.body, "Off"));
                self.sc(40 + 12) + lw
            }
            Kind::Button(t, _) if w == 0 => (self.measure(self.fonts.body, t) + self.sc(32)).max(self.sc(120)),
            Kind::Link(t) if w == 0 => self.measure(self.fonts.body, t) + self.sc(16),
            _ => self.sc(w),
        }
    }

    /// Lay out the pane, the header and the current page from the draft.
    fn layout(&mut self) {
        let mut controls = Vec::new();
        let mut blocks = Vec::new();
        let mut rows = Vec::new();

        // Pane: hamburger, menu (headers, separators, items), footer.
        let pw = self.pane_w();
        let labels = self.labels();
        let (ih, gap, ins) = (self.sc(NAV_ITEM_H), self.sc(NAV_GAP), self.sc(NAV_INSET));
        let item = |t: i32| Rect { l: ins, t, r: pw - ins, b: t + ih };
        let hb = Rect { l: ins, t: ins, r: ins + self.sc(40), b: ins + ih };
        controls.push(Control { id: ID_HAMBURGER, kind: Kind::Icon(G_HAMBURGER), rect: hb, layer: Layer::Fixed, enabled: true });
        let mut y = 0;
        for e in nav::MENU {
            match e {
                Entry::Item(p) => {
                    controls.push(Control { id: nav_id(p), kind: Kind::Nav(p), rect: item(y), layer: Layer::Menu, enabled: true });
                    y += ih + gap;
                }
                // Headers hide in compact mode; separators stay.
                Entry::Header(t) if labels => {
                    let h = self.sc(NAV_HEAD_H);
                    rows.push(NavRow::Header(Rect { l: ins, t: y, r: pw - ins, b: y + h }, t));
                    y += h;
                }
                Entry::Header(_) => {}
                Entry::Separator => {
                    let h = self.sc(NAV_SEP_H);
                    rows.push(NavRow::Separator(Rect { l: ins + self.sc(8), t: y, r: pw - ins - self.sc(8), b: y + h }));
                    y += h;
                }
            }
        }
        self.nav_h = y;
        let mut t = self.footer_top() + self.footer_chip_h();
        for p in nav::FOOTER {
            controls.push(Control { id: nav_id(p), kind: Kind::Nav(p), rect: item(t), layer: Layer::Fixed, enabled: true });
            t += ih + gap;
        }
        self.rows = rows;
        self.nav_scroll = self.nav_scroll.clamp(0, self.nav_max_scroll());

        // Header: back button.
        let (l, r) = self.col();
        let bs = self.sc(32);
        let by = (self.sc(HEADER_H) - bs) / 2 + self.sc(2);
        let back = Rect { l, t: by, r: l + bs, b: by + bs };
        let can_back = self.history.can_go_back();
        controls.push(Control { id: ID_BACK, kind: Kind::Icon(G_BACK), rect: back, layer: Layer::Fixed, enabled: can_back });

        // Page.
        let (row_h, ctl_h) = (self.sc(ROW_H), self.sc(CTL_H));
        let mut y = 0;
        self.panel_idx = None;
        self.pview.clear();
        for (n, d) in self.page_defs(self.page).into_iter().enumerate() {
            match d {
                Def::Heading(t) => {
                    if n > 0 {
                        y += self.sc(HEAD_TOP) - self.sc(ROW_GAP);
                    }
                    let h = self.sc(HEAD_H);
                    blocks.push(Block::Heading(Rect { l, t: y, r, b: y + h }, t));
                    y += h;
                }
                Def::Note(t) => {
                    let h = if t.is_empty() { self.sc(12) } else { self.sc(NOTE_H) };
                    blocks.push(Block::Note(Rect { l, t: y, r, b: y + h }, t));
                    y += h;
                }
                Def::Summary(i) => {
                    let Some(pg) = Page::of_panel(i) else { continue };
                    let rect = Rect { l, t: y, r, b: y + self.sc(SUMMARY_H) };
                    controls.push(Control { id: ID_SUMMARY + i as i32, kind: Kind::Summary(pg), rect, layer: Layer::Page, enabled: true });
                    y = rect.b + self.sc(ROW_GAP);
                }
                Def::Panel(i) => {
                    if let Some(p) = self.panels.get(i) {
                        let f = self.fonts.view();
                        y = self.pview.layout(p, self.mdc, &f, self.dpi, l, r, y, unix_ms());
                        self.panel_idx = Some(i);
                    }
                }
                Def::Card { title, desc, enabled, cells } => {
                    let rect = Rect { l, t: y, r, b: y + row_h };
                    let cy = y + row_h / 2;
                    let (ct, cb) = (cy - ctl_h / 2, cy - ctl_h / 2 + ctl_h);
                    let mut x = r - self.sc(CARD_PAD);
                    let mut labels = Vec::new();
                    let mut row = Vec::new();
                    for c in cells.iter().rev() {
                        let w = match c {
                            Cell::Ctl { id, kind, w, enabled: e } => {
                                let w = self.ctl_width(*kind, *w);
                                let rc = Rect { l: x - w, t: ct, r: x, b: cb };
                                row.push(Control { id: *id, kind: *kind, rect: rc, layer: Layer::Page, enabled: enabled && *e });
                                w
                            }
                            Cell::Unit(shown, reserve) => {
                                let w = self.measure(self.fonts.body, reserve);
                                labels.push((Rect { l: x - w, t: ct, r: x, b: cb }, shown.to_string()));
                                w
                            }
                            Cell::Value(id) => {
                                let s = self.value_text(*id);
                                let w = self.measure(self.fonts.body, "100 %").max(self.measure(self.fonts.body, &s));
                                labels.push((Rect { l: x - w, t: ct, r: x, b: cb }, s));
                                w
                            }
                        };
                        x -= w + self.sc(CELL_GAP);
                    }
                    row.reverse(); // tab order: left to right
                    controls.extend(row);
                    blocks.push(Block::Card { rect, text_r: x, title, desc, enabled, labels });
                    y += row_h + self.sc(ROW_GAP);
                }
            }
        }
        self.content_h = y + self.sc(24);
        self.controls = controls;
        self.blocks = blocks;

        let max = self.max_scroll();
        self.target = self.target.clamp(0, max);
        self.scroll = self.scroll.clamp(0.0, max as f32);
        if let Some(f) = self.focus {
            if !self.controls.iter().any(|c| c.id == f && c.enabled) {
                self.focus = None;
            }
        }
    }

    fn control(&self, id: i32) -> Option<&Control> {
        self.controls.iter().find(|c| c.id == id)
    }

    /// A control's rectangle in client coordinates.
    fn client_rect(&self, c: &Control) -> Rect {
        match c.layer {
            Layer::Page => shift(c.rect, self.view().0 - self.scroll_px()),
            Layer::Menu => shift(c.rect, self.menu_band().0 - self.nav_scroll),
            Layer::Fixed => c.rect,
        }
    }

    fn stepper_parts(&self, r: Rect) -> (Rect, Rect) {
        let (bw, pad) = (self.sc(28), self.sc(4));
        let plus = Rect { l: r.r - pad - bw, t: r.t + pad, r: r.r - pad, b: r.b - pad };
        let minus = Rect { l: plus.l - self.sc(2) - bw, r: plus.l - self.sc(2), ..plus };
        (minus, plus)
    }

    fn toggle_track(&self, r: Rect) -> Rect {
        let cy = (r.t + r.b) / 2;
        Rect { l: r.r - self.sc(40), t: cy - self.sc(10), r: r.r, b: cy - self.sc(10) + self.sc(20) }
    }

    /// Ends of the slider's travel (thumb centre positions).
    fn slider_span(&self, r: Rect) -> (i32, i32) {
        (r.l + self.sc(10), r.r - self.sc(10))
    }

    /// The point is over the pane (the full pane while the overlay is open).
    fn over_pane(&self, x: i32) -> bool {
        x < if self.overlay { self.pane_w() } else { self.content_x() }
    }

    fn hit(&self, x: i32, y: i32) -> Option<(i32, Part)> {
        let (vt, vb) = self.view();
        let (mt, mb) = self.menu_band();
        let pane = self.over_pane(x);
        for c in &self.controls {
            if !c.enabled {
                continue;
            }
            let visible = match c.layer {
                Layer::Page => !pane && y >= vt && y < vb,
                Layer::Menu => pane && y >= mt && y < mb,
                Layer::Fixed => pane == matches!(c.kind, Kind::Nav(_) | Kind::Icon(G_HAMBURGER)),
            };
            if !visible {
                continue;
            }
            let r = self.client_rect(c);
            if !r.contains(x, y) {
                continue;
            }
            let part = if c.kind == Kind::Stepper {
                let (m, p) = self.stepper_parts(r);
                if m.contains(x, y) {
                    Part::Minus
                } else if p.contains(x, y) {
                    Part::Plus
                } else {
                    Part::Body
                }
            } else {
                Part::Body
            };
            return Some((c.id, part));
        }
        None
    }

    /// Client x when (x, y) is over the chart plot of the current panel.
    fn chart_at(&self, x: i32, y: i32) -> Option<i32> {
        let (vt, vb) = self.view();
        if self.panel_idx.is_none() || self.over_pane(x) || y < vt || y >= vb {
            return None;
        }
        let cy = y - (vt - self.scroll_px());
        self.pview.over_chart(x, cy).then_some(x)
    }

    /// Scroll bar track (hit area) and thumb, if the page scrolls.
    fn scrollbar(&self) -> Option<(Rect, Rect)> {
        let max = self.max_scroll();
        if max <= 0 {
            return None;
        }
        let (vt, vb) = self.view();
        let track = Rect { l: self.cw - self.sc(16), t: vt + self.sc(4), r: self.cw - self.sc(2), b: vb - self.sc(4) };
        let th = (track.h() * (vb - vt) / self.content_h.max(1)).max(self.sc(32)).min(track.h());
        let ty = track.t + ((track.h() - th) as f32 * (self.scroll / max as f32)).round() as i32;
        let wide = self.sb_hover || matches!(self.drag, Drag::Thumb(_));
        let w = if wide { self.sc(6) } else { self.sc(4) };
        let r = track.r - self.sc(2);
        Some((track, Rect { l: r - w, t: ty, r, b: ty + th }))
    }

    // -------------------------------------------------------------- motion

    /// Keep the animation timer running while anything moves.
    fn kick(&mut self) {
        if !self.anim.timer {
            self.anim.timer = true;
            unsafe { SetTimer(self.hwnd, T_ANIM, 15, None) };
        }
    }

    fn stop_timer(&mut self) {
        self.anim.timer = false;
        unsafe { KillTimer(self.hwnd, T_ANIM) };
    }

    /// One animation frame. Stops the timer when nothing moves any more.
    fn anim_tick(&mut self) {
        let mut busy = false;
        let d = self.target as f32 - self.scroll;
        if d.abs() < 0.6 {
            self.scroll = self.target as f32;
        } else {
            self.scroll += d * 0.3;
            busy = true;
        }
        let done = |t: Option<Instant>, dur: f32| t.is_none_or(|t| t.elapsed().as_secs_f32() >= dur);
        if done(self.anim.page, PAGE_ANIM_S) {
            self.anim.page = None;
        } else {
            busy = true;
        }
        if done(self.anim.ind.map(|x| x.1), IND_ANIM_S) {
            self.anim.ind = None;
        } else {
            busy = true;
        }
        if done(self.anim.pane.map(|x| x.2), PANE_ANIM_S) {
            self.anim.pane = None;
        } else {
            busy = true;
        }
        if !busy {
            self.stop_timer();
        }
        self.refresh_hover();
        invalidate(self.hwnd);
    }

    fn progress(t: Instant, dur: f32) -> f32 {
        ease(t.elapsed().as_secs_f32() / dur)
    }

    // -------------------------------------------------------------- scrolling

    fn scroll_to(&mut self, target: i32, animate: bool) {
        self.target = target.clamp(0, self.max_scroll());
        if animate && self.animate && (self.target as f32 - self.scroll).abs() > 1.0 {
            self.kick();
        } else {
            self.scroll = self.target as f32;
        }
        invalidate(self.hwnd);
    }

    /// Scroll so a control (plus a margin) is fully visible: the page for
    /// page controls, the pane's menu for menu items.
    fn ensure_visible(&mut self, id: i32) {
        let Some(c) = self.control(id) else { return };
        let m = self.sc(16);
        match c.layer {
            Layer::Page => {
                let (vt, vb) = self.view();
                let (t, b) = (c.rect.t - m, c.rect.b + m);
                let s = self.target;
                if t < s {
                    self.scroll_to(t, true);
                } else if b > s + (vb - vt) {
                    self.scroll_to(b - (vb - vt), true);
                }
            }
            Layer::Menu => {
                let (mt, mb) = self.menu_band();
                let (t, b) = (c.rect.t - self.sc(4), c.rect.b + self.sc(4));
                if t < self.nav_scroll {
                    self.nav_scroll = t.max(0);
                } else if b > self.nav_scroll + (mb - mt) {
                    self.nav_scroll = (b - (mb - mt)).min(self.nav_max_scroll());
                }
                invalidate(self.hwnd);
            }
            Layer::Fixed => {}
        }
    }

    /// Hover from the current cursor position (after content moved under it).
    fn refresh_hover(&mut self) {
        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut pt);
            ScreenToClient(self.hwnd, &mut pt);
        }
        let inside = pt.x >= 0 && pt.y >= 0 && pt.x < self.cw && pt.y < self.ch;
        let free = inside && self.drag == Drag::None;
        self.hover = if free { self.hit(pt.x, pt.y) } else { None };
        self.chart_hover = if free { self.chart_at(pt.x, pt.y) } else { None };
    }

    // -------------------------------------------------------------- navigation

    /// Client y of the centre of `p`'s navigation item, if it is on screen.
    fn indicator_y(&self, p: Page) -> Option<i32> {
        let c = self.control(nav_id(p))?;
        let r = self.client_rect(c);
        let (mt, mb) = self.menu_band();
        let cy = (r.t + r.b) / 2;
        (c.layer != Layer::Menu || (cy >= mt && cy < mb)).then_some(cy)
    }

    /// Show page `p`. `record` = remember the current page for Back.
    /// Insights data is loaded by the caller (it needs the `App`).
    fn navigate(&mut self, p: Page, record: bool) -> bool {
        self.overlay = false;
        self.tip = None;
        if p == self.page {
            self.layout();
            invalidate(self.hwnd);
            return false;
        }
        let from = self.indicator_y(self.page);
        if record {
            self.history.push(self.page);
        }
        self.page = p;
        self.scroll = 0.0;
        self.target = 0;
        self.chart_hover = None;
        if let Some(f) = self.focus {
            if self.control(f).is_some_and(|c| c.layer == Layer::Page) {
                self.focus = None;
            }
        }
        self.layout();
        self.refresh_hover();
        if self.animate && !self.hwnd.is_null() {
            let now = Instant::now();
            self.anim.page = Some(now);
            self.anim.ind = from.map(|y| (y, now));
            self.kick();
        }
        invalidate(self.hwnd);
        true
    }

    fn back(&mut self) -> bool {
        match self.history.pop() {
            Some(p) => self.navigate(p, false),
            None => false,
        }
    }

    /// Hamburger: collapse / expand the pane (expanded mode) or open / close
    /// the overlay pane (compact mode).
    fn toggle_pane(&mut self) {
        self.tip = None;
        if self.mode == nav::Mode::Expanded {
            self.pane_closed = !self.pane_closed;
        } else {
            self.overlay = !self.overlay;
            if self.overlay && self.animate {
                self.anim.pane = Some((self.sc(PANE_COMPACT), self.sc(PANE_W), Instant::now()));
                self.kick();
            }
        }
        self.layout();
        self.refresh_hover();
        invalidate(self.hwnd);
    }

    /// New client size: pick the pane mode for the width.
    fn resized(&mut self, w: i32, h: i32) {
        self.cw = w;
        self.ch = h;
        let epx = w * 96 / self.dpi.max(1) as i32;
        let mode = nav::mode_for(epx);
        if mode != self.mode {
            self.mode = mode;
            self.overlay = false;
            self.pane_closed = false;
        }
        self.layout();
    }

    /// Ids a keyboard user walks with Up / Down in the pane.
    fn nav_ids(&self) -> Vec<i32> {
        std::iter::once(ID_HAMBURGER).chain(nav::nav_order().map(nav_id)).collect()
    }

    fn in_nav(&self, id: i32) -> bool {
        id == ID_HAMBURGER || self.control(id).is_some_and(|c| matches!(c.kind, Kind::Nav(_)))
    }

    /// Controls in Tab order: hamburger, the selected navigation item, back,
    /// the page.
    fn focus_order(&self) -> Vec<i32> {
        let sel = nav_id(self.page);
        let mut v = vec![ID_HAMBURGER, sel];
        if self.control(ID_BACK).is_some_and(|c| c.enabled) {
            v.push(ID_BACK);
        }
        v.extend(self.controls.iter().filter(|c| c.enabled && c.layer == Layer::Page).map(|c| c.id));
        v
    }

    fn set_focus(&mut self, id: Option<i32>) {
        self.focus = id;
        self.focus_visible = true;
        if let Some(id) = id {
            self.ensure_visible(id);
        }
        invalidate(self.hwnd);
    }

    fn move_focus(&mut self, dir: i32) {
        let order = self.focus_order();
        let n = order.len() as i32;
        // A non-selected pane item (reached with the arrows) counts as the selected one.
        let cur = self.focus.map(|f| if self.in_nav(f) && f != ID_HAMBURGER { nav_id(self.page) } else { f });
        let next = match cur.and_then(|f| order.iter().position(|x| *x == f)) {
            Some(i) => (i as i32 + dir).rem_euclid(n),
            None if dir > 0 => 0,
            None => n - 1,
        };
        self.set_focus(Some(order[next as usize]));
    }

    /// F6: between the pane and the content.
    fn switch_region(&mut self) {
        if self.focus.is_some_and(|f| self.in_nav(f)) {
            let first = self.controls.iter().find(|c| c.enabled && c.layer == Layer::Page).map(|c| c.id);
            self.set_focus(first.or(self.control(ID_BACK).filter(|c| c.enabled).map(|c| c.id)));
        } else {
            self.set_focus(Some(nav_id(self.page)));
        }
    }

    // -------------------------------------------------------------- painting

    fn paint(&mut self, dc: HDC) {
        let mut cv = std::mem::replace(&mut self.cv, Canvas::new());
        cv.resize(self.cw, self.ch);
        self.draw(&mut cv);
        cv.set_clip(None);
        cv.blit(dc, 0, 0);
        self.cv = cv;
    }

    fn state(&self, id: i32, part: Part) -> (bool, bool) {
        let hover = self.hover == Some((id, part));
        let pressed = hover && self.pressed == Some((id, part)) || self.open_dd == Some(id);
        (hover || self.open_dd == Some(id), pressed)
    }

    fn draw(&self, cv: &mut Canvas) {
        let p = self.pal;
        let f = &self.fonts;
        cv.clear(p.bg);
        let (vt, vb) = self.view();
        let cx0 = self.content_x();
        let (_, col_r) = self.col();
        let rad = self.sc(6) as f32;
        // Page transition: content slides up `PAGE_SLIDE` while it fades in.
        let fade = self.anim.page.map(|t| Self::progress(t, PAGE_ANIM_S));
        let slide = fade.map_or(0, |e| ((1.0 - e) * self.sc(PAGE_SLIDE) as f32).round() as i32);
        let dy = vt - self.scroll_px() + slide;

        // Page content, clipped to the band under the header.
        cv.set_clip(Some(Rect { l: cx0, t: vt, r: self.cw, b: vb }));
        for b in &self.blocks {
            match b {
                Block::Heading(r, t) => {
                    let r = shift(*r, dy);
                    if r.b < vt || r.t > vb {
                        continue;
                    }
                    let th = cv.measure(f.semi, t).1;
                    cv.text(f.semi, t, r.l + self.sc(2), r.b - th - self.sc(6), p.fg);
                }
                Block::Note(r, t) => {
                    let r = shift(*r, dy);
                    if t.is_empty() || r.b < vt || r.t > vb {
                        continue;
                    }
                    let s = cv.ellipsize(f.body, t, r.w() - self.sc(4));
                    cv.text(f.body, &s, r.l + self.sc(2), r.t + self.sc(2), p.dim);
                }
                Block::Card { rect, text_r, title, desc, enabled, labels } => {
                    let r = shift(*rect, dy);
                    if r.b < vt || r.t > vb {
                        continue;
                    }
                    cv.fill_round(r, rad, p.card);
                    cv.stroke_round(r, rad, p.card_border);
                    let (c_main, c_dim) = if *enabled { (p.fg, p.dim) } else { (p.off, p.off) };
                    let tx = r.l + self.sc(CARD_PAD);
                    let max_w = (text_r - tx - self.sc(8)).max(self.sc(24));
                    let cy = (r.t + r.b) / 2;
                    let title = cv.ellipsize(f.body, title, max_w);
                    if desc.is_empty() {
                        text_mid(cv, f.body, &title, tx, cy, c_main);
                    } else {
                        let th = cv.measure(f.body, &title).1;
                        let dh = cv.measure(f.small, desc).1;
                        let ty = cy - (th + dh) / 2;
                        cv.text(f.body, &title, tx, ty, c_main);
                        let desc = cv.ellipsize(f.small, desc, max_w);
                        cv.text(f.small, &desc, tx, ty + th, c_dim);
                    }
                    for (lr, s) in labels {
                        if !s.is_empty() {
                            text_mid(cv, f.body, s, lr.l, cy, c_dim);
                        }
                    }
                }
            }
        }
        if let Some(pn) = self.panel_idx.and_then(|i| self.panels.get(i)) {
            self.pview.draw(cv, pn, &f.view(), &p.view(), dy, vt, vb, self.chart_hover);
        }
        for c in self.controls.iter().filter(|c| c.layer == Layer::Page) {
            let r = shift(c.rect, dy);
            if r.b < vt || r.t > vb {
                continue;
            }
            self.draw_control(cv, c, r);
        }

        // Page header: back button and title.
        cv.set_clip(Some(Rect { l: cx0, t: 0, r: self.cw, b: vt }));
        let mut tx = self.col().0;
        let mut tcy = vt / 2 + slide;
        if let Some(c) = self.control(ID_BACK) {
            let r = shift(c.rect, slide);
            self.draw_control(cv, c, r);
            tx = r.r + self.sc(12);
            tcy = (r.t + r.b) / 2;
        }
        let title = cv.ellipsize(f.title, self.page.title(), col_r - tx);
        text_mid(cv, f.title, &title, tx, tcy - self.sc(2), p.fg);
        cv.set_clip(None);
        if let Some(e) = fade {
            cv.fade_rect(Rect { l: cx0, t: 0, r: self.cw, b: self.ch }, e, p.bg);
        }

        // Overlay scroll bar.
        if let Some((track, thumb)) = self.scrollbar() {
            let wide = self.sb_hover || matches!(self.drag, Drag::Thumb(_));
            if wide {
                let tr = Rect { l: thumb.l - self.sc(1), r: thumb.r + self.sc(1), t: track.t, b: track.b };
                cv.fill_round(tr, tr.w() as f32 / 2.0, p.sb_track);
            }
            cv.fill_round(thumb, thumb.w() as f32 / 2.0, p.sb);
        }

        self.draw_pane(cv);
        self.draw_tip(cv);
    }

    /// The navigation pane: hamburger, menu, selection indicator, footer.
    fn draw_pane(&self, cv: &mut Canvas) {
        let p = self.pal;
        let f = &self.fonts;
        let mut pw = self.pane_w();
        if self.overlay {
            // Light-dismiss overlay over the content: opaque, with an edge and a soft shadow.
            if let Some((a, b, t)) = self.anim.pane {
                pw = a + ((b - a) as f32 * Self::progress(t, PANE_ANIM_S)).round() as i32;
            }
            for i in 0..self.sc(8) {
                let al = (0x28 * (self.sc(8) - i) / self.sc(8)) as u8;
                cv.fill_rect(Rect { l: pw + i, t: 0, r: pw + i + 1, b: self.ch }, ka(if p.dark { al * 2 } else { al }));
            }
            cv.fill_rect(Rect { l: 0, t: 0, r: pw, b: self.ch }, p.pane);
            cv.fill_rect(Rect { l: pw - 1, t: 0, r: pw, b: self.ch }, p.card_border);
        }
        let clip_r = pw;
        let (mt, mb) = self.menu_band();
        let my = mt - self.nav_scroll;

        // Menu rows and items, clipped to the menu band.
        cv.set_clip(Some(Rect { l: 0, t: mt, r: clip_r, b: mb }));
        for row in &self.rows {
            match *row {
                NavRow::Header(r, t) => {
                    let r = shift(r, my);
                    let th = cv.measure(f.cap, t).1;
                    cv.text(f.cap, t, r.l + self.sc(12), r.b - th - self.sc(8), p.dim);
                }
                NavRow::Separator(r) => {
                    let r = shift(r, my);
                    let y = (r.t + r.b) / 2;
                    cv.fill_rect(Rect { l: r.l, t: y, r: r.r, b: y + 1 }, p.line);
                }
            }
        }
        for c in self.controls.iter().filter(|c| c.layer == Layer::Menu) {
            let r = self.client_rect(c);
            if r.b < mt || r.t > mb {
                continue;
            }
            self.draw_control(cv, c, r);
        }
        self.draw_indicator(cv, Some((mt, mb)), true);

        // Hamburger and footer.
        cv.set_clip(Some(Rect { l: 0, t: 0, r: clip_r, b: self.ch }));
        if self.nav_max_scroll() > 0 || self.footer_chip_h() > 0 {
            let y = self.footer_top() - self.sc(NAV_SEP_H) / 2 - 1;
            let ins = self.sc(NAV_INSET + 8);
            cv.fill_rect(Rect { l: ins, t: y, r: self.pane_w() - ins, b: y + 1 }, p.line);
        }
        for c in self.controls.iter().filter(|c| c.layer == Layer::Fixed && c.id != ID_BACK) {
            self.draw_control(cv, c, c.rect);
        }
        self.draw_indicator(cv, None, false);
        cv.set_clip(None);
    }

    /// The accent pill left of the selected item, sliding between items.
    fn draw_indicator(&self, cv: &mut Canvas, band: Option<(i32, i32)>, menu: bool) {
        let Some(c) = self.control(nav_id(self.page)) else { return };
        if (c.layer == Layer::Menu) != menu {
            return;
        }
        let r = self.client_rect(c);
        let mut cy = (r.t + r.b) / 2;
        if let Some((from, t)) = self.anim.ind {
            cy = from + ((cy - from) as f32 * Self::progress(t, IND_ANIM_S)).round() as i32;
        }
        if let Some((t, b)) = band {
            if cy < t || cy >= b {
                return;
            }
        }
        let (pw, ph) = (self.sc(3), self.sc(16));
        let t = cy - ph / 2;
        cv.fill_round(Rect { l: r.l, t, r: r.l + pw, b: t + ph }, pw as f32 / 2.0, self.pal.accent);
    }

    /// Tone of an Insights page (for its dot), when Insights is on.
    fn page_tone(&self, pg: Page) -> Option<crate::insights::Tone> {
        let i = pg.panel()?;
        self.ins_on.then(|| self.panels.get(i).map(|x| x.tone)).flatten()
    }

    fn draw_nav_item(&self, cv: &mut Canvas, _c: &Control, r: Rect, pg: Page, hover: bool, pressed: bool) {
        let p = self.pal;
        let f = &self.fonts;
        let cy = (r.t + r.b) / 2;
        let rad4 = self.sc(4) as f32;
        let sel = pg == self.page;
        let fill = if pressed {
            Some(p.nav_press)
        } else if sel {
            Some(p.nav_sel)
        } else if hover {
            Some(p.nav_hover)
        } else {
            None
        };
        if let Some(fl) = fill {
            cv.fill_round(r, rad4, fl);
        }
        let gx = r.l + self.sc(20);
        glyph(cv, f.icon, pg.glyph(), gx, cy, p.fg);
        let tone = self.page_tone(pg).map(|t| p.view().tone(t));
        if self.labels() {
            let dot = self.sc(8);
            let dx = r.r - self.sc(14);
            let right = if tone.is_some() { dx - dot - self.sc(8) } else { r.r - self.sc(8) };
            let label = cv.ellipsize(f.body, pg.title(), right - (r.l + self.sc(44)));
            text_mid(cv, f.body, &label, r.l + self.sc(44), cy, p.fg);
            if let Some(tc) = tone {
                pill(cv, dx, cy, dot, dot, tc);
            }
        } else if let Some(tc) = tone {
            // Compact: a small badge on the icon's corner.
            let d = self.sc(7);
            let (bx, by) = (gx + self.sc(8), cy - self.sc(7));
            if !self.mica {
                pill(cv, bx, by, d + 2, d + 2, p.bg);
            }
            pill(cv, bx, by, d, d, tc);
        }
    }

    /// Overview card for an Insights page: glyph, title, tone pill, headline, one fact.
    fn draw_summary(&self, cv: &mut Canvas, r: Rect, pg: Page, hover: bool, pressed: bool) {
        let p = self.pal;
        let f = &self.fonts;
        let rad = self.sc(6) as f32;
        cv.fill_round(r, rad, p.card);
        if pressed {
            cv.fill_round(r, rad, p.subtle_press);
        } else if hover {
            cv.fill_round(r, rad, p.subtle_hover);
        }
        cv.stroke_round(r, rad, p.card_border);
        let panel = pg.panel().filter(|_| self.ins_on).and_then(|i| self.panels.get(i));
        let (c_main, c_dim, c_icon) = if panel.is_some() { (p.fg, p.dim, p.accent) } else { (p.off, p.off, p.off) };
        let pad = self.sc(CARD_PAD);
        let cy = (r.t + r.b) / 2;
        glyph(cv, f.icon, pg.glyph(), r.l + pad + self.sc(12), r.t + pad + self.sc(10), c_icon);
        glyph(cv, f.icon_sm, G_CHEVRON_RIGHT, r.r - self.sc(24), cy, c_dim);
        let tx = r.l + pad + self.sc(36);
        let right = r.r - self.sc(48);
        let sh = cv.measure(f.semi, "Ag").1;
        let ty = r.t + pad - self.sc(2);
        let mut title_r = right;
        if let Some(pn) = panel {
            let (tw, _) = cv.measure(f.small, view::tone_label(pn.tone));
            let pw = self.sc(10 + 8 + 6 + 12) + tw;
            let px = right - pw;
            // Centred on the card, in line with the chevron.
            view::draw_pill(cv, &f.view(), &p.view(), self.dpi, px, cy - self.sc(12), pn.tone);
            title_r = px - self.sc(12);
        }
        let t = cv.ellipsize(f.semi, panel.map_or(pg.title(), |pn| pn.title), title_r - tx);
        cv.text(f.semi, &t, tx, ty, c_main);
        let (headline, fact) = match panel {
            Some(pn) => (pn.headline.as_str(), pn.lines.first().map_or("", |s| s.as_str())),
            None => ("Insights is off", "Turn on Insights above to see this."),
        };
        let bh = cv.measure(f.body, "Ag").1;
        let hy = ty + sh + self.sc(6);
        let h = cv.ellipsize(f.body, headline, title_r - tx);
        cv.text(f.body, &h, tx, hy, c_main);
        let s = cv.ellipsize(f.small, fact, title_r - tx);
        cv.text(f.small, &s, tx, hy + bh + self.sc(2), c_dim);
    }

    /// Compact pane: the hovered item's name, beside it.
    fn draw_tip(&self, cv: &mut Canvas) {
        let Some(pg) = self.tip.filter(|_| !self.labels()) else { return };
        let Some(c) = self.control(nav_id(pg)) else { return };
        let r = self.client_rect(c);
        let p = self.pal;
        let f = &self.fonts;
        let (tw, th) = cv.measure(f.body, pg.title());
        let (bw, bh) = (tw + self.sc(16), th + self.sc(10));
        let l = r.r + self.sc(8);
        let t = ((r.t + r.b) / 2 - bh / 2).clamp(0, (self.ch - bh).max(0));
        let br = Rect { l, t, r: l + bw, b: t + bh };
        cv.fill_round(br, self.sc(4) as f32, p.bubble);
        cv.stroke_round(br, self.sc(4) as f32, p.ctl_border);
        cv.text(f.body, pg.title(), l + self.sc(8), t + self.sc(5), p.fg);
    }

    fn draw_control(&self, cv: &mut Canvas, c: &Control, r: Rect) {
        let p = self.pal;
        let f = &self.fonts;
        let (hover, pressed) = self.state(c.id, Part::Body);
        let en = c.enabled;
        let rad4 = self.sc(4) as f32;
        let cy = (r.t + r.b) / 2;
        let focus = self.focus_visible && self.focus == Some(c.id);
        // Standard control fill by state.
        let ctl_fill = if !en {
            p.ctl_dis
        } else if pressed {
            p.ctl_press
        } else if hover {
            p.ctl_hover
        } else {
            p.ctl
        };
        let text_c = if en { p.fg } else { p.off };
        let mut ring_r = (inflate(r, self.sc(3)), rad4 + self.sc(3) as f32);
        match c.kind {
            Kind::Nav(pg) => {
                self.draw_nav_item(cv, c, r, pg, hover, pressed);
                ring_r = (r, rad4);
            }
            Kind::Summary(pg) => {
                self.draw_summary(cv, r, pg, hover, pressed);
                ring_r = (r, self.sc(6) as f32);
            }
            Kind::Icon(g) => {
                if en && pressed {
                    cv.fill_round(r, rad4, p.subtle_press);
                } else if en && hover {
                    cv.fill_round(r, rad4, p.subtle_hover);
                }
                glyph(cv, f.icon, g, (r.l + r.r) / 2, cy, text_c);
                ring_r = (r, rad4);
            }
            Kind::Toggle => {
                let on = self.flag(c.id);
                let track = self.toggle_track(r);
                let trad = track.h() as f32 / 2.0;
                let (kh, kw) = if pressed && en {
                    (self.sc(14), self.sc(17))
                } else if hover && en {
                    (self.sc(14), self.sc(14))
                } else {
                    (self.sc(12), self.sc(12))
                };
                if on {
                    let fill = if !en {
                        p.tog_dis
                    } else if pressed {
                        p.accent_press
                    } else if hover {
                        p.accent_hover
                    } else {
                        p.accent
                    };
                    cv.fill_round(track, trad, fill);
                    let kx = track.r - self.sc(10) - (kw - kh) / 2;
                    pill(cv, kx, cy, kw, kh, if en { p.on_accent } else { p.knob_dis });
                } else {
                    cv.fill_round(track, trad, if hover && en { p.tog_fill_hover } else { p.tog_fill });
                    cv.stroke_round(track, trad, if en { p.tog_stroke } else { p.tog_dis });
                    let kx = track.l + self.sc(10) + (kw - kh) / 2;
                    pill(cv, kx, cy, kw, kh, if en { p.knob_off } else { p.tog_dis });
                }
                let label = if on { "On" } else { "Off" };
                let (lw, lh) = cv.measure(f.body, label);
                cv.text(f.body, label, track.l - self.sc(12) - lw, cy - lh / 2, text_c);
                ring_r = (inflate(track, self.sc(3)), trad + self.sc(3) as f32);
            }
            Kind::Dropdown => {
                cv.fill_round(r, rad4, ctl_fill);
                cv.stroke_round(r, rad4, p.ctl_border);
                let (opts, cur) = self.dd_options(c.id);
                let s = opts.get(cur).map_or("", |s| s.as_str());
                let s = cv.ellipsize(f.body, s, r.w() - self.sc(12 + 36));
                text_mid(cv, f.body, &s, r.l + self.sc(12), cy, text_c);
                let gy = if pressed { cy + self.sc(1) } else { cy };
                glyph(cv, f.icon_sm, G_CHEVRON_DOWN, r.r - self.sc(20), gy, if en { p.dim } else { p.off });
            }
            Kind::Slider => {
                let (lo, hi, ..) = slider_spec(c.id);
                let v = self.slider_get(c.id);
                let (a, b) = self.slider_span(r);
                let frac = (v - lo) as f32 / (hi - lo).max(1) as f32;
                let cx = a + ((b - a) as f32 * frac).round() as i32;
                let th = self.sc(4).max(2);
                let rail = Rect { l: a - self.sc(2), t: cy - th / 2, r: b + self.sc(2), b: cy - th / 2 + th };
                cv.fill_round(rail, th as f32 / 2.0, if en { p.rail } else { p.tog_dis });
                let accent = if en { p.accent } else { p.off };
                cv.fill_round(Rect { r: cx, ..rail }, th as f32 / 2.0, accent);
                let dragging = self.drag == Drag::Slider(c.id);
                let d = self.sc(20);
                pill(cv, cx, cy, d, d, p.thumb);
                cv.stroke_round(Rect { l: cx - d / 2, t: cy - d / 2, r: cx - d / 2 + d, b: cy - d / 2 + d }, d as f32 / 2.0, p.thumb_rim);
                let dot = if dragging || (hover && en) { self.sc(14) } else { self.sc(12) };
                pill(cv, cx, cy, dot, dot, accent);
            }
            Kind::Stepper => {
                let (hb, _) = self.state(c.id, Part::Body);
                let (hm, pm) = self.state(c.id, Part::Minus);
                let (hp, pp) = self.state(c.id, Part::Plus);
                let any = hb || hm || hp;
                let fill = if !en {
                    p.ctl_dis
                } else if any {
                    p.ctl_hover
                } else {
                    p.ctl
                };
                cv.fill_round(r, rad4, fill);
                cv.stroke_round(r, rad4, p.ctl_border);
                text_mid(cv, f.body, &self.num_text(c.id), r.l + self.sc(12), cy, text_c);
                let (lo, hi, ..) = num_spec(c.id);
                let v = self.num_get(c.id);
                let (mr, pr) = self.stepper_parts(r);
                for (br, g, can, h, pd) in [(mr, G_MINUS, v > lo, hm, pm), (pr, G_PLUS, v < hi, hp, pp)] {
                    let live = en && can;
                    if live && (h || pd) {
                        cv.fill_round(br, rad4 - 1.0, if pd { p.subtle_press } else { p.subtle_hover });
                    }
                    glyph(cv, f.icon_sm, g, (br.l + br.r) / 2, cy, if live { p.dim } else { p.off });
                }
            }
            Kind::Button(text, accent) => {
                if accent && en {
                    let fill = if pressed {
                        p.accent_press
                    } else if hover {
                        p.accent_hover
                    } else {
                        p.accent
                    };
                    cv.fill_round(r, rad4, fill);
                    let (tw, th) = cv.measure(f.body, text);
                    cv.text(f.body, text, (r.l + r.r - tw) / 2, cy - th / 2, p.on_accent);
                } else {
                    cv.fill_round(r, rad4, ctl_fill);
                    cv.stroke_round(r, rad4, p.ctl_border);
                    let (tw, th) = cv.measure(f.body, text);
                    let c = if !en {
                        p.off
                    } else if pressed {
                        p.dim
                    } else {
                        p.fg
                    };
                    cv.text(f.body, text, (r.l + r.r - tw) / 2, cy - th / 2, c);
                }
            }
            Kind::Link(text) => {
                let col = if !en {
                    p.off
                } else if pressed {
                    p.accent.mix(p.dim, 0.5)
                } else if hover {
                    p.accent.mix(p.fg, 0.25)
                } else {
                    p.accent
                };
                let (tw, th) = cv.measure(f.body, text);
                cv.text(f.body, text, (r.l + r.r - tw) / 2, cy - th / 2, col);
            }
            Kind::Swatch(m) => {
                if pressed {
                    cv.fill_round(r, rad4, p.subtle_press);
                } else if hover {
                    cv.fill_round(r, rad4, p.subtle_hover);
                }
                let (sw, sh) = (self.sc(32), self.sc(20));
                let cx = (r.l + r.r) / 2;
                let s = Rect { l: cx - sw / 2, t: cy - sh / 2, r: cx - sw / 2 + sw, b: cy - sh / 2 + sh };
                cv.fill_round(s, rad4, hx(self.swatch_rgb(m)));
                cv.stroke_round(s, rad4, p.swatch_rim);
                ring_r = (r, rad4);
            }
            Kind::Glyph(g) => {
                if en && pressed {
                    cv.fill_round(r, rad4, p.subtle_press);
                } else if en && hover {
                    cv.fill_round(r, rad4, p.subtle_hover);
                }
                glyph(cv, f.icon_sm, g, (r.l + r.r) / 2, cy, text_c);
                ring_r = (r, rad4);
            }
        }
        if focus {
            ring(cv, ring_r.0, ring_r.1, p.fg);
        }
    }
}

// ------------------------------------------------------------------ actions

/// The draft changed: rebuild the layout, repaint, and apply it live
/// (debounced so slider drags / stepper repeats apply smoothly, not per pixel).
fn changed(hwnd: HWND) {
    let mut live = false;
    if let Some(u) = ui() {
        if READY.load(Relaxed) {
            u.dirty = true;
            live = true;
        }
        u.layout();
    }
    invalidate(hwnd);
    if live {
        unsafe { SetTimer(hwnd, T_LIVE, 150, None) };
    }
}

/// Apply anything still pending (closing the window, editing the file).
fn flush(hwnd: HWND) {
    unsafe { KillTimer(hwnd, T_LIVE) };
    if ui().is_some_and(|u| u.dirty) {
        commit(hwnd);
    }
}

/// Copy the settings this window owns into `dst`; everything else (widget
/// position and placement, ...) keeps its live value.
fn merge(dst: &mut Settings, src: &Settings) {
    dst.max_width_pct = src.max_width_pct;
    dst.displays = src.displays.clone();
    dst.stacked_on = src.stacked_on.clone();
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
    dst.achievements = src.achievements;
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
    dst.insights = src.insights;
    dst.insight_toasts = src.insight_toasts;
    dst.bag_guard = src.bag_guard;
}

/// Validate, apply to the app, save. Returns false if the app is gone.
fn commit(hwnd: HWND) -> bool {
    let Some((draft, autostart)) = ui().map(|u| {
        let d = &mut u.draft;
        d.text_scale = d.text_scale.clamp(70, 200);
        d.interval_ms = d.interval_ms.clamp(250, 10_000);
        d.cap_reset_day = d.cap_reset_day.clamp(1, 28);
        d.data_cap_gb = d.data_cap_gb.clamp(0.0, 1_000_000.0);
        if d.crit_pct < d.warn_pct {
            d.crit_pct = d.warn_pct;
        }
        (u.draft.clone(), u.autostart)
    }) else {
        return false;
    };
    if autostart != crate::autostart::enabled() {
        crate::autostart::set(autostart);
    }
    let Some(a) = app() else { return false };
    let mut live = a.m.s.clone();
    merge(&mut live, &draft);
    a.apply_new_settings(live);
    let (theme, dark) = (a.m.s.theme, effective_dark(a.m.s.theme));

    // Re-fetch: applying may have pumped messages.
    if let Some(u) = ui() {
        u.dirty = false;
        if u.dark != dark || u.theme != theme {
            u.dark = dark;
            u.theme = theme;
            apply_theme(hwnd);
        }
        u.layout();
    }
    invalidate(hwnd);
    true
}


fn pick_colour(hwnd: HWND, m: Metric) {
    let Some((cur, mut cust)) = ui().map(|u| (u.swatch_rgb(m), u.cust)) else { return };
    let mut cc: CHOOSECOLORW = unsafe { std::mem::zeroed() };
    cc.lStructSize = std::mem::size_of::<CHOOSECOLORW>() as u32;
    cc.hwndOwner = hwnd;
    cc.rgbResult = rgb_to_cr(cur);
    cc.lpCustColors = cust.as_mut_ptr();
    cc.Flags = CC_RGBINIT | CC_FULLOPEN | CC_ANYCOLOR;
    // Modal: runs a nested message loop. No `ui()` borrow is used across it.
    let picked = unsafe { ChooseColorW(&mut cc) } != 0;
    let Some(u) = ui() else { return };
    u.cust = cust;
    if picked {
        let rgb = cr_to_rgb(cc.rgbResult) & 0xFF_FFFF;
        u.draft.colors.retain(|(x, _)| *x != m);
        u.draft.colors.push((m, rgb));
        changed(hwnd);
    }
}

fn move_metric(hwnd: HWND, pos: usize, dir: i32) {
    let Some(u) = ui() else { return };
    let to = pos as i32 + dir;
    if to < 0 || to as usize >= u.draft.order.len() {
        return;
    }
    u.draft.order.swap(pos, to as usize);
    changed(hwnd);
    let Some(u) = ui() else { return };
    // Keep the focus on the row that moved.
    let (same, other) = if dir < 0 { (ID_MUP + to, ID_MDN + to) } else { (ID_MDN + to, ID_MUP + to) };
    let id = if u.control(same).is_some_and(|c| c.enabled) { same } else { other };
    if u.focus.is_some() {
        u.focus = Some(id);
        u.ensure_visible(id);
    }
    u.refresh_hover();
}

/// Open the native popup menu of a drop-down under it.
fn open_dropdown(hwnd: HWND, id: i32) {
    let Some(u) = ui() else { return };
    let Some(c) = u.control(id) else { return };
    let r = u.client_rect(c);
    let (opts, cur) = u.dd_options(id);
    if opts.is_empty() {
        return;
    }
    u.open_dd = Some(id);
    u.pressed = None;
    invalidate(hwnd);
    let cmd = unsafe {
        let menu = CreatePopupMenu();
        for (i, o) in opts.iter().enumerate() {
            // The day list reads better as 4 columns of 7.
            let brk = if id == ID_DAY && i > 0 && i % 7 == 0 { MF_MENUBARBREAK } else { 0 };
            let w = wide(o);
            AppendMenuW(menu, MF_STRING | brk, i + 1, w.as_ptr());
        }
        CheckMenuRadioItem(menu, 1, opts.len() as u32, cur as u32 + 1, MF_BYCOMMAND);
        let mut tl = POINT { x: r.l, y: r.t };
        let mut br = POINT { x: r.r, y: r.b };
        ClientToScreen(hwnd, &mut tl);
        ClientToScreen(hwnd, &mut br);
        let tpm = TPMPARAMS {
            cbSize: std::mem::size_of::<TPMPARAMS>() as u32,
            rcExclude: RECT { left: tl.x, top: tl.y, right: br.x, bottom: br.y },
        };
        // Nested message loop: `u` is not used again below.
        let cmd = TrackPopupMenuEx(menu, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN | TPM_VERTICAL, tl.x, br.y, hwnd, &tpm);
        DestroyMenu(menu);
        cmd
    };
    let Some(u) = ui() else { return };
    u.open_dd = None;
    u.refresh_hover();
    if id == ID_MODE {
        // Placement is the app's: floating starts where the docked widget is.
        if let Some((_, m)) = (cmd > 0).then(|| MODES.get(cmd as usize - 1)).flatten() {
            u.draft.mode = *m;
            u.layout();
            invalidate(hwnd);
            run(if *m == Mode::Float { Cmd::Float } else { Cmd::Dock });
        } else {
            invalidate(hwnd);
        }
        return;
    }
    if cmd > 0 && u.dd_select(id, cmd as usize - 1) {
        changed(hwnd);
    } else {
        invalidate(hwnd);
    }
}

/// Ask the app to do something. May pump messages: no `ui()` borrow is held across it.
fn run(cmd: Cmd) {
    if let Some(a) = app() {
        a.run(cmd);
    }
}


/// Navigate to `p` (recording it for Back) and load its Insights data.
fn go(p: Page) {
    let Some(u) = ui() else { return };
    if u.navigate(p, true) && p.shows_insights() {
        if let Some(a) = app() {
            sync_insights(a, false);
        }
    }
}

fn go_back() {
    let Some(u) = ui() else { return };
    if u.back() && u.page.shows_insights() {
        if let Some(a) = app() {
            sync_insights(a, false);
        }
    }
}

/// Rebuild the Insights panels from the app (at most every 2 s unless
/// `force`); relayout and repaint only if something visible changed.
fn sync_insights(a: &crate::app::App, force: bool) {
    let Some(u) = ui() else { return };
    if u.hwnd.is_null() {
        return;
    }
    // SAFETY: no preconditions.
    let now = unsafe { GetTickCount64() };
    if !force && now.saturating_sub(u.built) < REBUILD_MS {
        return;
    }
    let (on, panels) = view::fetch(a, unix_ms());
    if u.take_panels(on, panels) {
        u.layout();
        u.refresh_hover();
        invalidate(u.hwnd);
    }
}

/// Click / Space / Enter on a control.
fn activate(hwnd: HWND, id: i32) {
    let Some(u) = ui() else { return };
    let Some(kind) = u.control(id).filter(|c| c.enabled).map(|c| c.kind) else { return };
    let metrics = Metric::ALL.len() as i32;
    match kind {
        Kind::Nav(p) => {
            let keep = u.focus.is_some() || u.focus_visible;
            go(p);
            if keep {
                if let Some(u) = ui() {
                    u.focus = Some(nav_id(p));
                }
            }
            invalidate(hwnd);
        }
        Kind::Summary(p) => go(p),
        Kind::Icon(_) => match id {
            ID_HAMBURGER => u.toggle_pane(),
            ID_BACK => go_back(),
            _ => {}
        },
        Kind::Toggle => {
            if u.toggle(id) {
                changed(hwnd);
            }
        }
        Kind::Dropdown => open_dropdown(hwnd, id),
        Kind::Swatch(m) => pick_colour(hwnd, m),
        Kind::Link(_) => {
            if (ID_CRESET..ID_CRESET + metrics).contains(&id) {
                let m = Metric::ALL[(id - ID_CRESET) as usize];
                u.draft.colors.retain(|(x, _)| *x != m);
                changed(hwnd);
            }
        }
        Kind::Glyph(_) => {
            if (ID_MUP..ID_MUP + metrics).contains(&id) {
                move_metric(hwnd, (id - ID_MUP) as usize, -1);
            } else if (ID_MDN..ID_MDN + metrics).contains(&id) {
                move_metric(hwnd, (id - ID_MDN) as usize, 1);
            }
        }
        Kind::Button(..) => match id {
            ID_RESETALL => {
                u.draft.colors.clear();
                changed(hwnd);
            }
            ID_REPORT => run(Cmd::Report),
            ID_WEBSITE => run(Cmd::Website),
            ID_UPDATES => run(Cmd::CheckUpdates),
            ID_RECHECK => run(Cmd::RecheckIcons),
            ID_RESETPOS => run(Cmd::ResetPosition),
            ID_EDITFILE => {
                // Save pending edits first, so the file shows them.
                flush(hwnd);
                run(Cmd::EditSettings);
            }
            ID_DATADIR => run(Cmd::DataFolder),
            ID_ACH_OPEN => {
                // Apply a pending on/off first so the window opens in the right state.
                flush(hwnd);
                if let Some(a) = app() {
                    a.open_achievements();
                }
            }
            ID_INS_CLEAR => {
                if let Some(a) = app() {
                    a.ins.clear();
                    sync_insights(a, true);
                }
            }
            _ => {}
        },
        Kind::Slider | Kind::Stepper => {}
    }
}

fn slider_from_x(hwnd: HWND, id: i32, x: i32) {
    let Some(u) = ui() else { return };
    let Some(c) = u.control(id) else { return };
    let (a, b) = u.slider_span(u.client_rect(c));
    let (lo, hi, _, step) = slider_spec(id);
    let frac = ((x - a) as f32 / (b - a).max(1) as f32).clamp(0.0, 1.0);
    let raw = lo as f32 + frac * (hi - lo) as f32;
    let v = lo + (((raw - lo as f32) / step as f32).round() as i32) * step;
    if u.slider_set(id, v) {
        changed(hwnd);
    }
}

/// Arrow / page / home / end keys and the wheel on sliders and steppers.
/// `dir` is +1 / -1; `big` uses a page-sized step; `edge` jumps to an end.
fn adjust(hwnd: HWND, id: i32, kind: Kind, dir: i32, big: bool, edge: bool) {
    let Some(u) = ui() else { return };
    let moved = match kind {
        Kind::Slider => {
            let (lo, hi, key, _) = slider_spec(id);
            let v = u.slider_get(id);
            let n = if edge {
                if dir > 0 {
                    hi
                } else {
                    lo
                }
            } else {
                let s = if big { key * 4 } else { key };
                // Snap to the step grid (a hand-edited value may be off it).
                let base = lo + ((v - lo) / s) * s;
                if dir > 0 {
                    base + s
                } else if base < v {
                    base
                } else {
                    base - s
                }
            };
            u.slider_set(id, n)
        }
        Kind::Stepper => {
            if edge {
                let (lo, hi, ..) = num_spec(id);
                let old = u.num_get(id);
                // End on the data cap would mean a million GB: ignore it.
                let n = match (dir > 0, id) {
                    (true, ID_CAP) => return,
                    (true, _) => hi,
                    _ => lo,
                };
                u.num_set(id, n);
                (u.num_get(id) - old).abs() > 1e-9
            } else {
                u.num_step(id, dir, if big { 10.0 } else { 1.0 })
            }
        }
        _ => false,
    };
    if moved {
        changed(hwnd);
    }
}


fn on_key(hwnd: HWND, key: u16) {
    let ctrl = unsafe { GetKeyState(VK_CONTROL as i32) } < 0;
    let shift = unsafe { GetKeyState(VK_SHIFT as i32) } < 0;
    let Some(u) = ui() else { return };
    let focus = u.focus.and_then(|f| u.control(f).filter(|c| c.enabled).map(|c| (c.id, c.kind)));
    let in_nav = u.focus.is_some_and(|f| u.in_nav(f));
    match key {
        VK_TAB if ctrl => {
            let p = u.page.step(if shift { -1 } else { 1 });
            go(p);
            if in_nav {
                if let Some(u) = ui() {
                    u.focus = Some(nav_id(p));
                }
            }
        }
        VK_TAB => u.move_focus(if shift { -1 } else { 1 }),
        VK_F6 => u.switch_region(),
        VK_BROWSER_BACK => go_back(),
        // Esc only dismisses: the overlay pane, a tooltip. It never closes the window.
        VK_ESCAPE => {
            if u.overlay {
                u.overlay = false;
                u.layout();
                if in_nav && u.focus != Some(ID_HAMBURGER) {
                    u.focus = Some(ID_HAMBURGER);
                }
            }
            u.tip = None;
        }
        VK_RETURN => {
            if let Some((id, kind)) = focus {
                if !matches!(kind, Kind::Slider | Kind::Stepper | Kind::Toggle) {
                    u.focus_visible = true;
                    activate(hwnd, id);
                }
            }
        }
        VK_SPACE => {
            if let Some((id, kind)) = focus {
                if !matches!(kind, Kind::Slider | Kind::Stepper) {
                    u.focus_visible = true;
                    activate(hwnd, id);
                }
            }
        }
        VK_F4 => {
            if let Some((id, Kind::Dropdown)) = focus {
                activate(hwnd, id);
            }
        }
        VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT | VK_HOME | VK_END => {
            let dir = if matches!(key, VK_RIGHT | VK_UP | VK_PRIOR | VK_END) { 1 } else { -1 };
            // For steppers Up is "more" and Page Up is a big step.
            let big = matches!(key, VK_PRIOR | VK_NEXT);
            let edge = matches!(key, VK_HOME | VK_END);
            match focus {
                Some((id, kind @ (Kind::Slider | Kind::Stepper))) => {
                    u.focus_visible = true;
                    adjust(hwnd, id, kind, dir, big, edge);
                }
                // Pane: the arrows move the focus, Enter / Space opens the page.
                Some((id, _)) if in_nav => {
                    let ids = u.nav_ids();
                    let cur = ids.iter().position(|x| *x == id).unwrap_or(0);
                    let next = match key {
                        VK_UP | VK_LEFT => cur.saturating_sub(1),
                        VK_DOWN | VK_RIGHT => (cur + 1).min(ids.len() - 1),
                        VK_HOME => 0,
                        VK_END => ids.len() - 1,
                        _ => cur,
                    };
                    u.set_focus(Some(ids[next]));
                }
                Some((id, Kind::Dropdown)) if matches!(key, VK_UP | VK_DOWN) => {
                    let (opts, cur) = u.dd_options(id);
                    let next = if key == VK_UP { cur.saturating_sub(1) } else { (cur + 1).min(opts.len().saturating_sub(1)) };
                    u.focus_visible = true;
                    if u.dd_select(id, next) {
                        changed(hwnd);
                    }
                }
                _ => {
                    // Scroll the page.
                    let (vt, vb) = u.view();
                    let page = (vb - vt - u.sc(WHEEL_STEP)).max(u.sc(WHEEL_STEP));
                    let t = match key {
                        VK_UP => u.target - u.sc(WHEEL_STEP),
                        VK_DOWN => u.target + u.sc(WHEEL_STEP),
                        VK_PRIOR => u.target - page,
                        VK_NEXT => u.target + page,
                        VK_HOME => 0,
                        VK_END => u.max_scroll(),
                        _ => u.target,
                    };
                    u.scroll_to(t, true);
                }
            }
        }
        _ => {}
    }
    invalidate(hwnd);
}

fn on_wheel(hwnd: HWND, delta: i32, sx: i32, sy: i32) {
    let Some(u) = ui() else { return };
    let mut pt = POINT { x: sx, y: sy };
    unsafe { ScreenToClient(hwnd, &mut pt) };
    // Over the pane: scroll its menu when it does not fit.
    if u.over_pane(pt.x) {
        let max = u.nav_max_scroll();
        if max > 0 {
            u.nav_scroll = (u.nav_scroll - delta * u.sc(WHEEL_STEP) / 120).clamp(0, max);
            u.tip = None;
            u.refresh_hover();
            invalidate(hwnd);
        }
        return;
    }
    // A focused slider / stepper under the mouse takes the wheel.
    if let Some((id, _)) = u.hit(pt.x, pt.y) {
        if u.focus == Some(id) {
            if let Some(kind) = u.control(id).map(|c| c.kind).filter(|k| matches!(k, Kind::Slider | Kind::Stepper)) {
                adjust(hwnd, id, kind, if delta > 0 { 1 } else { -1 }, false, false);
                return;
            }
        }
    }
    let t = u.target - delta * u.sc(WHEEL_STEP) / 120;
    u.scroll_to(t, true);
}

fn track_mouse(hwnd: HWND, hover: bool) -> bool {
    let flags = if hover { TME_LEAVE | TME_HOVER } else { TME_LEAVE };
    let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: flags, hwndTrack: hwnd, dwHoverTime: u32::MAX /* HOVER_DEFAULT */ };
    unsafe { TrackMouseEvent(&mut t) != 0 }
}

fn on_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let Some(u) = ui() else { return };
    if !u.tracking {
        u.tracking = track_mouse(hwnd, false);
    }
    match u.drag {
        Drag::Slider(id) => {
            slider_from_x(hwnd, id, x);
            return;
        }
        Drag::Thumb(grab) => {
            if let Some((track, thumb)) = u.scrollbar() {
                let span = (track.h() - thumb.h()).max(1);
                let frac = ((y - grab - track.t) as f32 / span as f32).clamp(0.0, 1.0);
                let t = (frac * u.max_scroll() as f32).round() as i32;
                u.scroll_to(t, false);
            }
            return;
        }
        Drag::None => {}
    }
    let hover = u.hit(x, y);
    let sb = !u.over_pane(x) && u.scrollbar().is_some_and(|(t, _)| t.contains(x, y));
    let chart = u.chart_at(x, y);
    if hover != u.hover || sb != u.sb_hover || chart != u.chart_hover {
        if hover != u.hover {
            u.tip = None;
            // Compact pane: ask for WM_MOUSEHOVER to show the item's name.
            let nav = hover.and_then(|(id, _)| u.control(id)).is_some_and(|c| matches!(c.kind, Kind::Nav(_)));
            if nav && !u.labels() {
                track_mouse(hwnd, true);
            }
        }
        u.hover = hover;
        u.sb_hover = sb;
        u.chart_hover = chart;
        invalidate(hwnd);
    }
}

fn on_mouse_hover(hwnd: HWND) {
    let Some(u) = ui() else { return };
    let pg = u.hover.and_then(|(id, _)| u.control(id)).and_then(|c| match c.kind {
        Kind::Nav(p) => Some(p),
        _ => None,
    });
    if pg.is_some() && !u.labels() && u.tip != pg {
        u.tip = pg;
        invalidate(hwnd);
    }
}

fn on_button_down(hwnd: HWND, x: i32, y: i32) {
    let Some(u) = ui() else { return };
    u.focus_visible = false;
    u.tip = None;
    // A click outside the overlay pane closes it (and does nothing else).
    if u.overlay && x >= u.pane_w() {
        u.overlay = false;
        u.layout();
        u.refresh_hover();
        invalidate(hwnd);
        return;
    }
    // Scroll bar first: it overlays the right edge of the page.
    if let Some((track, thumb)) = u.scrollbar().filter(|_| !u.over_pane(x)) {
        if track.contains(x, y) {
            if y >= thumb.t && y < thumb.b {
                u.drag = Drag::Thumb(y - thumb.t);
            } else {
                let (vt, vb) = u.view();
                let page = (vb - vt - u.sc(WHEEL_STEP)).max(u.sc(WHEEL_STEP));
                let t = if y < thumb.t { u.target - page } else { u.target + page };
                u.scroll_to(t, true);
            }
            unsafe { SetCapture(hwnd) };
            invalidate(hwnd);
            return;
        }
    }
    let hit = u.hit(x, y);
    u.hover = hit;
    u.pressed = hit;
    let Some((id, part)) = hit else {
        u.focus = None;
        invalidate(hwnd);
        return;
    };
    u.focus = Some(id);
    unsafe { SetCapture(hwnd) };
    let kind = u.control(id).map(|c| c.kind);
    match (kind, part) {
        (Some(Kind::Slider), _) => {
            u.drag = Drag::Slider(id);
            slider_from_x(hwnd, id, x);
        }
        (Some(Kind::Stepper), Part::Minus | Part::Plus) => {
            u.repeat_n = 0;
            let dir = if part == Part::Plus { 1 } else { -1 };
            unsafe { SetTimer(hwnd, T_REPEAT, 400, None) };
            if u.num_step(id, dir, 1.0) {
                changed(hwnd);
            }
        }
        _ => {}
    }
    invalidate(hwnd);
}

fn on_button_up(hwnd: HWND, x: i32, y: i32) {
    let Some(u) = ui() else { return };
    let was = u.pressed.take();
    let dragged = u.drag != Drag::None;
    u.drag = Drag::None;
    unsafe {
        KillTimer(hwnd, T_REPEAT);
        // Sends WM_CAPTURECHANGED, which only clears the state cleared above.
        ReleaseCapture();
    }
    let Some(u) = ui() else { return };
    let hit = u.hit(x, y);
    u.hover = hit;
    u.sb_hover = !u.over_pane(x) && u.scrollbar().is_some_and(|(t, _)| t.contains(x, y));
    invalidate(hwnd);
    if dragged {
        return;
    }
    if let (Some((id, Part::Body)), Some(h)) = (was, hit) {
        if h == (id, Part::Body) {
            activate(hwnd, id);
        }
    }
}

/// Auto-repeat of a held stepper button: 400 ms, then every 50 ms, faster
/// steps after a while on wide ranges.
fn on_repeat(hwnd: HWND) {
    let Some(u) = ui() else { return };
    let Some((id, part)) = u.pressed.filter(|p| p.1 != Part::Body) else {
        unsafe { KillTimer(hwnd, T_REPEAT) };
        return;
    };
    unsafe { SetTimer(hwnd, T_REPEAT, 50, None) };
    if u.hover != Some((id, part)) {
        return; // the mouse slid off the button: pause
    }
    u.repeat_n += 1;
    let (lo, hi, step, _) = num_spec(id);
    let mut mult = if u.repeat_n > 40 {
        100.0
    } else if u.repeat_n > 15 {
        10.0
    } else {
        1.0
    };
    while mult > 1.0 && step * mult > (hi - lo) / 10.0 {
        mult /= 10.0;
    }
    if u.num_step(id, if part == Part::Plus { 1 } else { -1 }, mult) {
        changed(hwnd);
    } else {
        unsafe { KillTimer(hwnd, T_REPEAT) };
    }
}

// ------------------------------------------------------------------ theme

/// Windows 11: Mica behind the whole window (frame extended into the client
/// area, painted with alpha 0). Returns false where that is not available.
pub(crate) fn enable_mica(hwnd: HWND) -> bool {
    if darkmode::build() < 22000 {
        return false;
    }
    unsafe {
        // DWMWA_SYSTEMBACKDROP_TYPE = 38, DWMSBT_MAINWINDOW = 2 (22H2+);
        // build 22000 only knows the older DWMWA_MICA_EFFECT = 1029.
        let kind: i32 = 2;
        let mut ok = DwmSetWindowAttribute(hwnd, 38, &kind as *const i32 as _, 4) >= 0;
        if !ok {
            let on: i32 = 1;
            ok = DwmSetWindowAttribute(hwnd, 1029, &on as *const i32 as _, 4) >= 0;
        }
        if !ok {
            return false;
        }
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        DwmExtendFrameIntoClientArea(hwnd, &m) >= 0
    }
}

fn apply_theme(hwnd: HWND) {
    let Some(u) = ui() else { return };
    u.pal = Pal::new(u.dark, u.mica, accent_color(u.dark));
    // Process-wide (popup menus, tray menu), then the window itself.
    darkmode::set_app_mode(u.theme);
    darkmode::allow_window(hwnd, u.dark);
    style_window(hwnd, u.dark);
    unsafe { RedrawWindow(hwnd, std::ptr::null(), std::ptr::null_mut(), RDW_INVALIDATE | RDW_FRAME) };
}


// ------------------------------------------------------------------ window

pub fn register(hinst: HINSTANCE) {
    unsafe {
        let cls = wide(CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hIcon: LoadIconW(hinst, 1 as _),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            lpszClassName: cls.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassExW(&wc) == 0 {
            diag(&format!("main window: RegisterClassExW failed ({})", GetLastError()));
        }
    }
}

const STYLE: u32 = WS_OVERLAPPEDWINDOW;

/// Work area of the monitor nearest to `r`.
fn work_area_near(r: Rect) -> Option<Rect> {
    unsafe {
        let rc = RECT { left: r.l, top: r.t, right: r.r, bottom: r.b };
        let mon = MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        (GetMonitorInfoW(mon, &mut mi) != 0).then(|| {
            let w = mi.rcWork;
            Rect { l: w.left, t: w.top, r: w.right, b: w.bottom }
        })
    }
}

/// Where the window opens: the saved placement (kept on a visible monitor),
/// else the default size centred on the main taskbar's work area.
fn initial_rect(a: &crate::app::App, dpi: u32) -> (Rect, bool) {
    if let Some(p) = a.m.s.window {
        let r = p.rect();
        if let Some(work) = work_area_near(r) {
            return (nav::clamp_rect(r, work), p.max);
        }
    }
    let tb = a.primary_taskbar();
    let work = tb.map(|t| t.work).unwrap_or(Rect { l: 0, t: 0, r: 1280, b: 720 });
    let sc = |v: i32| (v * dpi as i32 + 48) / 96;
    let mut rc = RECT { left: 0, top: 0, right: sc(DEF_W), bottom: sc(DEF_H) };
    unsafe { AdjustWindowRectExForDpi(&mut rc, STYLE, 0, 0, dpi) };
    (nav::centre_in(rc.right - rc.left, rc.bottom - rc.top, work), false)
}

/// Open the main window on `page`, or bring the existing one to the front
/// (restored if minimised) and navigate it there.
pub fn open(a: &mut crate::app::App, page: Page) {
    unsafe {
        if let Some(u) = ui().filter(|u| !u.hwnd.is_null()) {
            let hwnd = u.hwnd;
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            u.navigate(page, true);
            sync_insights(a, true);
            SetForegroundWindow(hwnd);
            return;
        }
        let hinst = GetModuleHandleW(std::ptr::null());
        // Process-wide, and before any window exists so menus pick it up too.
        darkmode::set_app_mode(a.m.s.theme);

        let mut draft = a.m.s.clone();
        draft.order = full_order(&draft.order);

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

        let dpi = a.primary_taskbar().map_or(96, |t| t.dpi).max(96);
        let (rect, max) = initial_rect(a, dpi);
        let mut state = Box::new(Ui::new(dpi, a.m.dark, a.m.s.theme, draft, adapters, labels, a.displays(), page));
        let (on, panels) = view::fetch(a, unix_ms());
        state.take_panels(on, panels);
        state.normal = rect;
        state.maximized = max;
        free_ui();
        UI = Box::into_raw(state);

        let cls = wide(CLASS);
        let title = wide(TITLE);
        let hwnd = CreateWindowExW(
            0,
            cls.as_ptr(),
            title.as_ptr(),
            STYLE,
            rect.l,
            rect.t,
            rect.w(),
            rect.h(),
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
        ShowWindow(hwnd, if max { SW_SHOWMAXIMIZED } else { SW_SHOWNORMAL });
        SetForegroundWindow(hwnd);
    }
}

/// Tray "Settings…": the settings page used last, else General.
pub fn open_settings(a: &mut crate::app::App) {
    let last = ui().map_or_else(|| Page::from_key(&a.m.s.window_page), |u| Some(u.page));
    open(a, last.filter(|p| p.is_settings()).unwrap_or(Page::General));
}

/// The app's settings or theme changed (any source: this window, the file,
/// the tray, Windows): follow the theme, pick up Insights on/off, and take
/// settings changed elsewhere into the draft when nothing is pending here.
pub fn app_changed(dark: bool, a: &crate::app::App) {
    let Some(u) = ui() else { return };
    if u.hwnd.is_null() {
        return;
    }
    if !u.dirty {
        merge(&mut u.draft, &a.m.s);
        u.draft.order = full_order(&u.draft.order);
        u.draft.mode = a.m.s.mode;
        u.autostart = crate::autostart::enabled();
    }
    if u.dark != dark || u.theme != a.m.s.theme {
        u.dark = dark;
        u.theme = a.m.s.theme;
        apply_theme(u.hwnd);
    }
    if let Some(u) = ui() {
        u.layout();
        invalidate(u.hwnd);
    }
    sync_insights(a, true);
}

/// Called on every app tick. Nearly free unless the window is open on an
/// Insights page or Overview; then the panels are rebuilt at most every 2 s
/// and the window repaints only when something visible changed.
pub fn insights_tick(a: &crate::app::App) {
    let Some(u) = ui() else { return };
    if u.hwnd.is_null() || !u.page.shows_insights() {
        return;
    }
    sync_insights(a, false);
}

/// Close the window if it is open (app exit): applies pending edits and
/// saves the placement.
pub fn close() {
    if let Some(u) = ui() {
        if !u.hwnd.is_null() {
            unsafe { SendMessageW(u.hwnd, WM_CLOSE, 0, 0) };
        }
    }
}

/// Remember size, position, maximized and the page for next time.
fn save_placement(hwnd: HWND) {
    let Some(u) = ui() else { return };
    let max = unsafe { IsZoomed(hwnd) != 0 } || (unsafe { IsIconic(hwnd) != 0 } && u.maximized);
    let n = u.normal;
    let place = (n.w() > 0 && n.h() > 0).then_some(Placement { x: n.l, y: n.t, w: n.w(), h: n.h(), max });
    let page = u.page.key();
    let Some(a) = app() else { return };
    if a.m.s.window != place || a.m.s.window_page != page {
        a.m.s.window = place.or(a.m.s.window);
        a.m.s.window_page = page.to_string();
        a.save_settings();
    }
}

/// Track the normal (restored) rectangle for `save_placement`.
fn note_rect(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd) != 0 || IsZoomed(hwnd) != 0 {
            return;
        }
        if let (Some(u), Some(r)) = (ui(), window_rect(hwnd)) {
            u.normal = r;
        }
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


const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_MOUSEHOVER: u32 = 0x02A1;
const XBUTTON1: u16 = 1;

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let Some(u) = ui() else { return -1 };
            u.hwnd = hwnd;
            let dpi = GetDpiForWindow(hwnd);
            if dpi != 0 && dpi != u.dpi {
                u.set_dpi(dpi);
            }
            u.mica = enable_mica(hwnd);
            apply_theme(hwnd);
            if let Some(u) = ui() {
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                u.resized(rc.right, rc.bottom);
            }
            READY.store(true, Relaxed);
            0
        }
        WM_SIZE => {
            if let Some(u) = ui() {
                match w as u32 {
                    SIZE_MAXIMIZED => u.maximized = true,
                    SIZE_RESTORED => u.maximized = false,
                    _ => {}
                }
                if w as u32 != SIZE_MINIMIZED {
                    u.resized((l & 0xFFFF) as i32, ((l >> 16) & 0xFFFF) as i32);
                }
            }
            note_rect(hwnd);
            invalidate(hwnd);
            0
        }
        WM_MOVE => {
            note_rect(hwnd);
            0
        }
        WM_GETMINMAXINFO => {
            let dpi = GetDpiForWindow(hwnd).max(96);
            let sc = |v: i32| (v * dpi as i32 + 48) / 96;
            let mut rc = RECT { left: 0, top: 0, right: sc(MIN_W), bottom: sc(MIN_H) };
            AdjustWindowRectExForDpi(&mut rc, STYLE, 0, 0, dpi);
            let mm = &mut *(l as *mut MINMAXINFO);
            mm.ptMinTrackSize = POINT { x: rc.right - rc.left, y: rc.bottom - rc.top };
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            if let Some(u) = ui() {
                if !u.painted {
                    u.painted = true;
                    diag(&format!(
                        "main window: first paint {}x{} mica={} dark={} dpi={} page={}",
                        u.cw,
                        u.ch,
                        u.mica,
                        u.dark,
                        u.dpi,
                        u.page.key()
                    ));
                }
                u.paint(dc);
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_MOUSEMOVE => {
            on_mouse_move(hwnd, get_x(l), get_y(l));
            0
        }
        WM_MOUSEHOVER => {
            on_mouse_hover(hwnd);
            0
        }
        WM_MOUSELEAVE => {
            if let Some(u) = ui() {
                u.tracking = false;
                if u.drag == Drag::None && (u.hover.is_some() || u.sb_hover || u.chart_hover.is_some() || u.tip.is_some()) {
                    u.hover = None;
                    u.sb_hover = false;
                    u.chart_hover = None;
                    u.tip = None;
                    invalidate(hwnd);
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            on_button_down(hwnd, get_x(l), get_y(l));
            0
        }
        WM_LBUTTONUP => {
            on_button_up(hwnd, get_x(l), get_y(l));
            0
        }
        WM_XBUTTONUP => {
            // Mouse back button.
            if ((w >> 16) & 0xFFFF) as u16 == XBUTTON1 {
                go_back();
            }
            1
        }
        WM_CAPTURECHANGED => {
            // Lost the mouse (Alt+Tab, a menu, ...): end any press or drag.
            if let Some(u) = ui() {
                if u.pressed.is_some() || u.drag != Drag::None {
                    u.pressed = None;
                    u.drag = Drag::None;
                    KillTimer(hwnd, T_REPEAT);
                    invalidate(hwnd);
                }
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((w >> 16) & 0xFFFF) as u16 as i16 as i32;
            on_wheel(hwnd, delta, get_x(l), get_y(l));
            0
        }
        WM_KEYDOWN => {
            on_key(hwnd, w as u16);
            0
        }
        WM_SYSKEYDOWN => {
            // Alt+Left: back. Everything else (Alt+F4, Alt+Space) is the system's.
            if w as u16 == VK_LEFT {
                go_back();
                return 0;
            }
            DefWindowProcW(hwnd, msg, w, l)
        }
        WM_TIMER => {
            match w {
                T_ANIM => {
                    if let Some(u) = ui() {
                        u.anim_tick();
                    }
                }
                T_REPEAT => on_repeat(hwnd),
                T_LIVE => {
                    KillTimer(hwnd, T_LIVE);
                    commit(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_SETTINGCHANGE => {
            if let Some(u) = ui() {
                u.animate = animations_on();
            }
            // The user switched Windows dark/light mode or accent colour.
            if setting_name(l) == "ImmersiveColorSet" {
                if let Some(u) = ui() {
                    u.dark = effective_dark(u.theme);
                    apply_theme(hwnd);
                }
            }
            DefWindowProcW(hwnd, msg, w, l)
        }
        WM_DWMCOLORIZATIONCOLORCHANGED => {
            apply_theme(hwnd);
            DefWindowProcW(hwnd, msg, w, l)
        }
        WM_DPICHANGED => {
            let dpi = ((w >> 16) & 0xFFFF) as u32;
            let r = &*(l as *const RECT);
            if let Some(u) = ui() {
                u.set_dpi(dpi);
            }
            // WM_SIZE re-lays out with the new fonts.
            SetWindowPos(hwnd, std::ptr::null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            if let Some(u) = ui() {
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                u.resized(rc.right, rc.bottom);
            }
            invalidate(hwnd);
            0
        }
        WM_CLOSE => {
            // Title-bar X / Alt+F4: apply anything pending, remember where we were, close.
            flush(hwnd);
            save_placement(hwnd);
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, T_LIVE);
            KillTimer(hwnd, T_ANIM);
            KillTimer(hwnd, T_REPEAT);
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

#[cfg(test)]
mod tests {
    #[test]
    fn class_differs_from_the_app_window() {
        assert_ne!(super::CLASS, crate::app::MAIN_CLASS);
    }
}
