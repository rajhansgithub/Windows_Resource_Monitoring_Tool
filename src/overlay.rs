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
    pub tb: Taskbar,
    canvas: Canvas,
    font: HFONT,
    font_px: u32,
    sticky: [i32; Metric::ALL.len()],
    pub scale: f32,
    pub pos: Rect,
    pub shown: bool,
    pub tracking: bool,
    pub drag: Option<Drag>,
    /// Set after a click closes the panel; cleared when the mouse leaves.
    pub no_hover: bool,
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
    pub fn new(tb: Taskbar, click_through: bool) -> Option<Overlay> {
        let cls = wide(CLASS);
        let title = wide("Resource Monitor");
        let mut ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
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
                std::ptr::null_mut(),
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
            tb,
            canvas,
            font: std::ptr::null_mut(),
            font_px: 0,
            sticky: [0; Metric::ALL.len()],
            scale: tb.dpi as f32 / 96.0,
            pos: Rect::default(),
            shown: false,
            tracking: false,
            drag: None,
            no_hover: false,
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
        self.sticky = [0; Metric::ALL.len()];
    }

    fn ensure_font(&mut self, px: u32) {
        if px == self.font_px && !self.font.is_null() {
            return;
        }
        unsafe {
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
        }
        self.font = create_font(px as i32, 600, ui_face(), true);
        self.font_px = px;
        self.reset_widths();
    }

    pub fn show(&mut self, on: bool) {
        unsafe {
            if on {
                if !self.shown {
                    ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                }
                // Win11 raises the taskbar over other topmost windows when clicked; re-assert.
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
        let mut font_px = (13.0 * scale * ts).round() as u32;
        // Rows per column for a horizontal strip: fit 2 lines (or 3 on tall taskbars).
        let mut rpc = if m.s.stacked { 2usize } else { 1 };
        if dock && horiz && m.s.stacked {
            let avail = thickness - sc(4.0);
            let line = |px: u32| (px as f32 * 1.32).ceil() as i32;
            if line(font_px) * 3 <= avail && thickness >= sc(64.0) {
                rpc = 3;
            } else if line(font_px) * 2 > avail {
                let fit = ((avail / 2).max(0) as f32 / 1.32).floor() as u32;
                if fit as f32 >= 10.0 * scale {
                    font_px = fit;
                } else {
                    rpc = 1;
                }
            }
        }
        self.ensure_font(font_px);
        let font = self.font;
        let rows = m.rows();
        let n = rows.len();

        let (_, line_h) = self.canvas.measure(font, "Ag");
        let line_h = if line_h == 0 { (font_px as f32 * 1.32) as i32 } else { line_h - sc(1.0) }.max(1);
        let gap = sc(6.0);
        let col_gap = sc(14.0);
        let pad = sc(6.0);
        let spark_w = if m.s.sparklines { sc(30.0) } else { 0 };
        let spark_h = (line_h - sc(4.0)).max(4);
        // Row pitch: use the taskbar's height for breathing room between rows
        // (up to 1.5x line height) instead of stacking lines edge to edge.
        let pitch = if dock && horiz {
            let avail = thickness - 2 * sc(3.0);
            (avail / rpc as i32).clamp(line_h, (line_h as f32 * 1.5) as i32)
        } else {
            line_h + sc(4.0)
        };

        let values: Vec<(String, u8, &str)> = rows.iter().map(|&r| m.value(r)).collect();
        let label_w = |c: &[Metric]| -> i32 {
            if !m.s.labels {
                return 0;
            }
            c.iter().map(|&r| self.canvas.measure(font, short_label(r)).0).max().unwrap_or(0)
        };
        for (i, &r) in rows.iter().enumerate() {
            let (v, _, tpl) = &values[i];
            let tw = self.canvas.measure(font, tpl).0.max(self.canvas.measure(font, v).0);
            let cap = if r == Metric::TopProc { sc(110.0 * ts) } else { i32::MAX };
            self.sticky[r.idx()] = self.sticky[r.idx()].max(tw).min(cap);
        }
        let vw = |r: Metric| self.sticky[r.idx()];
        let spark_part = |r: Metric| if spark_w > 0 && has_spark(r) { gap + spark_w } else { 0 };

        // (row index, x, y, value_left, value_right, spark rect, draw label)
        let mut cells: Vec<(usize, i32, i32, i32, i32, Option<Rect>, bool)> = Vec::with_capacity(n);
        let (w, h);
        if n == 0 {
            (w, h) = (sc(24.0), if dock && horiz { thickness } else { line_h + 2 * pad });
        } else if horiz {
            let cols = pack(&rows, rpc);
            let hh = if dock { thickness.max(line_h + 2) } else { rpc as i32 * pitch + 2 * pad };
            let mut x = pad;
            let mut idx = 0;
            let small_gap = sc(8.0);
            for (ci, col) in cols.iter().enumerate() {
                let val_w = col.iter().map(|&r| vw(r)).max().unwrap_or(0);
                let sp = col.iter().map(|&r| spark_part(r)).max().unwrap_or(0);
                let lw = label_w(col);
                let lab_part = if lw > 0 { lw + gap } else { 0 };
                let y0 = (hh - col.len() as i32 * pitch) / 2 + (pitch - line_h) / 2;
                for (j, &r) in col.iter().enumerate() {
                    let y = y0 + j as i32 * pitch;
                    let vr = x + lab_part + val_w;
                    let spark = (spark_part(r) > 0).then(|| Rect {
                        l: vr + gap,
                        t: y + (line_h - spark_h) / 2,
                        r: vr + gap + spark_w,
                        b: y + (line_h - spark_h) / 2 + spark_h,
                    });
                    cells.push((idx, x, y, x + lab_part, vr, spark, lw > 0));
                    idx += 1;
                }
                // Related metrics on one line (▲ then ▼) sit closer together.
                let near = rpc == 1
                    && cols.get(ci + 1).is_some_and(|n| group(n[0]) == group(col[col.len() - 1]));
                let g = if near { small_gap } else { col_gap };
                x += lab_part + val_w + sp + g;
                if ci + 1 == cols.len() {
                    x -= g;
                }
            }
            (w, h) = (x + pad, hh);
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
                cells.push((i, x, y, vl, vr, spark, with_label));
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
        // Soft outline behind text when there's no solid background behind it.
        self.canvas.halo = (opacity < 120).then(|| if m.dark { Rgba(0, 0, 0, 110) } else { Rgba(255, 255, 255, 150) });
        for &(i, x, y, vl, vr, spark, with_label) in &cells {
            let r = rows[i];
            let (v, lvl, _) = &values[i];
            let ac = accent(r, m.dark);
            if with_label {
                // Lightened accent: keeps the colour cue but reads like text.
                self.canvas.text(font, short_label(r), x, y, ac.mix(fg, 0.3));
            }
            let vc = match lvl {
                2 => C_CRIT,
                1 => C_WARN,
                _ => fg,
            };
            let max_w = vw(r);
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
                self.canvas.fill_rect(sr, ac.with_a(if m.dark { 22 } else { 30 }));
                self.canvas.spark(sr, &data, ac);
            }
        }

        // ---- position
        let (x, y) = if dock {
            let off = (m.s.offset(&tb.key(), tb.primary) as f32 * scale) as i32;
            match tb.edge {
                Edge::Bottom | Edge::Top => {
                    let x = (tb.tray.l - w - sc(4.0) - off).max(tb.rect.l);
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
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
        }
    }
}
