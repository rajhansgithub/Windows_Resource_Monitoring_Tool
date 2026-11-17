//! One widget window per taskbar: layout, drawing, placement.

use crate::app::{accent, short_label, Model, C_CRIT, C_WARN};
use crate::render::{create_font, ui_face, Canvas, Rgba};
use crate::settings::{Metric, Mode};
use crate::taskbar::{self, Edge, Taskbar};
use crate::util::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub const CLASS: &str = "ResourceMonitor.Overlay";

pub struct Drag {
    pub start: POINT,
    pub win_start: (i32, i32),
    pub offset_start: i32,
    pub moved: bool,
}

pub struct Overlay {
    pub hwnd: HWND,
    /// Taskbar that owns this widget (dock mode), or null (floating).
    pub owner: HWND,
    pub tb: Taskbar,
    canvas: Canvas,
    /// [0] one-line layout (large), [1] stacked rows (fits two lines).
    faces: [Face; 2],
    pub scale: f32,
    pub pos: Rect,
    pub shown: bool,
    pub tracking: bool,
    pub drag: Option<Drag>,
    /// Set after a click closes the panel; cleared when the mouse leaves.
    pub no_hover: bool,
    /// Screen x where the taskbar's app buttons end (from UI Automation).
    pub icons: Option<i32>,
    /// A full-screen app is on this monitor (updated on events only).
    pub fullscreen: bool,
    /// Measured start of the clock/tray area on a secondary taskbar.
    pub tray_l: Option<i32>,
}

/// Which metrics belong together in one column (e.g. ▲ above ▼).
fn group(m: Metric) -> u8 {
    match m {
        Metric::Cpu | Metric::Ram => 0,
        Metric::Gpu | Metric::Vram => 1,
        Metric::DiskRead | Metric::DiskWrite | Metric::DiskActive => 2,
        Metric::Up | Metric::Down => 3,
        Metric::TodayUp | Metric::TodayDown => 4,
        Metric::TopProc => 5,
        Metric::Battery => 6,
    }
}

/// Pack rows into columns of `rpc`, never splitting a group across columns
/// when it would fit whole in the next one. Order is preserved.
pub fn pack(rows: &[Metric], rpc: usize) -> Vec<Vec<Metric>> {
    let mut groups: Vec<Vec<Metric>> = Vec::new();
    for &m in rows {
        match groups.last_mut() {
            Some(g) if group(g[0]) == group(m) && g.len() < rpc => g.push(m),
            _ => groups.push(vec![m]),
        }
    }
    let mut cols: Vec<Vec<Metric>> = Vec::new();
    for g in groups {
        match cols.last_mut() {
            Some(c) if c.len() + g.len() <= rpc => c.extend(g),
            _ => cols.push(g),
        }
    }
    cols
}

/// (row index, x, y, value_left, value_right, spark rect, draw label, face)
type Cell = (usize, i32, i32, i32, i32, Option<Rect>, bool, usize);

/// A font size with its line metrics and reserved value widths.
struct Face {
    font: HFONT,
    px: u32,
    line_h: i32,
    spark_h: i32,
    /// Widest value seen per metric (keeps the layout from jittering).
    sticky: [i32; Metric::ALL.len()],
}

