//! Bluetooth page (the Bluetooth module): every paired device with its battery, in one of
//! three layouts the user switches between on the page.
//!
//! - List: one row per device, a segmented level bar and the percent. Fits the most.
//! - Rack: a card per device with a level readout and a meter; earbuds that report each
//!   bud get a double-width card with left, right and case.
//! - Hero: the device in use large on top with rings, the rest as small tiles.
//!
//! The levels are what Windows itself shows in Settings (`sensors::deep::bluetooth`), read
//! every 10 s while this page is open and every 30 s on the overview. A child module of
//! `monitor`, so it draws with the same `Draw` helpers and palette. Icons are drawn as
//! strokes so they need no icon font and scale with the DPI.

use super::*;
use crate::sensors::deep::bluetooth::{BtDevice, Kind};

/// Layout segments on the page's top row (`Hit::BtLayout`).
pub(super) const BT_LAYOUTS: [&str; 3] = ["List", "Rack", "Hero"];
/// At or below this percent a connected device is called low.
const LOW_PCT: u8 = 20;
/// Rows on the overview card.
const CARD_ROWS: usize = 4;
/// Segments in a rack card's meter.
const VU_SEGS: i32 = 16;

/// Laid-out pieces of the Bluetooth page: (index into the device list, rect).
#[derive(Clone, Default)]
pub(super) struct BtLayout {
    /// The single card of the List layout.
    pub list: Option<Rect>,
    pub tiles: Vec<(usize, Rect)>,
    /// Hero layout: the device in use.
    pub hero: Option<(usize, Rect)>,
    /// Nothing paired: the empty card.
    pub empty: Option<Rect>,
}

/// The level that decides a device's colour and order: the lower bud when both report.
pub(super) fn level(d: &BtDevice) -> Option<u8> {
    match (d.left, d.right) {
        (Some(l), Some(r)) => Some(l.min(r)),
        _ => d.battery.or(d.left).or(d.right),
    }
}

/// Reports left, right and case separately (the helper module fills these).
fn per_bud(d: &BtDevice) -> bool {
    d.left.is_some() || d.right.is_some()
}

fn is_low(d: &BtDevice) -> bool {
    d.connected && level(d).is_some_and(|l| l <= LOW_PCT)
}

fn kind_label(k: Kind) -> &'static str {
    match k {
        Kind::Earbuds => "EARBUDS",
        Kind::Headphones => "HEADPHONES",
        Kind::Headset => "HEADSET",
        Kind::Speaker => "SPEAKER",
        Kind::Mouse => "MOUSE",
        Kind::Keyboard => "KEYBOARD",
        Kind::Gamepad => "CONTROLLER",
        Kind::Pen => "PEN",
        Kind::Phone => "PHONE",
        Kind::Watch => "WATCH",
        Kind::Computer => "COMPUTER",
        Kind::Other => "DEVICE",
    }
}

/// "3 h", "2 d": how long ago, short.
fn ago(unix: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let s = now.saturating_sub(unix);
    if s < 90 {
        "just now".into()
    } else if s < 3600 {
        format!("{} min ago", s / 60)
    } else if s < 48 * 3600 {
        format!("{} h ago", s / 3600)
    } else {
        format!("{} d ago", s / 86400)
    }
}

/// Connection state in words.
fn state(d: &BtDevice) -> String {
    if d.connected {
        if d.charging == Some(true) {
            "Charging".into()
        } else {
            "Connected".into()
        }
    } else {
        match d.last_seen {
            Some(t) => format!("Seen {}", ago(t)),
            None => "Not connected".into(),
        }
    }
}

fn pct_text(v: Option<u8>) -> String {
    v.map_or("--".into(), |v| v.to_string())
}

impl Monitor {
    /// Paired Bluetooth devices from the last sample (connected first).
    pub(super) fn bt(&self) -> &[BtDevice] {
        &self.det.deep.bt
    }

    /// The Bluetooth module is on and something is paired.
    pub(super) fn has_bt(&self) -> bool {
        self.bt_on && !self.bt().is_empty()
    }

