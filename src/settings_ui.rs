//! Settings window in the style of Windows 11 Settings: a navigation pane on
//! the left, a scrollable page of translucent "setting cards" on the right and
//! a "Done" footer (changes apply live), all on the Mica backdrop (Windows 11).
//!
//! There are no child windows. The whole client area is one surface rendered
//! into a `render::Canvas` (premultiplied BGRA) and copied to the window in a
//! single `BitBlt`, so nothing flickers and nothing shows up as white blocks
//! while scrolling. Every control (toggle switch, drop-down, slider, number
//! stepper, buttons, colour swatches) is painted and hit-tested here.
//!
//! With Mica the background is painted with alpha 0 and the frame is extended
//! into the client area, so DWM shows the backdrop through it; cards and hover
//! fills are translucent white / black like the system's own. On Windows 10
//! (or if the backdrop cannot be enabled) opaque fallback colours are used.
//!
//! The window edits a *copy* of the settings; each change is merged into
//! the live settings via `App::apply_new_settings` ~150 ms later (debounced).
//!
//! Re-entrancy rules (single UI thread, raw Win32):
//! * `ui()` borrows are short; none is used after a call that runs a nested
//!   message loop (`TrackPopupMenuEx`, `ChooseColorW`, `apply_new_settings`,
//!   `DestroyWindow`): such calls are made from free functions that fetch
//!   `ui()` again afterwards.
//! * The layout (controls and cards of the current page) is rebuilt from the
//!   draft after every change; it is a few dozen rectangles.

use crate::app::{app, default_accent, effective_dark};
use crate::fmt::SpeedUnit;
use crate::popup::style_window;
use crate::render::{create_font, measure_dc, ui_face, Canvas, Rgba};
use crate::settings::{Metric, Settings, Theme};
use crate::util::*;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmGetColorizationColor, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::Dialogs::*;
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL, VK_DOWN, VK_END, VK_ESCAPE, VK_F4,
    VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "ResourceMonitor.Settings";

// ------------------------------------------------------------------ layout
// All values are device-independent pixels (96 dpi); `Ui::sc` scales them.

const CLIENT_W: i32 = 800;
const CLIENT_H: i32 = 600;
const NAV_W: i32 = 220;
/// Top of the first navigation item (the app title sits above it).
const NAV_TOP: i32 = 64;
const NAV_ITEM_H: i32 = 36;
const NAV_GAP: i32 = 4;
/// Page title strip above the scrolling cards.
const HEADER_H: i32 = 76;
const FOOTER_H: i32 = 64;
const PAGE_PAD_L: i32 = 8;
/// Right of the cards; the overlay scroll bar lives in this gutter.
const PAGE_PAD_R: i32 = 24;
const CARD_PAD: i32 = 16;
const ROW_H: i32 = 64;
const ROW_GAP: i32 = 4;
const HEAD_TOP: i32 = 24;
const HEAD_H: i32 = 28;
const NOTE_H: i32 = 36;
const CTL_H: i32 = 32;
const CELL_GAP: i32 = 8;
const WHEEL_STEP: i32 = 48;
const BTN_W: i32 = 112;
const STEPPER_W: i32 = 132;

const T_SCROLL: usize = 1;
const T_REPEAT: usize = 2;
/// Debounced live apply: changes take effect ~150 ms after the last edit.
const T_LIVE: usize = 3;

// ------------------------------------------------------------------ ids

const IDOK: i32 = 1;
const IDCANCEL: i32 = 2;
const ID_APPLY: i32 = 3;
/// + page index.
const ID_NAV: i32 = 40;

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

const PAGES: [&str; 5] = ["General", "Metrics", "Colours", "Alerts", "Data"];
/// Segoe Fluent Icons / Segoe MDL2 Assets glyphs of the navigation items.
const NAV_GLYPHS: [char; 5] = ['\u{E713}', '\u{E9D9}', '\u{E790}', '\u{EA8F}', '\u{E8F1}'];
const G_CHEVRON_DOWN: char = '\u{E70D}';
const G_CHEVRON_UP: char = '\u{E70E}';
const G_MINUS: char = '\u{E738}';
const G_PLUS: char = '\u{E710}';