impl Default for Face {
    fn default() -> Self {
        Face { font: std::ptr::null_mut(), px: 0, line_h: 1, spark_h: 4, sticky: [0; Metric::ALL.len()] }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Pack {
    /// One line, every metric its own column.
    Flat,
    /// Related metrics stacked (▲ over ▼, CPU over RAM); groups never mix.
    Groups,
    /// Fill every column (different groups may share one).
    Full,
}

pub fn pack_groups(rows: &[Metric], rpc: usize) -> Vec<Vec<Metric>> {
    let mut cols: Vec<Vec<Metric>> = Vec::new();
    for &m in rows {
        match cols.last_mut() {
            Some(c) if group(c[0]) == group(m) && c.len() < rpc => c.push(m),
            _ => cols.push(vec![m]),
        }
    }
    cols
}

struct Geo {
    gap: i32,
    col_gap: i32,
    pad: i32,
    spark_w: i32,
    small_gap: i32,
}

fn has_spark(m: Metric) -> bool {
    !matches!(m, Metric::TodayUp | Metric::TodayDown | Metric::TopProc | Metric::Battery)
}

pub fn register(hinst: HINSTANCE, wndproc: WNDPROC) {
    unsafe {
        let cls = wide(CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: wndproc,
            hInstance: hinst,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            lpszClassName: cls.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
    }
}

impl Overlay {
    /// `owned`: dock mode. The taskbar becomes the widget's owner window, so
    /// Windows keeps the widget directly above the taskbar in the same z-order
    /// step — clicking the taskbar can't cover it, so no flicker and no
    /// topmost re-asserting. Floating widgets are plain topmost windows.
    pub fn new(tb: Taskbar, click_through: bool, owned: bool) -> Option<Overlay> {
        let cls = wide(CLASS);
        let title = wide("Resource Monitor");
        let owner = if owned { tb.hwnd } else { std::ptr::null_mut() };
        let mut ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        if owner.is_null() {
            ex |= WS_EX_TOPMOST;
        }
        if click_through {
            ex |= WS_EX_TRANSPARENT;
        }
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                cls.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                owner,
                std::ptr::null_mut(),
                windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        let mut canvas = Canvas::new();
        // Text can be measured before the first real layout.
        canvas.resize(1, 1);
        (!hwnd.is_null()).then(|| Overlay {
            hwnd,
            owner,
            tb,
            canvas,
            faces: [Face::default(), Face::default()],
            scale: tb.dpi as f32 / 96.0,
            pos: Rect::default(),
            shown: false,
            tracking: false,
            drag: None,
            no_hover: false,
            icons: None,
            fullscreen: false,
            tray_l: None,
        })
    }

    pub fn set_click_through(&self, on: bool) {
        unsafe {
            let ex = GetWindowLongW(self.hwnd, GWL_EXSTYLE) as u32;
            let ex = if on { ex | WS_EX_TRANSPARENT } else { ex & !WS_EX_TRANSPARENT };
            SetWindowLongW(self.hwnd, GWL_EXSTYLE, ex as i32);
        }
    }

    pub fn reset_widths(&mut self) {
        for f in &mut self.faces {
            f.sticky = [0; Metric::ALL.len()];
        }
    }

    /// Create/resize face `i` and refresh its metrics.
    fn ensure_face(&mut self, i: usize, px: u32, scale: f32) {
        let f = &mut self.faces[i];
        if px != f.px || f.font.is_null() {
            unsafe {
                if !f.font.is_null() {
                    DeleteObject(f.font);
                }
            }
            // Regular weight: crisper and easier to read than semibold at a distance.
            f.font = create_font(px as i32, 400, ui_face(), true);
            f.px = px;
            f.sticky = [0; Metric::ALL.len()];
        }
        let (_, lh) = self.canvas.measure(f.font, "Ag");
        let f = &mut self.faces[i];
        f.line_h = if lh == 0 { (px as f32 * 1.25) as i32 } else { lh - (scale).round() as i32 }.max(1);
        f.spark_h = (f.line_h - (4.0 * scale).round() as i32).max(4);
    }

    /// `raise`: re-assert topmost (only after events, e.g. focus changed).
    pub fn show(&mut self, on: bool, raise: bool) {
        unsafe {
            if on {
                if !self.shown {
                    ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                }
                // Owned by the taskbar: Windows already keeps us above it.
                if !self.owner.is_null() || (self.shown && !raise) {
                    self.shown = true;
                    return;
                }
                // Floating: keep above other always-on-top windows.
                SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
            } else if self.shown {
                ShowWindow(self.hwnd, SW_HIDE);
            }
        }
        self.shown = on;
    }

    /// Where the tray/clock area starts: measured on secondary taskbars,
    /// the real tray window on the main one.
    fn tray_left(&self) -> i32 {
        if self.tb.primary {
            self.tb.tray.l
        } else {
            self.tray_l.unwrap_or(self.tb.tray.l)
        }
    }

    /// Widest the widget may be on the taskbar without covering app icons.
    fn budget(&self, s: &crate::settings::Settings) -> Option<i32> {
        let tb = self.tb;
        if s.mode != Mode::Dock || !tb.edge.horizontal() {
            return None;
        }
        let scale = tb.dpi as f32 / 96.0;
        let sc = |v: f32| (v * scale).round() as i32;
        let off = (s.offset(&tb.key(), tb.primary) as f32 * scale) as i32;
        let right = self.tray_left() - sc(4.0) - off;
        let b = if s.max_width_pct > 0 {
            tb.rect.w() * s.max_width_pct as i32 / 100
        } else {
            match self.icons {
                Some(icons) => right - icons - sc(12.0),
                // Not known yet (or UI Automation unavailable): ~30% of the taskbar.
                None => tb.rect.w() * 30 / 100,
            }
        };
        Some(b.max(sc(60.0)))
    }

    /// Horizontal layout for one packing level. Returns (cells, width, height).
    #[allow(clippy::too_many_arguments)]
    fn layout_h(
        &self,
        rows: &[Metric],
        p: Pack,
        rpc: usize,
        sparks: bool,
        labels: bool,
        g: &Geo,
        pitch_for: &dyn Fn(usize, i32) -> i32,
        dock: bool,
        thickness: i32,
    ) -> (Vec<Cell>, i32, i32) {
        let fi = if p == Pack::Flat { 0 } else { 1 };
        let face = &self.faces[fi];
        let font = face.font;
        let (line_h, spark_h) = (face.line_h, face.spark_h);
        let cols = match p {
            Pack::Flat => pack(rows, 1),
            Pack::Groups => pack_groups(rows, rpc),
            Pack::Full => pack(rows, rpc),
        };
        let used_rows = cols.iter().map(|c| c.len()).max().unwrap_or(1);
        let pitch = pitch_for(used_rows, line_h);
        let hh = if dock { thickness.max(line_h + 2) } else { used_rows as i32 * pitch + 2 * g.pad };
        let vw = |r: Metric| face.sticky[r.idx()];
        let spark_part = |r: Metric| if sparks && has_spark(r) { g.gap + g.spark_w } else { 0 };
        let mut cells = Vec::with_capacity(rows.len());
        let mut x = g.pad;
        let mut idx = 0;
        for (ci, col) in cols.iter().enumerate() {
            let val_w = col.iter().map(|&r| vw(r)).max().unwrap_or(0);
            let sp = col.iter().map(|&r| spark_part(r)).max().unwrap_or(0);
            let lw = if labels {
                col.iter().map(|&r| self.canvas.measure(font, short_label(r)).0).max().unwrap_or(0)
            } else {
                0
            };
            let lab_part = if lw > 0 { lw + g.gap } else { 0 };
            let y0 = (hh - col.len() as i32 * pitch) / 2 + (pitch - line_h) / 2;
            for (j, &r) in col.iter().enumerate() {
                let y = y0 + j as i32 * pitch;
                let vr = x + lab_part + val_w;
                let spark = (spark_part(r) > 0).then(|| Rect {
                    l: vr + g.gap,
                    t: y + (line_h - spark_h) / 2,
                    r: vr + g.gap + g.spark_w,
                    b: y + (line_h - spark_h) / 2 + spark_h,
                });
                cells.push((idx, x, y, x + lab_part, vr, spark, lw > 0, fi));
                idx += 1;
            }
            // Related metrics on one line (▲ then ▼) sit closer together.
            let near = p == Pack::Flat
                && cols.get(ci + 1).is_some_and(|n| group(n[0]) == group(col[col.len() - 1]));
            let gp = if near { g.small_gap } else { g.col_gap };
            x += lab_part + val_w + sp;
            if ci + 1 < cols.len() {
                x += gp;
            }
        }
        (cells, x + g.pad, hh)
    }

    /// Measure, lay out and draw the widget, then position it.
    pub fn render(&mut self, m: &Model) {
        let tb = self.tb;
        let dock = m.s.mode == Mode::Dock;
        let horiz = !dock || tb.edge.horizontal();
        let dpi = if dock {
            tb.dpi
        } else {
            let hmon = unsafe {
                MonitorFromPoint(POINT { x: m.s.float_x, y: m.s.float_y }, MONITOR_DEFAULTTONEAREST)
            };
            taskbar::monitor_dpi(hmon)
        };
        let scale = dpi as f32 / 96.0;
        self.scale = scale;
        let ts = m.s.text_scale as f32 / 100.0;
        let sc = |v: f32| (v * scale).round() as i32;

        let thickness = if tb.edge.horizontal() { tb.rect.h() } else { tb.rect.w() };
        // One-line text is 1.5x the old 13px base; stacked text is as large as
        // two rows allow on this taskbar.
        let big = (19.5 * scale * ts).round() as u32;
        let line = |px: u32| (px as f32 * 1.25).ceil() as i32;
        let avail = thickness - sc(4.0);
        let mut flat_px = big;
        let mut stack_px = big;
        // Most rows a column may hold on this taskbar: 2 (3 on tall taskbars, 1 on tiny ones).
        let mut rpc = 2usize;
        if dock && horiz {
            flat_px = flat_px.min(((avail as f32) / 1.25).floor().max(9.0) as u32);
            if line(stack_px) * 3 <= avail && thickness >= sc(64.0) {
                rpc = 3;
            } else if line(stack_px) * 2 > avail {
                let fit = ((avail / 2).max(0) as f32 / 1.25).floor() as u32;
                if fit as f32 >= 10.0 * scale {
                    stack_px = fit;
                } else {
                    rpc = 1;
                }
            }
        }
        // Vertical taskbars are narrow: use the stacked size there.
        if dock && !horiz {
            flat_px = stack_px.min((13.0 * 1.3 * scale * ts).round() as u32);
            stack_px = flat_px;
        }
        self.ensure_face(0, flat_px, scale);
        self.ensure_face(1, stack_px, scale);
        let rows = m.rows();
        let n = rows.len();

        let gap = sc(6.0);
        let col_gap = sc(16.0);
        let pad = sc(6.0);
        let spark_w = if m.s.sparklines { sc(30.0) } else { 0 };
        // Row pitch: use the taskbar's height for breathing room between rows
        // (up to 1.5x line height) instead of stacking lines edge to edge.
        let pitch_for = |rows_n: usize, line_h: i32| {
            if dock && horiz {
                let avail = thickness - 2 * sc(3.0);
                (avail / rows_n.max(1) as i32).clamp(line_h, (line_h as f32 * 1.5) as i32)
            } else {
                line_h + sc(4.0)
            }
        };

        let values: Vec<(String, u8, &str)> = rows.iter().map(|&r| m.value(r)).collect();
        for fi in 0..2 {
            let font = self.faces[fi].font;
            // Top-process name: up to ~9 characters' worth before "…".
            let cap_w = (self.faces[fi].px as f32 * 8.5).round() as i32;
            for (i, &r) in rows.iter().enumerate() {
                let (v, _, tpl) = &values[i];
                let tw = self.canvas.measure(font, tpl).0.max(self.canvas.measure(font, v).0);
                let cap = if r == Metric::TopProc { cap_w } else { i32::MAX };
                let st = &mut self.faces[fi].sticky[r.idx()];
                *st = (*st).max(tw).min(cap);
            }
        }
        // Vertical taskbars use the stacked face.
        let vf = 1;
        let font = self.faces[vf].font;
        let line_h = self.faces[vf].line_h;
        let spark_h = self.faces[vf].spark_h;
        let pitch = pitch_for(1, line_h);
        let label_w = |c: &[Metric]| -> i32 {
            if !m.s.labels {
                return 0;
            }
            c.iter().map(|&r| self.canvas.measure(font, short_label(r)).0).max().unwrap_or(0)
        };
        let vw = |r: Metric| self.faces[vf].sticky[r.idx()];

        let geo = Geo { gap, col_gap, pad, spark_w, small_gap: sc(10.0) };
        let mut cells: Vec<Cell> = Vec::with_capacity(n);
        // "+N" marker for metrics that only fit in the hover panel.
        let mut more: Option<(String, i32, i32)> = None;
        let more_w = gap + self.canvas.measure(self.faces[1].font, "+88").0;
        let (w, h);
        if n == 0 {
            (w, h) = (sc(24.0), if dock && horiz { thickness } else { self.faces[0].line_h + 2 * pad });
        } else if horiz {
            // Try progressively more compact layouts until the widget fits the
            // free space next to the tray (budget), so it never covers app icons.
            // The user's choice for this display is always honoured; only with
            // auto_stack may a one-line widget fall back to stacked rows.
            let start = if m.s.stacked_for(&tb.key()) && rpc > 1 { Pack::Groups } else { Pack::Flat };
            let mut levels: Vec<(Pack, bool, bool)> = Vec::new();
            for (p, sp, lb) in [
                (Pack::Flat, true, true),
                (Pack::Groups, true, true),
                (Pack::Full, true, true),
                (Pack::Groups, false, true),
                (Pack::Full, false, true),
                (Pack::Full, false, false),
            ] {
                let p = if rpc == 1 { Pack::Flat } else { p };
                let allowed = if m.s.auto_stack { p >= start } else { p == start || (start == Pack::Groups && p == Pack::Full) };
                if !allowed || levels.contains(&(p, sp, lb)) {
                    continue;
                }
                levels.push((p, sp, lb));
            }
            let budget = self.budget(&m.s);
            // Still too wide when fully stacked? Move trailing metrics to the
            // hover panel (shown as "+N") rather than cover icons or drop labels.
            let mut vis = rows.len();
            if let Some(b) = budget {
                while vis > 2 {
                    let tightest = if start == Pack::Flat && !m.s.auto_stack { Pack::Flat } else { Pack::Full };
                    let l = self.layout_h(&rows[..vis], tightest, rpc, false, m.s.labels, &geo, &pitch_for, dock, thickness);
                    if l.1 + if vis < rows.len() { more_w } else { 0 } <= b {
                        break;
                    }
                    vis -= 1;
                }
            }
            let mut chosen = None;
            for &(p, sp, lb) in &levels {
                let l = self.layout_h(&rows[..vis], p, rpc, sp && spark_w > 0, lb && m.s.labels, &geo, &pitch_for, dock, thickness);
                let fits = budget.is_none_or(|b| l.1 + if vis < rows.len() { more_w } else { 0 } <= b);
                chosen = Some(l);
                if fits {
                    break;
                }
            }
            let (c, mut ww, hh) = chosen.unwrap_or_default();
            cells = c;
            if vis < rows.len() {
                let t = format!("+{}", rows.len() - vis);
                more = Some((t, ww - pad + gap, (hh - self.faces[1].line_h) / 2));
                ww += more_w;
            }
            (w, h) = (ww, hh);
        } else {
            // Vertical taskbar: one row per metric, drop parts that don't fit.
            let width = thickness.max(sc(40.0));
            let inner = width - 2 * sc(4.0);
            let max_v = rows.iter().map(|&r| vw(r)).max().unwrap_or(0);
            let lw = label_w(&rows);
            let lab_part = if lw > 0 { lw + gap } else { 0 };
            let with_label = lw > 0 && lab_part + max_v <= inner;
            let with_spark =
                spark_w > 0 && (if with_label { lab_part } else { 0 }) + max_v + gap + spark_w <= inner;
            let mut y = pad;
            for (i, &r) in rows.iter().enumerate() {
                let x = sc(4.0);
                let sp = with_spark && has_spark(r);
                let vr = if sp { width - sc(4.0) - gap - spark_w } else { width - sc(4.0) };
                let spark = sp.then(|| Rect {
                    l: vr + gap,
                    t: y + (line_h - spark_h) / 2,
                    r: vr + gap + spark_w,
                    b: y + (line_h - spark_h) / 2 + spark_h,
                });
                let vl = x + if with_label { lab_part } else { 0 };
                cells.push((i, x, y, vl, vr, spark, with_label, vf));
                y += pitch;
            }
            (w, h) = (width, y + pad);
        }

        // ---- draw
        self.canvas.resize(w, h);
        let fg = if m.dark { Rgba::rgb(245, 245, 245) } else { Rgba::rgb(18, 18, 18) };
        let opacity = if dock { m.s.bg_opacity } else { m.s.bg_opacity.max(200) };
        // alpha 1 keeps the whole strip hoverable while staying invisible.
        self.canvas.clear(Rgba(0, 0, 0, 1));
        if opacity > 1 {
            let bg = if m.dark { Rgba(28, 28, 28, opacity) } else { Rgba(248, 248, 248, opacity) };
            let inset = if dock && horiz { sc(4.0) } else { 0 };
            self.canvas.fill_round(Rect { l: 0, t: inset, r: w, b: h - inset }, sc(6.0) as f32, bg);
        }
        // Crisp 1px shadow behind text when there's no solid background behind it.
        self.canvas.halo = (opacity < 120).then(|| if m.dark { Rgba(0, 0, 0, 150) } else { Rgba(255, 255, 255, 170) });
        if let Some((t, x, y)) = &more {
            self.canvas.text(self.faces[1].font, t, *x, *y, fg.mix(Rgba(128, 128, 128, 255), 0.5));
        }
        for &(i, x, y, vl, vr, spark, with_label, fi) in &cells {
            let font = self.faces[fi].font;
            let r = rows[i];
            let (v, lvl, _) = &values[i];
            let ac = accent(r, m.dark);
            if with_label {
                // Lightened accent: keeps the colour cue but reads like text.
                self.canvas.text(font, short_label(r), x, y, ac.mix(fg, 0.3));
            }
            let pct = m.pct_of(r);
            let vc = match (pct, lvl) {
                (Some(p), _) => m.heat_text(fg, p),
                (None, 2) => C_CRIT,
                (None, 1) => C_WARN,
                _ => fg,
            };
            let max_w = self.faces[fi].sticky[r.idx()];
            let mut v = v.clone();
            if self.canvas.measure(font, &v).0 > max_w {
                while v.chars().count() > 2 && self.canvas.measure(font, &format!("{}\u{2026}", v)).0 > max_w {
                    v.pop();
                }
                v.push('\u{2026}');
            }
            if r == Metric::TopProc {
                self.canvas.text(font, &v, vl, y, vc);
            } else {
                self.canvas.text_right(font, &v, vr, y, vc);
            }
            if let Some(sr) = spark {
                let data = m.series(r, 30);
                let sc_col = pct.map_or(ac, |p| m.heat(ac, p));
                self.canvas.fill_rect(sr, sc_col.with_a(if m.dark { 22 } else { 30 }));
                self.canvas.spark(sr, &data, sc_col);
            }
        }

        // ---- position
        let (x, y) = if dock {
            let off = (m.s.offset(&tb.key(), tb.primary) as f32 * scale) as i32;
            match tb.edge {
                Edge::Bottom | Edge::Top => {
                    let x = (self.tray_left() - w - sc(4.0) - off).max(tb.rect.l);
                    (x, tb.rect.t + (tb.rect.h() - h) / 2)
                }
                Edge::Left | Edge::Right => {
                    let y = (tb.tray.t - h - sc(4.0) - off).max(tb.rect.t);
                    (tb.rect.l + (tb.rect.w() - w) / 2, y)
                }
            }
        } else {
            let mon = taskbar::monitor_info(unsafe {
                MonitorFromPoint(POINT { x: m.s.float_x, y: m.s.float_y }, MONITOR_DEFAULTTONEAREST)
            })
            .map(|v| v.0)
            .unwrap_or(tb.monitor);
            (
                m.s.float_x.clamp(mon.l, (mon.r - w).max(mon.l)),
                m.s.float_y.clamp(mon.t, (mon.b - h).max(mon.t)),
            )
        };
        self.pos = Rect { l: x, t: y, r: x + w, b: y + h };
        self.canvas.present(self.hwnd, x, y);
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd);
            for f in &self.faces {
                if !f.font.is_null() {
                    DeleteObject(f.font);
                }
            }
        }
    }
}
