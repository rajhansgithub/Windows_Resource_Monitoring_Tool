//! Settings window: a native Windows 11 style dialog built from standard
//! common controls (tab, trackbar, list view, combo, up-down, buttons).
//!
//! The window edits a *copy* of the settings. OK / Apply merge that copy into
//! the live settings via `App::apply_new_settings`; Cancel throws it away.
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
use crate::render::{create_font, ui_face};
use crate::settings::{Metric, Settings, Theme};
use crate::util::*;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::{DwmGetColorizationColor, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::Dialogs::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "ResourceMonitor.Settings";

// ------------------------------------------------------------------ layout
// All values are device-independent pixels (96 dpi); `Ui::sc` scales them.

const CLIENT_W: i32 = 540;
const CLIENT_H: i32 = 504;
const M: i32 = 12; // outer margin
const TAB_H: i32 = 440;
const GX: i32 = 28; // group box left / width inside the tab
const GW: i32 = 484;
const LBL_X: i32 = 40; // label column inside a group
const CTL_X: i32 = 198; // control column on the General page
const CTL_W: i32 = 290;
const ROW: i32 = 24; // single-line control height
const BTN_Y: i32 = M + TAB_H + M;

// ------------------------------------------------------------------ ids

const IDOK: i32 = 1;
const IDCANCEL: i32 = 2;
const ID_APPLY: i32 = 3;
const ID_TAB: i32 = 50;

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
const ID_SCALE_VAL: i32 = 120;
const ID_OPACITY_VAL: i32 = 121;

const ID_LIST: i32 = 200;
const ID_UP: i32 = 201;
const ID_DOWN: i32 = 202;

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

/// Up-down control for edit `id` has id `id + SPIN`; its caption label
/// `id + LABEL`; its unit label `id + UNIT`.
const SPIN: i32 = 1000;
const LABEL: i32 = 2000;
const UNIT: i32 = 3000;

const PAGES: [&str; 5] = ["General", "Metrics", "Colours", "Alerts", "Data"];
const P_GENERAL: i32 = 0;
const P_METRICS: i32 = 1;
const P_COLOURS: i32 = 2;
const P_ALERTS: i32 = 3;
const P_DATA: i32 = 4;

const INTERVALS: [(&str, u32); 4] = [("0.5 seconds", 500), ("1 second", 1000), ("2 seconds", 2000), ("5 seconds", 5000)];
const THEMES: [(&str, Theme); 3] = [("Match Windows", Theme::Auto), ("Dark", Theme::Dark), ("Light", Theme::Light)];
const FONTS: [&str; 5] = ["Default (Segoe UI Variable)", "Segoe UI", "Cascadia Mono", "Consolas", "Bahnschrift"];
/// Alert rows that depend on the master "alerts" checkbox.
const ALERT_IDS: [i32; 6] = [ID_A_CPU, ID_A_RAM, ID_A_SECS, ID_A_DISK, ID_A_BAT, ID_A_CAP];
/// Checkboxes whose clicks mark the window as changed.
const CHECK_IDS: [i32; 11] = [
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

// Static / misc styles missing from windows-sys.
const SS_LEFT_CENTER: u32 = 0x0200 | 0x0080; // SS_CENTERIMAGE | SS_NOPREFIX
const SS_WRAP: u32 = 0x0080; // SS_NOPREFIX (multi-line wraps by default)
const ODS_SELECTED: u32 = 0x0001;
const ODS_FOCUS: u32 = 0x0010;
const DC_HASDEFID: u32 = 0x534B;
const TBM_GETPOS: u32 = 0x0400;
const CBS_AUTOHSCROLL: u32 = 0x0040;

const DARK_BG: u32 = 0x0020_2020; // COLORREF (r = g = b)
const DARK_CTL: u32 = 0x002B_2B2B;
const DARK_FG: u32 = 0x00F0_F0F0;

// ------------------------------------------------------------------ state

static READY: AtomicBool = AtomicBool::new(false);
static LOADING: AtomicBool = AtomicBool::new(false);
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
            DeleteObject(u.font);
            DeleteObject(u.br_bg);
            DeleteObject(u.br_ctl);
        }
    }
    READY.store(false, Relaxed);
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Group,
    Check,
    Edit,
    Spin,
    Combo,
    Slider,
    Button,
    List,
    Tab,
}

struct Item {
    hwnd: HWND,
    kind: Kind,
    /// Page this control lives on; -1 = always visible.
    page: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    /// Extra height for a combo box's drop-down list.
    drop: i32,
}