    /// The device the Hero layout puts on top: connected earbuds or headphones first,
    /// then any connected device with a level.
    fn hero_dev(&self) -> Option<usize> {
        let b = self.bt();
        let audio = |d: &BtDevice| matches!(d.kind, Kind::Earbuds | Kind::Headphones | Kind::Headset);
        b.iter()
            .position(|d| d.connected && per_bud(d))
            .or_else(|| b.iter().position(|d| d.connected && audio(d) && d.battery.is_some()))
            .or_else(|| b.iter().position(|d| d.connected && level(d).is_some()))
    }

    pub(super) fn bt_verdict(&self) -> (crate::insights::Tone, String, String) {
        use crate::insights::Tone;
        let b = self.bt();
        let on = b.iter().filter(|d| d.connected).count();
        let sub = format!("{on} connected \u{00B7} {} paired \u{00B7} levels as Windows reports them", b.len());
        if let Some(d) = b.iter().filter(|d| is_low(d)).min_by_key(|d| level(d)) {
            let l = level(d).unwrap_or(0);
            return (Tone::Bad, format!("{} is at {l}%. Charge it soon.", d.name), sub);
        }
        if on == 0 {
            return (Tone::Info, "No Bluetooth device is connected.".into(), format!("{} paired. Levels shown are the last Windows saw.", b.len()));
        }
        match b.iter().filter(|d| d.connected).filter_map(|d| level(d).map(|l| (l, d))).min_by_key(|x| x.0) {
            Some((l, d)) => (Tone::Good, "Every connected device has charge.".into(), format!("Lowest: {} at {l}% \u{00B7} {sub}", d.name)),
            None => (Tone::Info, format!("{on} connected. None report a battery level."), sub),
        }
    }

    /// Lay the Bluetooth page body out from `y`; returns the bottom.
    pub(super) fn layout_bt(&mut self, l: i32, r: i32, y: i32) -> i32 {
        let gap = self.sc(12);
        let w = r - l;
        let n = self.bt().len();
        let mut lay = BtLayout::default();
        if n == 0 {
            let rc = Rect { l, t: y, r, b: y + self.sc(120) };
            lay.empty = Some(rc);
            self.dp.bt = lay;
            return rc.b;
        }
        let bottom = match self.bt_layout {
            0 => {
                let h = self.sc(14 + 20 + 12) + n as i32 * self.sc(58) + self.sc(10);
                let rc = Rect { l, t: y, r, b: y + h };
                lay.list = Some(rc);
                rc.b
            }
            1 => {
                // Cards of at least 230 px; double width for earbuds with per-bud levels.
                let cols = ((w + gap) / (self.sc(230) + gap)).clamp(1, 6);
                let colw = (w - gap * (cols - 1)) as f32 / cols as f32;
                let h = self.sc(150);
                let (mut c, mut t) = (0, y);
                let mut row: Vec<(usize, i32, i32)> = Vec::new();
                let flush = |row: &mut Vec<(usize, i32, i32)>, t: i32, lay: &mut BtLayout| {
                    // A short row: its last card takes the rest of the row.
                    let used: i32 = row.iter().map(|x| x.2).sum();
                    if let Some(last) = row.last_mut() {
                        last.2 += cols - used;
                    }
                    let mut c0 = 0;
                    for (i, _, span) in row.drain(..) {
                        let x0 = l + (c0 as f32 * (colw + gap as f32)).round() as i32;
                        let x1 = if c0 + span == cols { r } else { l + ((c0 + span) as f32 * (colw + gap as f32) - gap as f32).round() as i32 };
                        lay.tiles.push((i, Rect { l: x0, t, r: x1, b: t + h }));
                        c0 += span;
                    }
                };
                for (i, d) in self.bt().iter().enumerate() {
                    let span = if per_bud(d) && cols >= 2 { 2 } else { 1 };
                    if c + span > cols {
                        flush(&mut row, t, &mut lay);
                        t += h + gap;
                        c = 0;
                    }
                    row.push((i, c, span));
                    c += span;
                }
                flush(&mut row, t, &mut lay);
                t + h
            }
            _ => {
                let mut t = y;
                let hero = self.hero_dev();
                if let Some(i) = hero {
                    let wide = w >= self.sc(520);
                    let h = if wide { self.sc(150) } else { self.sc(250) };
                    lay.hero = Some((i, Rect { l, t, r, b: t + h }));
                    t += h + gap;
                }
                let rest: Vec<usize> = (0..n).filter(|i| Some(*i) != hero).collect();
                if !rest.is_empty() {
                    let cols = ((w + gap) / (self.sc(160) + gap)).clamp(1, 8).min(rest.len() as i32);
                    let tw = (w - gap * (cols - 1)) as f32 / cols as f32;
                    let h = self.sc(150);
                    for (k, i) in rest.iter().enumerate() {
                        let (row, c) = (k as i32 / cols, k as i32 % cols);
                        let x0 = l + (c as f32 * (tw + gap as f32)).round() as i32;
                        let x1 = if c == cols - 1 { r } else { l + ((c + 1) as f32 * (tw + gap as f32) - gap as f32).round() as i32 };
                        let tt = t + row * (h + gap);
                        lay.tiles.push((*i, Rect { l: x0, t: tt, r: x1, b: tt + h }));
                    }
                    t += (rest.len() as i32 + cols - 1) / cols * (h + gap) - gap;
                } else {
                    t -= gap;
                }
                t
            }
        };
        self.dp.bt = lay;
        bottom
    }