const INTERVALS: [(&str, u32); 4] = [("0.5 seconds", 500), ("1 second", 1000), ("2 seconds", 2000), ("5 seconds", 5000)];
const THEMES: [(&str, Theme); 3] = [("Match Windows", Theme::Auto), ("Dark", Theme::Dark), ("Light", Theme::Light)];
/// Offered widget fonts; the first means "default" (empty setting).
const FONTS: [&str; 5] = ["Default", "Segoe UI", "Cascadia Mono", "Consolas", "Bahnschrift"];

// ------------------------------------------------------------------ state

static READY: AtomicBool = AtomicBool::new(false);
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
    bg: Rgba,
    card: Rgba,
    card_border: Rgba,
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
    footer: Rgba,
    line: Rgba,
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
                bg: if mica { Rgba(0, 0, 0, 0) } else { hx(0x202020) },
                card: if mica { wa(0x0D) } else { hx(0x2B2B2B) },
                card_border: if mica { wa(0x0F) } else { ka(0x33) },
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
                footer: ka(0x20),
                line: wa(0x0F),
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
                accent,
                accent_hover,
                accent_press,
                on_accent,
            }
        } else {
            let f9 = |a: u8| Rgba(0xF9, 0xF9, 0xF9, a);
            Pal {
                bg: if mica { Rgba(0, 0, 0, 0) } else { hx(0xF3F3F3) },
                card: if mica { wa(0xB3) } else { hx(0xFBFBFB) },
                card_border: ka(0x0F),
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
                footer: wa(0x80),
                line: ka(0x0F),
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
                accent,
                accent_hover,
                accent_press,
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
    semi: HFONT,
    title: HFONT,
    app: HFONT,
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
    /// Grayscale anti-aliased fonts (the canvas needs a coverage mask).
    fn new(dpi: u32) -> Fonts {
        let sc = |v: i32| ((v * dpi as i32 + 48) / 96).max(1);
        let text = ui_face();
        let display = pick_face("Segoe UI Variable Display", text);
        let icons = pick_face("Segoe Fluent Icons", "Segoe MDL2 Assets");
        Fonts {
            body: create_font(sc(14), 400, text, true),
            small: create_font(sc(12), 400, text, true),
            semi: create_font(sc(14), 600, text, true),
            title: create_font(sc(28), 600, display, true),
            app: create_font(sc(18), 600, display, true),
            icon: create_font(sc(16), 400, icons, true),
            icon_sm: create_font(sc(12), 400, icons, true),
        }
    }

    fn free(&self) {
        for f in [self.body, self.small, self.semi, self.title, self.app, self.icon, self.icon_sm] {
            unsafe { DeleteObject(f) };
        }
    }
}

// ------------------------------------------------------------------ model

/// What a control is; drives painting, hit-testing and input.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Nav(usize),
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

struct Control {
    id: i32,
    kind: Kind,
    /// Page controls: x in client, y in content coordinates. Others: client.
    rect: Rect,
    in_page: bool,
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
    unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
}

// ------------------------------------------------------------------ ui

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
    page: usize,
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
    /// Auto-repeat ticks of the stepper button held down.
    repeat_n: u32,
    /// Drop-down whose menu is open (drawn pressed).
    open_dd: Option<i32>,
    cv: Canvas,
    /// The copy being edited.
    draft: Settings,
    /// Adapter aliases offered in the drop-down (index + 1; 0 = automatic).
    adapters: Vec<String>,
    adapter_labels: Vec<String>,
    autostart: bool,
    /// Something changed since the last apply (enables Apply).
    dirty: bool,
    /// Custom colours of the colour picker, kept for the session.
    cust: [COLORREF; 16],
}