struct Ui {
    parent: HWND,
    hinst: HINSTANCE,
    items: Vec<Item>,
    font: HFONT,
    dpi: u32,
    dark: bool,
    br_bg: HBRUSH,
    br_ctl: HBRUSH,
    page: i32,
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

fn ctl(parent: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(parent, id) }
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

fn is_checked(h: HWND) -> bool {
    send(h, BM_GETCHECK, 0, 0) == 1
}

fn set_checked(h: HWND, on: bool) {
    send(h, BM_SETCHECK, on as usize, 0);
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

impl Ui {
    fn sc(&self, v: i32) -> i32 {
        (v * self.dpi as i32 + 48) / 96
    }

    fn make_font(dpi: u32) -> HFONT {
        // 9 pt at the window's dpi.
        let px = ((9 * dpi + 36) / 72) as i32;
        create_font(px, 400, ui_face(), false)
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

    // ---------------------------------------------------------------- build

    #[allow(clippy::too_many_arguments)]
    fn mk(
        &mut self,
        kind: Kind,
        class: &str,
        text: &str,
        style: u32,
        ex: u32,
        id: i32,
        page: i32,
        (x, y, w, h): (i32, i32, i32, i32),
        drop: i32,
    ) -> HWND {
        let vis = page < 0 || page == self.page;
        let cls = wide(class);
        let txt = wide(text);
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                cls.as_ptr(),
                txt.as_ptr(),
                WS_CHILD | style | if vis { WS_VISIBLE } else { 0 },
                self.sc(x),
                self.sc(y),
                self.sc(w),
                self.sc(h + drop),
                self.parent,
                id as usize as HMENU,
                self.hinst,
                std::ptr::null(),
            )
        };
        if !hwnd.is_null() {
            send(hwnd, WM_SETFONT, self.font as usize, 1);
            self.items.push(Item { hwnd, kind, page, x, y, w, h, drop });
        }
        hwnd
    }

    fn label(&mut self, page: i32, id: i32, text: &str, r: (i32, i32, i32, i32)) {
        self.mk(Kind::Text, "STATIC", text, SS_LEFT_CENTER, 0, id, page, r, 0);
    }

    fn hint(&mut self, page: i32, text: &str, r: (i32, i32, i32, i32)) {
        self.mk(Kind::Text, "STATIC", text, SS_WRAP, 0, 0, page, r, 0);
    }

    fn group(&mut self, page: i32, text: &str, y: i32, h: i32) {
        self.mk(Kind::Group, "BUTTON", text, BS_GROUPBOX as u32, 0, 0, page, (GX, y, GW, h), 0);
    }

    fn check(&mut self, page: i32, id: i32, text: &str, x: i32, y: i32, w: i32) {
        self.mk(Kind::Check, "BUTTON", text, BS_AUTOCHECKBOX as u32 | WS_TABSTOP, 0, id, page, (x, y, w, 22), 0);
    }

    fn button(&mut self, page: i32, id: i32, text: &str, r: (i32, i32, i32, i32), kind_style: i32) {
        self.mk(Kind::Button, "BUTTON", text, kind_style as u32 | WS_TABSTOP, 0, id, page, r, 0);
    }

    fn combo(&mut self, page: i32, id: i32, x: i32, y: i32, w: i32, editable: bool, rows: i32) {
        let style = if editable { CBS_DROPDOWN as u32 | CBS_AUTOHSCROLL } else { CBS_DROPDOWNLIST as u32 };
        self.mk(Kind::Combo, "COMBOBOX", "", style | WS_TABSTOP | WS_VSCROLL, 0, id, page, (x, y, w, ROW), rows.min(10) * 22 + 6);
    }

    fn slider(&mut self, page: i32, id: i32, x: i32, y: i32, w: i32, lo: i32, hi: i32, line: usize, pg: usize) {
        let h = self.mk(
            Kind::Slider,
            "msctls_trackbar32",
            "",
            TBS_HORZ | TBS_NOTICKS | WS_TABSTOP,
            0,
            id,
            page,
            (x, y - 2, w, 28),
            0,
        );
        send(h, TBM_SETRANGE, 1, ((hi << 16) | lo) as isize);
        send(h, TBM_SETLINESIZE, 0, line as isize);
        send(h, TBM_SETPAGESIZE, 0, pg as isize);
    }

    fn edit(&mut self, page: i32, id: i32, x: i32, y: i32, w: i32, numeric: bool) -> HWND {
        let style = ES_AUTOHSCROLL as u32 | if numeric { ES_NUMBER as u32 } else { 0 } | WS_TABSTOP;
        self.mk(Kind::Edit, "EDIT", "", style, WS_EX_CLIENTEDGE, id, page, (x, y, w, ROW), 0)
    }

    /// Numeric edit with an up-down arrow pair, a caption and a unit label.
    #[allow(clippy::too_many_arguments)]
    fn number_row(&mut self, page: i32, id: i32, caption: &str, unit: &str, y: i32, lo: i32, hi: i32) {
        self.label(page, id + LABEL, caption, (LBL_X, y, 250, ROW));
        let e = self.edit(page, id, 296, y, 72, true);
        let u = self.mk(
            Kind::Spin,
            "msctls_updown32",
            "",
            UDS_ALIGNRIGHT | UDS_SETBUDDYINT | UDS_ARROWKEYS | UDS_NOTHOUSANDS,
            0,
            id + SPIN,
            page,
            (0, 0, 0, 0),
            0,
        );
        send(u, UDM_SETBUDDY, e as usize, 0);
        send(u, UDM_SETRANGE32, lo as usize, hi as isize);
        if !unit.is_empty() {
            self.label(page, id + UNIT, unit, (376, y, 80, ROW));
        }
    }

    fn build(&mut self) {
        let tab = self.mk(Kind::Tab, "SysTabControl32", "", WS_TABSTOP | WS_CLIPSIBLINGS, 0, ID_TAB, -1, (M, M, CLIENT_W - 2 * M, TAB_H), 0);
        for (i, n) in PAGES.iter().enumerate() {
            let mut txt = wide(n);
            let mut it: TCITEMW = unsafe { std::mem::zeroed() };
            it.mask = TCIF_TEXT;
            it.pszText = txt.as_mut_ptr();
            send(tab, TCM_INSERTITEMW, i, &it as *const _ as isize);
        }
        self.build_general();
        self.build_metrics();
        self.build_colours();
        self.build_alerts();
        self.build_data();

        let by = BTN_Y;
        let bw = 88;
        let right = CLIENT_W - M;
        self.button(-1, IDOK, "OK", (right - 3 * bw - 16, by, bw, 28), BS_DEFPUSHBUTTON);
        self.button(-1, IDCANCEL, "Cancel", (right - 2 * bw - 8, by, bw, 28), BS_PUSHBUTTON);
        self.button(-1, ID_APPLY, "Apply", (right - bw, by, bw, 28), BS_PUSHBUTTON);
    }

    fn build_general(&mut self) {
        let p = P_GENERAL;
        self.group(p, "Display", 56, 228);
        let rows = [
            (ID_INTERVAL, "Update interval"),
            (ID_UNIT, "Network speed unit"),
            (ID_SCALE, "Text size"),
            (ID_THEME, "Theme"),
            (ID_OPACITY, "Background opacity"),
            (ID_FONT, "Font"),
        ];
        for (i, (id, text)) in rows.iter().enumerate() {
            let y = 80 + i as i32 * 34;
            self.label(p, id + LABEL, text, (LBL_X, y, 150, ROW));
            match *id {
                ID_SCALE => {
                    self.slider(p, *id, CTL_X, y, 232, 70, 200, 5, 10);
                    self.label(p, ID_SCALE_VAL, "", (436, y, 52, ROW));
                }
                ID_OPACITY => {
                    self.slider(p, *id, CTL_X, y, 232, 0, 255, 5, 25);
                    self.label(p, ID_OPACITY_VAL, "", (436, y, 52, ROW));
                }
                ID_FONT => self.combo(p, *id, CTL_X, y, CTL_W, true, FONTS.len() as i32),
                ID_INTERVAL => self.combo(p, *id, CTL_X, y, CTL_W, false, 5),
                ID_UNIT => self.combo(p, *id, CTL_X, y, CTL_W, false, SpeedUnit::ALL.len() as i32),
                _ => self.combo(p, *id, CTL_X, y, CTL_W, false, 3),
            }
        }

        self.group(p, "Behaviour", 292, 134);
        let (c0, c1, cw) = (LBL_X, LBL_X + 236, 226);
        let left = [
            (ID_SPARK, "Show sparklines"),
            (ID_LABELS, "Show metric labels"),
            (ID_STACKED, "Stack metrics in rows"),
            (ID_AUTOSTACK, "Auto-stack when it won\u{2019}t fit"),
        ];
        let right = [
            (ID_FULLSCREEN, "Hide in full-screen apps"),
            (ID_CLICKTHRU, "Click-through widget"),
            (ID_AUTOUPDATE, "Update automatically"),
            (ID_AUTOSTART, "Start with Windows"),
        ];
        for (i, (id, t)) in left.iter().enumerate() {
            self.check(p, *id, t, c0, 316 + i as i32 * 26, cw);
        }
        for (i, (id, t)) in right.iter().enumerate() {
            self.check(p, *id, t, c1, 316 + i as i32 * 26, cw);
        }

        // Fill the combos.
        let h = |id| ctl(self.parent, id);
        let (iv, un, th, ft) = (h(ID_INTERVAL), h(ID_UNIT), h(ID_THEME), h(ID_FONT));
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
    }

    fn build_metrics(&mut self) {
        let p = P_METRICS;
        self.hint(p, "Tick the metrics to show on the widget. Use Move up and Move down to change their order.", (GX, 56, GW, 34));
        let list = self.mk(
            Kind::List,
            "SysListView32",
            "",
            LVS_REPORT | LVS_SINGLESEL | LVS_NOCOLUMNHEADER | LVS_SHOWSELALWAYS | WS_TABSTOP,
            WS_EX_CLIENTEDGE,
            ID_LIST,
            p,
            (GX, 98, 376, 326),
            0,
        );
        send(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize);
        let mut col: LVCOLUMNW = unsafe { std::mem::zeroed() };
        col.mask = LVCF_WIDTH;
        col.cx = 300;
        send(list, LVM_INSERTCOLUMNW, 0, &col as *const _ as isize);
        self.button(p, ID_UP, "Move up", (GX + 388, 98, 96, 28), BS_PUSHBUTTON);
        self.button(p, ID_DOWN, "Move down", (GX + 388, 132, 96, 28), BS_PUSHBUTTON);
    }

    fn build_colours(&mut self) {
        let p = P_COLOURS;
        self.group(p, "Metric colours", 56, 270);
        for (i, m) in Metric::ALL.iter().enumerate() {
            let col = if i < 8 { 0 } else { 1 };
            let row = if i < 8 { i } else { i - 8 } as i32;
            let x = LBL_X + col * 232;
            let y = 80 + row * 30;
            self.label(p, 0, m.menu_label(), (x, y, 118, ROW));
            self.mk(
                Kind::Button,
                "BUTTON",
                "",
                BS_OWNERDRAW as u32 | WS_TABSTOP,
                0,
                ID_SWATCH + i as i32,
                p,
                (x + 122, y + 1, 40, 22),
                0,
            );
            self.button(p, ID_CRESET + i as i32, "Reset", (x + 168, y, 56, ROW), BS_PUSHBUTTON);
        }
        self.group(p, "Accent", 334, 90);
        self.check(p, ID_WINACCENT, "Use the Windows accent colour for metrics without a custom colour", LBL_X, 358, 456);
        self.button(p, ID_RESETALL, "Reset all colours", (LBL_X, 386, 150, 28), BS_PUSHBUTTON);
    }

    fn build_alerts(&mut self) {
        let p = P_ALERTS;
        self.group(p, "Notifications", 56, 248);
        self.check(p, ID_ALERTS, "Show alert notifications", LBL_X, 80, 440);
        self.number_row(p, ID_A_CPU, "CPU usage above (0 = off)", "%", 110, 0, 100);
        self.number_row(p, ID_A_RAM, "Memory usage above (0 = off)", "%", 142, 0, 100);
        self.number_row(p, ID_A_SECS, "\u{2026}for at least", "seconds", 174, 5, 3600);
        self.number_row(p, ID_A_DISK, "System drive free space below (0 = off)", "%", 206, 0, 100);
        self.number_row(p, ID_A_BAT, "Battery at or below (0 = off)", "%", 238, 0, 100);
        self.check(p, ID_A_CAP, "Alert at 90% and 100% of the monthly data cap", LBL_X, 272, 440);

        self.group(p, "Colour thresholds", 312, 90);
        self.number_row(p, ID_WARN, "Values turn amber from", "%", 336, 1, 100);
        self.number_row(p, ID_CRIT, "Values turn red from", "%", 368, 1, 100);
    }

    fn build_data(&mut self) {
        let p = P_DATA;
        self.group(p, "Data cap", 56, 136);
        self.label(p, ID_CAP + LABEL, "Monthly data cap (GB, 0 = off)", (LBL_X, 80, 250, ROW));
        self.edit(p, ID_CAP, 296, 80, 72, false);
        self.label(p, ID_CAP + UNIT, "GB", (376, 80, 60, ROW));
        self.number_row(p, ID_DAY, "Billing cycle resets on day", "", 114, 1, 28);
        self.hint(p, "The cap is compared with the data used since the last reset day. The widget\u{2019}s data metrics turn amber and red as it fills.", (LBL_X, 148, 448, 36));

        self.group(p, "Network adapter", 200, 60);
        self.label(p, ID_ADAPTER + LABEL, "Adapter", (LBL_X, 224, 100, ROW));
        let n = self.adapters.len() as i32 + 1;
        self.combo(p, ID_ADAPTER, 148, 224, 340, false, n);
        let ad = ctl(self.parent, ID_ADAPTER);
        combo_add(ad, "Automatic (all connected adapters)", 0);
        for l in self.adapter_labels.clone() {
            combo_add(ad, &l, 0);
        }
        self.button(p, ID_REPORT, "Open data usage report\u{2026}", (GX, 272, 220, 28), BS_PUSHBUTTON);
    }

    // --------------------------------------------------------------- layout

    /// Position every control for the current dpi and apply the font.
    fn layout(&mut self) {
        let was = LOADING.swap(true, Relaxed);
        let mut spin_fix: Vec<(HWND, HWND)> = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            send(it.hwnd, WM_SETFONT, self.font as usize, 1);
            if it.kind == Kind::Spin {
                // The buddy edit is the previous item.
                if let Some(prev) = i.checked_sub(1).and_then(|j| self.items.get(j)) {
                    spin_fix.push((it.hwnd, prev.hwnd));
                }
                continue;
            }
            unsafe {
                SetWindowPos(
                    it.hwnd,
                    std::ptr::null_mut(),
                    self.sc(it.x),
                    self.sc(it.y),
                    self.sc(it.w),
                    self.sc(it.h + it.drop),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        // Re-attaching a buddy rewrites its text from the arrow control: keep it.
        for (spin, edit) in spin_fix {
            let txt = get_text(edit);
            send(spin, UDM_SETBUDDY, edit as usize, 0);
            set_text(edit, &txt);
        }
        let list = ctl(self.parent, ID_LIST);
        if !list.is_null() {
            let mut rc: RECT = unsafe { std::mem::zeroed() };
            unsafe { GetClientRect(list, &mut rc) };
            send(list, LVM_SETCOLUMNWIDTH, 0, (rc.right - rc.left) as isize);
        }
        LOADING.store(was, Relaxed);
    }

    fn show_page(&mut self, page: i32) {
        self.page = page;
        for it in &self.items {
            let on = it.page < 0 || it.page == page;
            unsafe { ShowWindow(it.hwnd, if on { SW_SHOWNA } else { SW_HIDE }) };
        }
    }
}

// ------------------------------------------------------------------ theme

fn apply_theme(hwnd: HWND) {
    let Some(u) = ui() else { return };
    let dark = u.dark;
    unsafe {
        DeleteObject(u.br_bg);
        DeleteObject(u.br_ctl);
        let (bg, cl) = if dark { (DARK_BG, DARK_CTL) } else { (GetSysColor(COLOR_WINDOW), GetSysColor(COLOR_WINDOW)) };
        u.br_bg = CreateSolidBrush(bg);
        u.br_ctl = CreateSolidBrush(cl);
    }
    for it in &u.items {
        match (it.kind, dark) {
            (Kind::Group, true) => theme_name(it.hwnd, Some(""), Some("")), // classic: honours our text colour
            (Kind::Combo, true) => theme_name(it.hwnd, Some("DarkMode_CFD"), Some("Combobox")),
            (Kind::List, true) => theme_name(it.hwnd, Some("DarkMode_Explorer"), None),
            (Kind::List, false) => theme_name(it.hwnd, Some("Explorer"), None),
            (Kind::Text, _) => {}
            (_, true) => theme_name(it.hwnd, Some("DarkMode_Explorer"), None),
            (_, false) => theme_name(it.hwnd, None, None),
        }
    }
    let list = ctl(hwnd, ID_LIST);
    let (bk, tx) = if dark {
        (DARK_CTL, DARK_FG)
    } else {
        unsafe { (GetSysColor(COLOR_WINDOW), GetSysColor(COLOR_WINDOWTEXT)) }
    };
    send(list, LVM_SETBKCOLOR, 0, bk as isize);
    send(list, LVM_SETTEXTBKCOLOR, 0, bk as isize);
    send(list, LVM_SETTEXTCOLOR, 0, tx as isize);
    style_window(hwnd, dark);
    // Windows 11: Mica backdrop (DWMWA_SYSTEMBACKDROP_TYPE = 38, DWMSBT_MAINWINDOW = 2).
    let mica: i32 = 2;
    unsafe {
        DwmSetWindowAttribute(hwnd, 38, &mica as *const i32 as _, 4);
        RedrawWindow(hwnd, std::ptr::null(), std::ptr::null_mut(), RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME);
    }
}

// ------------------------------------------------------------------ values

fn fill_list(hwnd: HWND, select: Option<usize>) {
    let (order, show) = match ui() {
        Some(u) => (u.draft.order.clone(), u.draft.show),
        None => return,
    };
    let list = ctl(hwnd, ID_LIST);
    let was = LOADING.swap(true, Relaxed);
    send(list, LVM_DELETEALLITEMS, 0, 0);
    for (i, m) in order.iter().enumerate() {
        let mut txt = wide(m.menu_label());
        let mut it: LVITEMW = unsafe { std::mem::zeroed() };
        it.mask = LVIF_TEXT;
        it.iItem = i as i32;
        it.pszText = txt.as_mut_ptr();
        send(list, LVM_INSERTITEMW, 0, &it as *const _ as isize);
        if show[m.idx()] {
            let mut st: LVITEMW = unsafe { std::mem::zeroed() };
            st.stateMask = LVIS_STATEIMAGEMASK;
            st.state = 2 << 12;
            send(list, LVM_SETITEMSTATE, i, &st as *const _ as isize);
        }
    }
    if let Some(i) = select {
        let mut st: LVITEMW = unsafe { std::mem::zeroed() };
        st.stateMask = LVIS_SELECTED | LVIS_FOCUSED;
        st.state = LVIS_SELECTED | LVIS_FOCUSED;
        send(list, LVM_SETITEMSTATE, i, &st as *const _ as isize);
        send(list, LVM_ENSUREVISIBLE, i, 0);
    }
    LOADING.store(was, Relaxed);
    update_move_buttons(hwnd);
}

fn selected_row(hwnd: HWND) -> Option<usize> {
    let r = send(ctl(hwnd, ID_LIST), LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
    (r >= 0).then_some(r as usize)
}

fn update_move_buttons(hwnd: HWND) {
    let n = ui().map_or(0, |u| u.draft.order.len());
    let sel = selected_row(hwnd);
    unsafe {
        EnableWindow(ctl(hwnd, ID_UP), sel.is_some_and(|i| i > 0) as i32);
        EnableWindow(ctl(hwnd, ID_DOWN), sel.is_some_and(|i| i + 1 < n) as i32);
    }
}

fn move_selected(hwnd: HWND, dir: isize) {
    let Some(i) = selected_row(hwnd) else { return };
    let j = i as isize + dir;
    let ok = match ui() {
        Some(u) if j >= 0 && (j as usize) < u.draft.order.len() => {
            u.draft.order.swap(i, j as usize);
            true
        }
        _ => false,
    };
    if ok {
        fill_list(hwnd, Some(j as usize));
        mark_dirty(hwnd);
        unsafe { SetFocus(ctl(hwnd, ID_LIST)) };
    }
}

fn update_slider_labels(hwnd: HWND) {
    let sc = send(ctl(hwnd, ID_SCALE), TBM_GETPOS, 0, 0);
    let op = send(ctl(hwnd, ID_OPACITY), TBM_GETPOS, 0, 0);
    set_text(ctl(hwnd, ID_SCALE_VAL), &format!("{} %", sc));
    set_text(ctl(hwnd, ID_OPACITY_VAL), &format!("{} %", (op * 100 + 127) / 255));
}

fn update_alert_enabled(hwnd: HWND) {
    let on = is_checked(ctl(hwnd, ID_ALERTS)) as i32;
    for id in ALERT_IDS {
        for off in [0, SPIN, LABEL, UNIT] {
            let h = ctl(hwnd, id + off);
            if !h.is_null() {
                unsafe { EnableWindow(h, on) };
            }
        }
    }
}

fn update_colour_rows(hwnd: HWND) {
    let Some(u) = ui() else { return };
    for (i, m) in Metric::ALL.iter().enumerate() {
        let has = u.draft.colors.iter().any(|(x, _)| x == m);
        unsafe {
            EnableWindow(ctl(hwnd, ID_CRESET + i as i32), has as i32);
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
        set_checked(h(id), on);
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
    update_alert_enabled(hwnd);
    update_colour_rows(hwnd);
    fill_list(hwnd, None);
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

    d.sparklines = is_checked(h(ID_SPARK));
    d.labels = is_checked(h(ID_LABELS));
    d.stacked = is_checked(h(ID_STACKED));
    d.auto_stack = is_checked(h(ID_AUTOSTACK));
    d.hide_fullscreen = is_checked(h(ID_FULLSCREEN));
    d.click_through = is_checked(h(ID_CLICKTHRU));
    d.auto_update = is_checked(h(ID_AUTOUPDATE));
    d.alerts = is_checked(h(ID_ALERTS));
    d.alert_cap = is_checked(h(ID_A_CAP));
    d.windows_accent = is_checked(h(ID_WINACCENT));
    let autostart = is_checked(h(ID_AUTOSTART));

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
    let dark = effective_dark(a.m.s.theme);

    // Show the clamped values and the new baseline.
    populate(hwnd);
    unsafe {
        SetFocus(ctl(hwnd, IDOK));
        EnableWindow(ctl(hwnd, ID_APPLY), 0);
    }
    if let Some(u) = ui() {
        if u.dark != dark {
            u.dark = dark;
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

fn draw_swatch(dis: &DRAWITEMSTRUCT) {
    let Some(u) = ui() else { return };
    let i = (dis.CtlID as i32 - ID_SWATCH) as usize;
    let Some(m) = Metric::ALL.get(i) else { return };
    let rc = dis.rcItem;
    unsafe {
        FillRect(dis.hDC, &rc, u.br_bg);
        let edge = CreateSolidBrush(if u.dark { 0x0080_8080 } else { 0x008A_8A8A });
        FrameRect(dis.hDC, &rc, edge);
        DeleteObject(edge);
        let inner = RECT { left: rc.left + 1, top: rc.top + 1, right: rc.right - 1, bottom: rc.bottom - 1 };
        let fill = CreateSolidBrush(rgb_to_cr(u.swatch_rgb(*m)));
        FillRect(dis.hDC, &inner, fill);
        DeleteObject(fill);
        if dis.itemState & ODS_FOCUS != 0 {
            let f = RECT { left: rc.left + 3, top: rc.top + 3, right: rc.right - 3, bottom: rc.bottom - 3 };
            DrawFocusRect(dis.hDC, &f);
        }
        if dis.itemState & ODS_SELECTED != 0 {
            InvertRect(dis.hDC, &inner);
        }
    }
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

const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN;

/// Size the window for `dpi` and centre it on the primary work area.
fn fit(hwnd: HWND, dpi: u32) {
    unsafe {
        let sc = |v: i32| (v * dpi as i32 + 48) / 96;
        let mut rc = RECT { left: 0, top: 0, right: sc(CLIENT_W), bottom: sc(CLIENT_H) };
        AdjustWindowRectExForDpi(&mut rc, STYLE, 0, WS_EX_CONTROLPARENT, dpi);
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        let mut work: RECT = std::mem::zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut _ as _, 0);
        let x = work.left + ((work.right - work.left) - w) / 2;
        let y = work.top + (((work.bottom - work.top) - h) / 2).max(0);
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
            dwICC: ICC_TAB_CLASSES | ICC_BAR_CLASSES | ICC_LISTVIEW_CLASSES | ICC_UPDOWN_CLASS | ICC_STANDARD_CLASSES,
        };
        InitCommonControlsEx(&icc);

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

        let dark = a.m.dark;
        let dpi = GetDpiForSystem().max(96);
        let ui = Box::new(Ui {
            parent: std::ptr::null_mut(),
            hinst,
            items: Vec::new(),
            font: Ui::make_font(dpi),
            dpi,
            dark,
            br_bg: CreateSolidBrush(0),
            br_ctl: CreateSolidBrush(0),
            page: 0,
            draft,
            adapters,
            adapter_labels: labels,
            autostart: crate::autostart::enabled(),
            cust: [0x00FF_FFFF; 16],
        });
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

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let dpi = GetDpiForWindow(hwnd);
            let Some(u) = ui() else { return -1 };
            if dpi != 0 && dpi != u.dpi {
                DeleteObject(u.font);
                u.font = Ui::make_font(dpi);
                u.dpi = dpi;
            }
            u.parent = hwnd;
            u.build();
            u.layout();
            apply_theme(hwnd);
            populate(hwnd);
            EnableWindow(ctl(hwnd, ID_APPLY), 0);
            READY.store(true, Relaxed);
            0
        }
        WM_ERASEBKGND => {
            if let (true, Some(u)) = (READY.load(Relaxed), ui()) {
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                FillRect(w as HDC, &rc, u.br_bg);
                return 1;
            }
            DefWindowProcW(hwnd, msg, w, l)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORDLG | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let (true, Some(u)) = (READY.load(Relaxed), ui()) else { return DefWindowProcW(hwnd, msg, w, l) };
            let hdc = w as HDC;
            let input = msg == WM_CTLCOLOREDIT || msg == WM_CTLCOLORLISTBOX;
            if u.dark {
                SetTextColor(hdc, DARK_FG);
                SetBkColor(hdc, if input { DARK_CTL } else { DARK_BG });
                let br = if input { u.br_ctl } else { u.br_bg };
                return br as LRESULT;
            }
            if input {
                return DefWindowProcW(hwnd, msg, w, l);
            }
            SetTextColor(hdc, GetSysColor(COLOR_WINDOWTEXT));
            SetBkColor(hdc, GetSysColor(COLOR_WINDOW));
            u.br_bg as LRESULT
        }
        WM_DRAWITEM => {
            let dis = &*(l as *const DRAWITEMSTRUCT);
            if (ID_SWATCH..ID_SWATCH + Metric::ALL.len() as i32).contains(&(dis.CtlID as i32)) {
                draw_swatch(dis);
                return 1;
            }
            0
        }
        DM_GETDEFID => ((DC_HASDEFID << 16) | IDOK as u32) as LRESULT,
        WM_COMMAND => {
            let id = (w & 0xFFFF) as i32;
            let code = ((w >> 16) & 0xFFFF) as u32;
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
                ID_UP => move_selected(hwnd, -1),
                ID_DOWN => move_selected(hwnd, 1),
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
                ID_ALERTS if code == BN_CLICKED => {
                    update_alert_enabled(hwnd);
                    mark_dirty(hwnd);
                }
                ID_WINACCENT if code == BN_CLICKED => {
                    let on = is_checked(ctl(hwnd, ID_WINACCENT));
                    if let Some(u) = ui() {
                        u.draft.windows_accent = on;
                    }
                    update_colour_rows(hwnd);
                    mark_dirty(hwnd);
                }
                _ if (ID_SWATCH..ID_SWATCH + Metric::ALL.len() as i32).contains(&id) => {
                    if code == BN_CLICKED {
                        pick_colour(hwnd, (id - ID_SWATCH) as usize);
                    }
                }
                _ if (ID_CRESET..ID_CRESET + Metric::ALL.len() as i32).contains(&id) => {
                    if code == BN_CLICKED {
                        reset_colour(hwnd, (id - ID_CRESET) as usize);
                    }
                }
                _ if CHECK_IDS.contains(&id) => {
                    if code == BN_CLICKED {
                        mark_dirty(hwnd);
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
            if nm.idFrom == ID_TAB as usize && nm.code == TCN_SELCHANGE {
                let page = send(ctl(hwnd, ID_TAB), TCM_GETCURSEL, 0, 0) as i32;
                if let Some(u) = ui() {
                    u.show_page(page.max(0));
                }
            } else if nm.idFrom == ID_LIST as usize && nm.code == LVN_ITEMCHANGED && !LOADING.load(Relaxed) {
                let lv = &*(l as *const NMLISTVIEW);
                let changed = lv.uNewState ^ lv.uOldState;
                if lv.iItem >= 0 && changed & LVIS_STATEIMAGEMASK != 0 {
                    let on = (lv.uNewState & LVIS_STATEIMAGEMASK) >> 12 == 2;
                    if let Some(u) = ui() {
                        if let Some(m) = u.draft.order.get(lv.iItem as usize).copied() {
                            u.draft.show[m.idx()] = on;
                        }
                    }
                    mark_dirty(hwnd);
                }
                if changed & LVIS_SELECTED != 0 {
                    update_move_buttons(hwnd);
                }
            }
            0
        }
        WM_DPICHANGED => {
            let dpi = ((w >> 16) & 0xFFFF) as u32;
            let r = &*(l as *const RECT);
            if let Some(u) = ui() {
                DeleteObject(u.font);
                u.font = Ui::make_font(dpi);
                u.dpi = dpi;
            }
            SetWindowPos(hwnd, std::ptr::null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            if let Some(u) = ui() {
                u.layout();
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