    /// Height of the overview's Bluetooth card.
    pub(super) fn bt_card_h(&self) -> i32 {
        let rows = self.bt().len().clamp(1, CARD_ROWS) as i32;
        let more = if self.bt().len() > CARD_ROWS { self.sc(22) } else { 0 };
        self.sc(14 + 20 + 12) + rows * self.sc(34) + more + self.sc(12)
    }
}

impl Draw<'_> {
    /// Battery colour: low red, charging green, else the accent; dim when disconnected.
    fn bt_col(&self, d: &BtDevice) -> Rgba {
        if !d.connected {
            self.col.dim
        } else if is_low(d) {
            self.col.crit
        } else if d.charging == Some(true) {
            self.col.good
        } else {
            self.col.accent
        }
    }

    pub(super) fn draw_bt(&self, cv: &mut Canvas, vis: &dyn Fn(Rect) -> bool) {
        let m = self.m;
        let lay = &m.dp.bt;
        let sh = |r: Rect| Rect { t: r.t + self.dy, b: r.b + self.dy, ..r };
        if let Some(r) = lay.empty.filter(|r| vis(*r)) {
            let r = sh(r);
            self.card(cv, r);
            let y = self.header(cv, r, "No paired Bluetooth devices", "", 0);
            cv.text(self.f.small, "Pair earbuds, a mouse or a controller in Windows Settings and they show up here.", r.l + self.s(16), y, self.col.dim);
        }
        if let Some(r) = lay.list.filter(|r| vis(*r)) {
            self.bt_list(cv, sh(r));
        }
        if let Some((i, r)) = lay.hero.filter(|x| vis(x.1)) {
            if let Some(d) = m.bt().get(i) {
                self.bt_hero(cv, d, sh(r));
            }
        }
        for (i, r) in &lay.tiles {
            let (Some(d), true) = (m.bt().get(*i), vis(*r)) else { continue };
            let r = sh(*r);
            if m.bt_layout == 1 {
                self.bt_rack(cv, d, r);
            } else {
                self.bt_tile(cv, d, r);
            }
        }
    }

    // ---- List

    fn bt_list(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        self.card(cv, r);
        let on = m.bt().iter().filter(|d| d.connected).count();
        let y = self.header(cv, r, "Paired devices", &format!("{on} connected"), 0);
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let rh = self.s(58);
        let name_w = ((rr - l) as f32 * 0.34).clamp(self.sf(150.0), self.sf(300.0)) as i32;
        let pct_w = self.s(64);
        for (k, d) in m.bt().iter().enumerate() {
            let t = y + k as i32 * rh;
            if k > 0 {
                cv.fill_rect(Rect { l, t, r: rr, b: t + 1 }, self.col.line);
            }
            let cy = t + rh / 2;
            let ic = if d.connected { self.col.fg } else { self.col.dim };
            bt_icon(cv, d.kind, (l + self.s(12)) as f32, cy as f32, self.sf(22.0), ic);
            let nx = l + self.s(36);
            let nm = cv.ellipsize(self.f.semi, &d.name, name_w - self.s(40));
            cv.text(self.f.semi, &nm, nx, cy - self.s(17), ic);
            let sub = format!("{} \u{00B7} {}", state(d).to_uppercase(), kind_label(d.kind));
            let sub = cv.ellipsize(self.f.tiny, &sub, name_w - self.s(40));
            cv.text(self.f.tiny, &sub, nx, cy + self.s(3), if d.connected { self.bt_col(d) } else { self.col.dim });
            // Level: big number on the right, segmented bars between.
            let c = self.bt_col(d);
            let pt = pct_text(level(d));
            let (pw, ph) = cv.measure(m.num_sm_font, &pt);
            let px = rr - self.s(14) - pw;
            cv.numeral(m.num_sm_font, &pt, px, cy - ph / 2, if level(d).is_some() { c } else { self.col.dim });
            cv.text(self.f.tiny, "%", rr - self.s(12), cy - self.s(1), self.col.dim);
            let bl = l + name_w;
            let br = rr - pct_w - self.s(12);
            if br - bl < self.s(40) {
                continue;
            }
            if per_bud(d) {
                let bh = self.s(5);
                for (j, (lab, v)) in [("L", d.left), ("R", d.right), ("C", d.case)].iter().enumerate() {
                    let bt = cy - self.s(13) + j as i32 * self.s(9);
                    cv.text(self.f.tiny, lab, bl, bt - self.s(5), self.col.dim);
                    self.segs(cv, Rect { l: bl + self.s(12), t: bt, r: br, b: bt + bh }, *v, c);
                }
            } else {
                self.segs(cv, Rect { l: bl, t: cy - self.s(5), r: br, b: cy + self.s(5) }, level(d), c);
            }
        }
    }

    /// Dot-matrix level bar: square-ended segments, lit up to `v` percent.
    fn segs(&self, cv: &mut Canvas, r: Rect, v: Option<u8>, c: Rgba) {
        let sw = self.s(4).max(2);
        let gap = self.s(2).max(1);
        let n = ((r.w() + gap) / (sw + gap)).max(1);
        let lit = v.map_or(0, |v| ((f32::from(v) / 100.0) * n as f32).round() as i32);
        let off = self.col.fg.with_a(if self.dark() { 22 } else { 26 });
        for i in 0..n {
            let x = r.l + i * (sw + gap);
            cv.fill_rect(Rect { l: x, t: r.t, r: x + sw, b: r.b }, if i < lit { c } else { off });
        }
    }

    // ---- Rack

    fn bt_rack(&self, cv: &mut Canvas, d: &BtDevice, r: Rect) {
        let m = self.m;
        self.card(cv, r);
        let (l, rr) = (r.l + self.s(12), r.r - self.s(12));
        // Top row: status lamp, name, icon.
        let cy = r.t + self.s(22);
        let lamp = if d.connected { self.bt_col(d) } else { self.col.off };
        if d.connected {
            cv.fill_disc((l + self.s(4)) as f32, cy as f32, self.sf(7.0), lamp.with_a(50));
        }
        cv.fill_disc((l + self.s(4)) as f32, cy as f32, self.sf(3.5), lamp);
        let icon_x = rr - self.s(10);
        bt_icon(cv, d.kind, icon_x as f32, cy as f32, self.sf(18.0), self.col.dim);
        let nm = cv.ellipsize(self.f.cap, &d.name, icon_x - self.s(14) - (l + self.s(14)));
        text_mid(cv, self.f.cap, &nm, l + self.s(14), cy, if d.connected { self.col.fg } else { self.col.dim });
        // Readout well.
        let well = Rect { l, t: r.t + self.s(42), r: rr, b: r.t + self.s(108) };
        cv.fill_round(well, self.sf(6.0), self.sub);
        let num_c = if d.connected { self.col.fg } else { self.col.dim };
        let cols: Vec<(&str, Option<u8>)> = if per_bud(d) { vec![("L", d.left), ("R", d.right), ("CASE", d.case)] } else { vec![("BATT %", level(d))] };
        let mut x = well.l + self.s(10);
        let step = if per_bud(d) { ((well.w() - self.s(110)) / 3).max(self.s(54)) } else { 0 };
        for (lab, v) in &cols {
            self.cap(cv, lab, x, well.t + self.s(8));
            let c = if v.is_some_and(|v| v <= LOW_PCT) && d.connected { self.col.crit } else { num_c };
            cv.numeral(m.num_font, &pct_text(*v), x, well.t + self.s(22), c);
            x += step;
        }
        // State on the right of the well.
        let st = state(d).to_uppercase();
        let kd = kind_label(d.kind);
        let sx = well.r - self.s(10);
        let st = cv.ellipsize(self.f.tiny, &st, self.s(110));
        let (sw, _) = cv.measure(self.f.tiny, &st);
        cv.text(self.f.tiny, &st, sx - sw, well.b - self.s(34), if d.connected { self.bt_col(d) } else { self.col.dim });
        let (kw, _) = cv.measure(self.f.tiny, kd);
        cv.text(self.f.tiny, kd, sx - kw, well.b - self.s(19), self.col.dim);
        // Meter: 16 segments; the top quarter in the accent.
        let mt = Rect { l, t: r.t + self.s(120), r: rr, b: r.t + self.s(134) };
        cv.fill_round(mt, self.sf(4.0), self.sub);
        let g = self.s(2).max(1);
        let inner = Rect { l: mt.l + g, t: mt.t + g, r: mt.r - g, b: mt.b - g };
        let segw = (inner.w() - g * (VU_SEGS - 1)) as f32 / VU_SEGS as f32;
        let lit = level(d).map_or(0, |v| (f32::from(v) / 100.0 * VU_SEGS as f32).round() as i32);
        let off = self.col.fg.with_a(if self.dark() { 18 } else { 22 });
        for i in 0..VU_SEGS {
            let x0 = inner.l + (i as f32 * (segw + g as f32)).round() as i32;
            let x1 = inner.l + (i as f32 * (segw + g as f32) + segw).round() as i32;
            let c = if i >= lit {
                off
            } else if !d.connected {
                self.col.dim.with_a(120)
            } else if is_low(d) {
                self.col.crit
            } else if i >= VU_SEGS - 4 {
                self.col.accent
            } else {
                self.col.fg.with_a(if self.dark() { 200 } else { 220 })
            };
            cv.fill_round(Rect { l: x0, t: inner.t, r: x1, b: inner.b }, self.sf(1.5), c);
        }
    }

    // ---- Hero

    fn bt_hero(&self, cv: &mut Canvas, d: &BtDevice, r: Rect) {
        let m = self.m;
        // Inverted tile: the text colour as the fill, so it stands apart in both themes.
        let fill = self.col.fg;
        // Opaque: `card` is translucent white under Mica in dark mode, which vanished on this fill.
        let ink = self.col.bubble;
        let dim = ink.mix(fill, 0.45);
        cv.fill_round(r, self.sf(16.0), fill);
        let (l, rr) = (r.l + self.s(20), r.r - self.s(20));
        let wide = r.w() >= self.s(520);
        // Chip, name, state.
        let chip = "In use";
        let (cw, ch) = cv.measure(self.f.tiny, chip);
        let cr = Rect { l, t: r.t + self.s(20), r: l + cw + self.s(34), b: r.t + self.s(20) + ch + self.s(8) };
        cv.fill_round(cr, cr.h() as f32 / 2.0, self.col.accent);
        bt_icon(cv, d.kind, (cr.l + self.s(12)) as f32, ((cr.t + cr.b) / 2) as f32, self.sf(12.0), fill);
        text_mid(cv, self.f.tiny, chip, cr.l + self.s(22), (cr.t + cr.b) / 2, fill);
        let text_r = if wide { r.l + r.w() / 2 } else { rr };
        let nm = cv.ellipsize(self.f.head, &d.name, text_r - l);
        cv.text(self.f.head, &nm, l, cr.b + self.s(10), ink);
        let sub = if per_bud(d) { format!("{} \u{00B7} left, right and case", state(d)) } else { format!("{} \u{00B7} {}", state(d), kind_label(d.kind).to_lowercase()) };
        let sub = cv.ellipsize(self.f.small, &sub, text_r - l);
        cv.text(self.f.small, &sub, l, cr.b + self.s(38), dim);
        // Rings: right half when wide, a row below when narrow.
        let rings: Vec<(&str, Option<u8>)> = if per_bud(d) { vec![("LEFT", d.left), ("RIGHT", d.right), ("CASE", d.case)] } else { vec![("LEVEL", level(d))] };
        let rad = self.sf(38.0);
        let thick = self.sf(5.0);
        let step = rad * 2.0 + self.sf(16.0);
        let (mut cx, cy) = if wide {
            (rr as f32 - rad - (rings.len() as f32 - 1.0) * step, (r.t + r.b) as f32 / 2.0)
        } else {
            (l as f32 + rad, r.b as f32 - self.sf(20.0) - rad)
        };
        for (lab, v) in rings {
            cv.ring(cx, cy, rad, thick, 1.0, ink.with_a(40));
            let c = if v.is_some_and(|v| v <= LOW_PCT) { self.col.crit } else { self.col.accent };
            if let Some(v) = v {
                cv.ring(cx, cy, rad, thick, f32::from(v) / 100.0, c);
            }
            let t = pct_text(v);
            let (_, th) = cv.measure(m.num_sm_font, &t);
            cv.numeral_center(m.num_sm_font, &t, cx as i32, cy as i32 - th / 2 - self.s(5), ink);
            cv.text_center(self.f.tiny, lab, cx as i32, cy as i32 + self.s(11), dim);
            cx += step;
        }
    }

    fn bt_tile(&self, cv: &mut Canvas, d: &BtDevice, r: Rect) {
        let m = self.m;
        self.card(cv, r);
        let (l, rr) = (r.l + self.s(14), r.r - self.s(14));
        let ic = if d.connected { self.col.fg } else { self.col.dim };
        bt_icon(cv, d.kind, (l + self.s(11)) as f32, (r.t + self.s(24)) as f32, self.sf(22.0), ic);
        let dot = if d.connected { self.col.good } else { self.col.off };
        cv.fill_disc((rr - self.s(4)) as f32, (r.t + self.s(24)) as f32, self.sf(4.0), dot);
        let c = self.bt_col(d);
        let t = pct_text(level(d));
        let tw = cv.numeral(m.num_font, &t, l, r.t + self.s(50), if level(d).is_some() && d.connected { if is_low(d) { self.col.crit } else { self.col.fg } } else { self.col.dim });
        let tw = tw.max(cv.measure(m.num_font, &t).0);
        cv.text(self.f.small, "%", l + tw + self.s(3), r.t + self.s(68), self.col.dim);
        let bar = Rect { l, t: r.t + self.s(100), r: rr, b: r.t + self.s(106) };
        match level(d) {
            Some(v) => self.hbar(cv, bar, f32::from(v) / 100.0, c),
            None => cv.meter(bar, 0.0, self.track, self.track),
        }
        let nm = cv.ellipsize(self.f.small, &d.name, rr - l);
        cv.text(self.f.small, &nm, l, r.t + self.s(116), self.col.dim);
    }

    // ---- Overview card

    pub(super) fn bt_card(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let b = m.bt();
        let on = b.iter().filter(|d| d.connected).count();
        let low = b.iter().filter(|d| is_low(d)).count();
        let pw = if low > 0 { self.pill(cv, r, &format!("{low} low"), self.col.crit, true) } else { 0 };
        let y = self.header(cv, r, "Bluetooth", &format!("{on} connected"), pw + self.s(8));
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        for (k, d) in b.iter().take(CARD_ROWS).enumerate() {
            let cy = y + k as i32 * self.s(34) + self.s(14);
            let ic = if d.connected { self.col.fg } else { self.col.dim };
            bt_icon(cv, d.kind, (l + self.s(9)) as f32, cy as f32, self.sf(17.0), ic);
            let pct = level(d).map_or("--".into(), |v| format!("{v}%"));
            let (pw, _) = cv.measure(self.f.cap, &pct);
            text_mid(cv, self.f.cap, &pct, rr - pw, cy, if d.connected { if is_low(d) { self.col.crit } else { self.col.fg } } else { self.col.dim });
            let nx = l + self.s(28);
            let bar_w = ((rr - nx) as f32 * 0.32) as i32;
            let bar_r = rr - pw - self.s(10);
            let nm = cv.ellipsize(self.f.small, &d.name, bar_r - bar_w - self.s(10) - nx);
            text_mid(cv, self.f.small, &nm, nx, cy, ic);
            let bar = Rect { l: bar_r - bar_w, t: cy - self.s(3), r: bar_r, b: cy + self.s(3) };
            match level(d) {
                Some(v) => self.hbar(cv, bar, f32::from(v) / 100.0, self.bt_col(d)),
                None => cv.meter(bar, 0.0, self.track, self.track),
            }
        }
        if b.len() > CARD_ROWS {
            let t = y + CARD_ROWS as i32 * self.s(34) + self.s(2);
            self.cap(cv, &format!("+{} MORE \u{00B7} OPEN FOR ALL", b.len() - CARD_ROWS), l, t);
        }
    }
}