impl Ui {
    fn new(dpi: u32, dark: bool, theme: Theme, draft: Settings, adapters: Vec<String>, labels: Vec<String>) -> Ui {
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
            page: 0,
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
            repeat_n: 0,
            open_dd: None,
            cv: Canvas::new(),
            draft,
            adapters,
            adapter_labels: labels,
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
            ID_STACKED => d.stacked,
            ID_AUTOSTACK => d.auto_stack,
            ID_FULLSCREEN => d.hide_fullscreen,
            ID_CLICKTHRU => d.click_through,
            ID_AUTOUPDATE => d.auto_update,
            ID_AUTOSTART => self.autostart,
            ID_ALERTS => d.alerts,
            ID_A_CAP => d.alert_cap,
            ID_WINACCENT => d.windows_accent,
            _ => self.metric_at(id - ID_MTOG).is_some_and(|m| d.show[m.idx()]),
        }
    }

    fn flag_slot(&mut self, id: i32) -> Option<&mut bool> {
        let m = self.metric_at(id - ID_MTOG);
        let d = &mut self.draft;
        Some(match id {
            ID_SPARK => &mut d.sparklines,
            ID_LABELS => &mut d.labels,
            ID_STACKED => &mut d.stacked,
            ID_AUTOSTACK => &mut d.auto_stack,
            ID_FULLSCREEN => &mut d.hide_fullscreen,
            ID_CLICKTHRU => &mut d.click_through,
            ID_AUTOUPDATE => &mut d.auto_update,
            ID_AUTOSTART => &mut self.autostart,
            ID_ALERTS => &mut d.alerts,
            ID_A_CAP => &mut d.alert_cap,
            ID_WINACCENT => &mut d.windows_accent,
            _ => &mut d.show[m?.idx()],
        })
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

    fn page_defs(&self, page: usize) -> Vec<Def> {
        let d = &self.draft;
        match page {
            0 => vec![
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
                Def::Heading("Behaviour"),
                toggle_card("Sparklines", "Draw a small history graph behind each value", ID_SPARK),
                toggle_card("Metric labels", "Show the name next to each value", ID_LABELS),
                toggle_card("Stacked rows", "Two metrics per column on the taskbar", ID_STACKED),
                toggle_card("Auto-stack when crowded", "Switch to stacked rows when the widget would not fit", ID_AUTOSTACK),
                toggle_card("Hide in full-screen apps", "Keep the widget out of the way of games and videos", ID_FULLSCREEN),
                toggle_card("Click-through", "Mouse clicks pass through the widget to the taskbar", ID_CLICKTHRU),
                toggle_card("Update automatically", "Install new versions when they are released", ID_AUTOUPDATE),
                toggle_card("Start with Windows", "Launch Resource Monitor when you sign in", ID_AUTOSTART),
            ],
            1 => {
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
            2 => {
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
            3 => {
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
                ]
            }
            _ => vec![
                self.number_card("Monthly data cap", "In gigabytes. 0 turns the cap off", ID_CAP, "GB"),
                card("Billing cycle resets on day", "Data is counted since the last reset day", vec![cell(ID_DAY, Kind::Dropdown, 120)]),
                card("Network adapter", "Which adapter the data counters follow", vec![cell(ID_ADAPTER, Kind::Dropdown, 250)]),
                card(
                    "Data usage report",
                    "Daily and monthly totals",
                    vec![cell(ID_REPORT, Kind::Button("Open data usage report", false), 0)],
                ),
            ],
        }
    }

    // -------------------------------------------------------------- layout

    fn view(&self) -> (i32, i32) {
        let t = self.sc(HEADER_H);
        (t, (self.ch - self.sc(FOOTER_H)).max(t + 1))
    }

    fn max_scroll(&self) -> i32 {
        let (vt, vb) = self.view();
        (self.content_h - (vb - vt)).max(0)
    }

    fn scroll_px(&self) -> i32 {
        self.scroll.round() as i32
    }

    fn nav_rect(&self, i: usize) -> Rect {
        let t = self.sc(NAV_TOP) + i as i32 * (self.sc(NAV_ITEM_H) + self.sc(NAV_GAP));
        Rect { l: self.sc(8), t, r: self.sc(NAV_W - 8), b: t + self.sc(NAV_ITEM_H) }
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

    /// Rebuild the controls and cards of the current page from the draft.
    fn layout(&mut self) {
        let mut controls = Vec::new();
        let mut blocks = Vec::new();
        for i in 0..PAGES.len() {
            controls.push(Control { id: ID_NAV + i as i32, kind: Kind::Nav(i), rect: self.nav_rect(i), in_page: false, enabled: true });
        }
        let l = self.sc(NAV_W + PAGE_PAD_L);
        let r = (self.cw - self.sc(PAGE_PAD_R)).max(l + self.sc(240));
        let (row_h, ctl_h) = (self.sc(ROW_H), self.sc(CTL_H));
        let mut y = 0;
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
                    let h = self.sc(NOTE_H);
                    blocks.push(Block::Note(Rect { l, t: y, r, b: y + h }, t));
                    y += h;
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
                                row.push(Control { id: *id, kind: *kind, rect: rc, in_page: true, enabled: enabled && *e });
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

        // Footer buttons, right-aligned: OK, Cancel, Apply.
        let (bw, bh, gap) = (self.sc(BTN_W), self.sc(CTL_H), self.sc(8));
        let fh = self.sc(FOOTER_H);
        let by = self.ch - fh + (fh - bh) / 2;
        let right = self.cw - self.sc(24);
        // Changes apply live, so the footer only needs "Done" (like Windows Settings).
        let _ = gap;
        let x = right - bw;
        controls.push(Control {
            id: IDOK,
            kind: Kind::Button("Done", true),
            rect: Rect { l: x, t: by, r: x + bw, b: by + bh },
            in_page: false,
            enabled: true,
        });
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
        if c.in_page {
            shift(c.rect, self.view().0 - self.scroll_px())
        } else {
            c.rect
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

    fn hit(&self, x: i32, y: i32) -> Option<(i32, Part)> {
        let (vt, vb) = self.view();
        for c in &self.controls {
            if !c.enabled || (c.in_page && (y < vt || y >= vb)) {
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

    // -------------------------------------------------------------- scrolling

    fn scroll_to(&mut self, target: i32, animate: bool) {
        self.target = target.clamp(0, self.max_scroll());
        unsafe {
            if animate && (self.target as f32 - self.scroll).abs() > 1.0 {
                SetTimer(self.hwnd, T_SCROLL, 15, None);
            } else {
                self.scroll = self.target as f32;
                KillTimer(self.hwnd, T_SCROLL);
            }
        }
        invalidate(self.hwnd);
    }

    fn scroll_tick(&mut self) {
        let d = self.target as f32 - self.scroll;
        if d.abs() < 0.6 {
            self.scroll = self.target as f32;
            unsafe { KillTimer(self.hwnd, T_SCROLL) };
        } else {
            self.scroll += d * 0.3;
        }
        self.refresh_hover();
        invalidate(self.hwnd);
    }

    /// Scroll so a page control (plus a margin) is fully visible.
    fn ensure_visible(&mut self, id: i32) {
        let Some(c) = self.control(id) else { return };
        if !c.in_page {
            return;
        }
        let (vt, vb) = self.view();
        let m = self.sc(16);
        let (t, b) = (c.rect.t - m, c.rect.b + m);
        let s = self.target;
        if t < s {
            self.scroll_to(t, true);
        } else if b > s + (vb - vt) {
            self.scroll_to(b - (vb - vt), true);
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
        self.hover = if inside && self.drag == Drag::None { self.hit(pt.x, pt.y) } else { None };
    }

    fn show_page(&mut self, p: usize) {
        if p >= PAGES.len() {
            return;
        }
        if p != self.page {
            self.page = p;
            self.scroll = 0.0;
            self.target = 0;
            unsafe { KillTimer(self.hwnd, T_SCROLL) };
            if let Some(f) = self.focus {
                if self.control(f).is_some_and(|c| c.in_page || matches!(c.kind, Kind::Nav(_))) {
                    self.focus = None;
                }
            }
            self.layout();
            self.refresh_hover();
        }
        invalidate(self.hwnd);
    }

    /// Controls in keyboard order: the selected navigation item, the page,
    /// the footer.
    fn focus_order(&self) -> Vec<i32> {
        self.controls
            .iter()
            .filter(|c| c.enabled && !matches!(c.kind, Kind::Nav(i) if i != self.page))
            .map(|c| c.id)
            .collect()
    }

    fn move_focus(&mut self, dir: i32) {
        let order = self.focus_order();
        if order.is_empty() {
            return;
        }
        let n = order.len() as i32;
        let next = match self.focus.and_then(|f| order.iter().position(|x| *x == f)) {
            Some(i) => (i as i32 + dir).rem_euclid(n),
            None if dir > 0 => 0,
            None => n - 1,
        };
        let id = order[next as usize];
        self.focus = Some(id);
        self.focus_visible = true;
        self.ensure_visible(id);
        invalidate(self.hwnd);
    }

    // -------------------------------------------------------------- painting

    fn paint(&mut self, dc: HDC) {
        let mut cv = std::mem::replace(&mut self.cv, Canvas::new());
        cv.resize(self.cw, self.ch);
        self.draw(&mut cv);
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
        let dy = vt - self.scroll_px();
        let rad = self.sc(6) as f32;

        // Page content (scrolled), clipped afterwards by repainting the
        // header and footer strips.
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
                    if r.b < vt || r.t > vb {
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
        for c in self.controls.iter().filter(|c| c.in_page) {
            let r = self.client_rect(c);
            if r.b < vt || r.t > vb {
                continue;
            }
            self.draw_control(cv, c, r);
        }
        let px = self.sc(NAV_W);
        cv.clear_rect(Rect { l: px, t: 0, r: cv.w, b: vt }, p.bg);
        cv.clear_rect(Rect { l: px, t: vb, r: cv.w, b: cv.h }, p.bg);

        // Page title.
        let name = PAGES[self.page];
        let th = cv.measure(f.title, name).1;
        cv.text(f.title, name, self.sc(NAV_W + PAGE_PAD_L + 2), (vt - th) / 2 + self.sc(2), p.fg);

        // Footer.
        cv.fill_rect(Rect { l: px, t: vb, r: cv.w, b: cv.h }, p.footer);
        cv.fill_rect(Rect { l: px, t: vb, r: cv.w, b: vb + 1 }, p.line);

        // Hint left of "Done".
        {
            let hint = "Changes are applied as you make them";
            let (_, th) = cv.measure(f.small, hint);
            cv.text(f.small, hint, px + self.sc(PAGE_PAD_L), vb + (cv.h - vb - th) / 2, p.dim);
        }

        // App title, navigation, footer buttons.
        cv.text(f.app, "Settings", self.sc(20), self.sc(18), p.fg);
        for c in self.controls.iter().filter(|c| !c.in_page) {
            self.draw_control(cv, c, c.rect);
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
            Kind::Nav(i) => {
                let sel = i == self.page;
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
                if sel {
                    let (pw, ph) = (self.sc(3), self.sc(16));
                    let t = cy - ph / 2;
                    cv.fill_round(Rect { l: r.l, t, r: r.l + pw, b: t + ph }, pw as f32 / 2.0, p.accent);
                }
                glyph(cv, f.icon, NAV_GLYPHS[i], r.l + self.sc(24), cy, p.fg);
                text_mid(cv, f.body, PAGES[i], r.l + self.sc(48), cy, p.fg);
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

/// Apply anything still pending (closing the window, pressing Done).
fn flush(hwnd: HWND) {
    unsafe { KillTimer(hwnd, T_LIVE) };
    if ui().is_some_and(|u| u.dirty) {
        commit(hwnd);
    }
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
        if u.focus == Some(ID_APPLY) {
            u.focus = Some(IDOK);
        }
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

fn ok(hwnd: HWND) {
    flush(hwnd);
    unsafe { DestroyWindow(hwnd) };
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
    if cmd > 0 && u.dd_select(id, cmd as usize - 1) {
        changed(hwnd);
    } else {
        invalidate(hwnd);
    }
}

/// Click / Space / Enter on a control.
fn activate(hwnd: HWND, id: i32) {
    let Some(u) = ui() else { return };
    let Some(kind) = u.control(id).filter(|c| c.enabled).map(|c| c.kind) else { return };
    let metrics = Metric::ALL.len() as i32;
    match kind {
        Kind::Nav(i) => {
            u.show_page(i);
            if u.focus.is_some() || u.focus_visible {
                u.focus = Some(ID_NAV + i as i32);
            }
            invalidate(hwnd);
        }
        Kind::Toggle => {
            if let Some(v) = u.flag_slot(id) {
                *v = !*v;
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
            IDOK => ok(hwnd),
            IDCANCEL => unsafe {
                DestroyWindow(hwnd);
            },
            ID_APPLY => {
                commit(hwnd);
            }
            ID_RESETALL => {
                u.draft.colors.clear();
                changed(hwnd);
            }
            ID_REPORT => {
                if let Some(a) = app() {
                    crate::report::open(a);
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
    match key {
        VK_TAB if ctrl => {
            let n = PAGES.len();
            let p = if shift { (u.page + n - 1) % n } else { (u.page + 1) % n };
            let nav = matches!(focus, Some((_, Kind::Nav(_))));
            u.show_page(p);
            if nav {
                u.focus = Some(ID_NAV + p as i32);
            }
        }
        VK_TAB => u.move_focus(if shift { -1 } else { 1 }),
        VK_ESCAPE => {
            flush(hwnd);
            unsafe { DestroyWindow(hwnd) };
        }
        VK_RETURN => match focus {
            Some((id, Kind::Button(..) | Kind::Link(_) | Kind::Swatch(_) | Kind::Glyph(_) | Kind::Dropdown)) => {
                u.focus_visible = true;
                activate(hwnd, id);
            }
            _ => ok(hwnd),
        },
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
                Some((_, Kind::Nav(cur))) => {
                    let n = PAGES.len();
                    let next = match key {
                        VK_UP | VK_LEFT => cur.saturating_sub(1),
                        VK_DOWN | VK_RIGHT => (cur + 1).min(n - 1),
                        VK_HOME | VK_PRIOR => 0,
                        _ => n - 1,
                    };
                    u.focus_visible = true;
                    u.show_page(next);
                    u.focus = Some(ID_NAV + next as i32);
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

fn on_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let Some(u) = ui() else { return };
    if !u.tracking {
        let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
        u.tracking = unsafe { TrackMouseEvent(&mut t) } != 0;
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
    let sb = u.scrollbar().is_some_and(|(t, _)| t.contains(x, y));
    if hover != u.hover || sb != u.sb_hover {
        u.hover = hover;
        u.sb_hover = sb;
        invalidate(hwnd);
    }
}

fn on_button_down(hwnd: HWND, x: i32, y: i32) {
    let Some(u) = ui() else { return };
    u.focus_visible = false;
    // Scroll bar first: it overlays the right edge of the page.
    if let Some((track, thumb)) = u.scrollbar() {
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
    u.sb_hover = u.scrollbar().is_some_and(|(t, _)| t.contains(x, y));
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
fn enable_mica(hwnd: HWND) -> bool {
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
        RegisterClassExW(&wc);
    }
}

const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;

/// Size the window for `dpi` (never larger than the work area) and centre it.
fn fit(hwnd: HWND, dpi: u32) {
    unsafe {
        let sc = |v: i32| (v * dpi as i32 + 48) / 96;
        let mut rc = RECT { left: 0, top: 0, right: sc(CLIENT_W), bottom: sc(CLIENT_H) };
        AdjustWindowRectExForDpi(&mut rc, STYLE, 0, 0, dpi);
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
        let state = Box::new(Ui::new(dpi, a.m.dark, a.m.s.theme, draft, adapters, labels));
        free_ui();
        UI = Box::into_raw(state);

        let cls = wide(CLASS);
        let title = wide("Resource Monitor Settings");
        let hwnd = CreateWindowExW(
            0,
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
        fit(hwnd, ui().map_or(96, |u| u.dpi));
        ShowWindow(hwnd, SW_SHOWNORMAL);
        SetForegroundWindow(hwnd);
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
                u.cw = rc.right;
                u.ch = rc.bottom;
                u.layout();
            }
            READY.store(true, Relaxed);
            0
        }
        WM_SIZE => {
            if let Some(u) = ui() {
                u.cw = (l & 0xFFFF) as i32;
                u.ch = ((l >> 16) & 0xFFFF) as i32;
                u.layout();
            }
            invalidate(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            if let Some(u) = ui() {
                u.paint(dc);
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_MOUSEMOVE => {
            on_mouse_move(hwnd, get_x(l), get_y(l));
            0
        }
        WM_MOUSELEAVE => {
            if let Some(u) = ui() {
                u.tracking = false;
                if u.drag == Drag::None && (u.hover.is_some() || u.sb_hover) {
                    u.hover = None;
                    u.sb_hover = false;
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
        WM_TIMER => {
            match w {
                T_SCROLL => {
                    if let Some(u) = ui() {
                        u.scroll_tick();
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
                u.layout();
            }
            invalidate(hwnd);
            0
        }
        WM_CLOSE => {
            // Title-bar X / Alt+F4: apply anything pending, then close.
            flush(hwnd);
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, T_LIVE);
            KillTimer(hwnd, T_SCROLL);
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