// ------------------------------------------------------------------ icons

/// Points of a rounded rectangle outline in icon units (24 x 24 box), closed.
fn rrect(x: f32, y: f32, w: f32, h: f32, rad: f32) -> Vec<(f32, f32)> {
    let rad = rad.min(w / 2.0).min(h / 2.0);
    let mut v = Vec::with_capacity(36);
    let corners = [(x + w - rad, y + rad, -90.0f32), (x + w - rad, y + h - rad, 0.0), (x + rad, y + h - rad, 90.0), (x + rad, y + rad, 180.0)];
    for (cx, cy, a0) in corners {
        for k in 0..=6 {
            let a = (a0 + k as f32 * 15.0).to_radians();
            v.push((cx + rad * a.cos(), cy + rad * a.sin()));
        }
    }
    v.push(v[0]);
    v
}

/// Arc points (degrees, clockwise from 3 o'clock, y down) in icon units.
fn arc(cx: f32, cy: f32, rad: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
    let n = (((a1 - a0).abs() / 12.0).ceil() as usize).max(2);
    (0..=n)
        .map(|k| {
            let a = (a0 + (a1 - a0) * k as f32 / n as f32).to_radians();
            (cx + rad * a.cos(), cy + rad * a.sin())
        })
        .collect()
}

/// Stroked device icon centred on (cx, cy), `size` px across.
pub(super) fn bt_icon(cv: &mut Canvas, k: Kind, cx: f32, cy: f32, size: f32, c: Rgba) {
    let sc = size / 24.0;
    let th = (1.7 * sc).max(1.2);
    let mut paths: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut dots: Vec<(f32, f32)> = Vec::new();
    match k {
        Kind::Earbuds => {
            paths.push(rrect(3.0, 3.5, 7.0, 8.5, 3.5));
            paths.push(rrect(6.0, 10.5, 3.2, 10.0, 1.6));
            paths.push(rrect(14.0, 3.5, 7.0, 8.5, 3.5));
            paths.push(rrect(14.8, 10.5, 3.2, 10.0, 1.6));
        }
        Kind::Headphones | Kind::Headset => {
            paths.push(arc(12.0, 13.0, 8.5, 180.0, 360.0));
            paths.push(rrect(2.5, 13.5, 4.5, 7.0, 2.0));
            paths.push(rrect(17.0, 13.5, 4.5, 7.0, 2.0));
            if k == Kind::Headset {
                paths.push(vec![(19.3, 20.5), (19.3, 21.5), (13.5, 21.8)]);
            }
        }
        Kind::Speaker => {
            paths.push(rrect(5.5, 2.5, 13.0, 19.0, 3.0));
            paths.push(arc(12.0, 14.5, 3.3, 0.0, 360.0));
            dots.push((12.0, 7.0));
        }
        Kind::Mouse => {
            paths.push(rrect(6.5, 3.0, 11.0, 18.0, 5.5));
            paths.push(vec![(12.0, 7.0), (12.0, 10.0)]);
        }
        Kind::Keyboard => {
            paths.push(rrect(2.5, 6.5, 19.0, 11.0, 2.5));
            dots.extend([(6.0, 10.0), (9.5, 10.0), (13.0, 10.0), (16.5, 10.0)]);
            paths.push(vec![(7.5, 14.0), (16.5, 14.0)]);
        }
        Kind::Gamepad => {
            paths.push(rrect(2.0, 7.0, 20.0, 10.5, 5.0));
            paths.push(vec![(7.5, 10.5), (7.5, 14.0)]);
            paths.push(vec![(5.75, 12.25), (9.25, 12.25)]);
            dots.extend([(16.0, 11.0), (18.0, 13.5)]);
        }
        Kind::Pen => {
            paths.push(vec![(16.5, 3.5), (20.5, 7.5), (9.0, 19.0), (4.0, 20.0), (5.0, 15.0), (16.5, 3.5)]);
            paths.push(vec![(14.0, 6.0), (18.0, 10.0)]);
        }
        Kind::Phone => {
            paths.push(rrect(6.5, 2.5, 11.0, 19.0, 2.8));
            paths.push(vec![(10.5, 18.5), (13.5, 18.5)]);
        }
        Kind::Watch => {
            paths.push(rrect(6.5, 6.0, 11.0, 12.0, 3.0));
            paths.push(vec![(9.0, 6.0), (9.5, 2.5), (14.5, 2.5), (15.0, 6.0)]);
            paths.push(vec![(9.0, 18.0), (9.5, 21.5), (14.5, 21.5), (15.0, 18.0)]);
        }
        Kind::Computer => {
            paths.push(rrect(3.0, 4.5, 18.0, 12.0, 2.0));
            paths.push(vec![(12.0, 16.5), (12.0, 20.0)]);
            paths.push(vec![(8.0, 20.0), (16.0, 20.0)]);
        }
        Kind::Other => {
            // The Bluetooth rune.
            paths.push(vec![(7.0, 7.5), (17.0, 16.5), (12.0, 21.0), (12.0, 3.0), (17.0, 7.5), (7.0, 16.5)]);
        }
    }
    let (ox, oy) = (cx - 12.0 * sc, cy - 12.0 * sc);
    for p in paths {
        let pts: Vec<(f32, f32)> = p.iter().map(|(x, y)| (ox + x * sc, oy + y * sc)).collect();
        cv.stroke_poly(&pts, th, |_, _| c);
    }
    for (x, y) in dots {
        cv.fill_disc(ox + x * sc, oy + y * sc, th * 0.65, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(battery: Option<u8>, left: Option<u8>, right: Option<u8>) -> BtDevice {
        BtDevice { battery, left, right, connected: true, ..Default::default() }
    }

    #[test]
    fn level_takes_the_lower_bud() {
        assert_eq!(level(&dev(Some(70), Some(90), Some(40))), Some(40));
        assert_eq!(level(&dev(Some(70), None, None)), Some(70));
        assert_eq!(level(&dev(None, None, None)), None);
    }

    #[test]
    fn low_only_when_connected() {
        let mut d = dev(Some(15), None, None);
        assert!(is_low(&d));
        d.connected = false;
        assert!(!is_low(&d));
    }
}
