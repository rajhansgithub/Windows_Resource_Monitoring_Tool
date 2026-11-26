//! The Server Room: the Achievements page of the main window, drawn in software
//! onto the window's `Canvas`. The tray's medal is the live 3D coin (`ui::Stage`,
//! rendered off screen on the GPU), or a flat disc when there is no GPU.
//!
//! Every badge is a 1U unit in a rack. A unit shows seven tier LEDs, how far the
//! next tier is, and a thin progress bar. A rack is certified at a tier when all
//! of its units reach it; its placard names the units that still hold it back.
//! Rack 04 · Records holds Still Here and the singles, one unit per category.
//! Clicking a unit pulls it out and opens the tray below the racks: its coin (drag
//! to turn, click to flip, Polish when it gathered dust) and what the next tier needs.
//!
//! Also here: the Overview "Next goal" card and the level chip of the nav pane.
//! Layout is in content coordinates; drawing takes the scroll offset `dy`.

use super::core::{self as c, Category, Catalog, Goal, State};
use crate::insights::view::{Colors, Fonts};
use crate::render::{measure_dc, Canvas, Rgba};
use crate::util::Rect;
use super::ui::{wear_words, Stage};
use windows_sys::Win32::Graphics::Gdi::HDC;

const fn hx(v: u32) -> Rgba {
    Rgba((v >> 16) as u8, (v >> 8) as u8, v as u8, 255)
}

/// Tier colours (Bronze .. Emulsion), as in the medal case.
pub const TIER_HEX: [u32; 7] = [0xc07f45, 0xc9cfd8, 0xeac35e, 0xa9e4e1, 0xcbe7ff, 0xff4757, 0xb49cff];

pub fn tier_col(t: u8) -> Rgba {
    hx(TIER_HEX[(t.clamp(1, 7) - 1) as usize])
}

// The room is a physical place: steel racks under cold-aisle light, the same in both themes.
const FLOOR: Rgba = hx(0x0d1015);
const FLOOR_LINE: Rgba = hx(0x1b2028);
const STEEL: Rgba = hx(0x1c1f24);
const STEEL_EDGE: Rgba = hx(0x2a2f37);
const UNIT: Rgba = hx(0x262a31);
const UNIT_HOT: Rgba = hx(0x2f343c);
const LED_OFF: Rgba = hx(0x3a3f48);
const INK: Rgba = hx(0xe6e9ee);
const INK_DIM: Rgba = hx(0x8b93a1);
const TAPE: Rgba = hx(0xe9e2cf);
const TAPE_INK: Rgba = hx(0x1c1a14);
const COLD: Rgba = hx(0x6fb7ff);
const LINK: Rgba = hx(0x3be08a);
const AMBER: Rgba = hx(0xffb020);
const VENT: Rgba = hx(0x0c0e12);

/// What a unit stands for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Unit {
    Ladder(&'static str),
    Singles(Category),
    /// A blank 1U filler panel (racks with fewer units).
    Blank,
}

/// Singles wear a fixed metal (same as the medal case).
fn single_tier(cat: Category) -> u8 {
    match cat {
        Category::Start => 1,
        Category::Accident => 2,
        Category::Secret => 3,
        _ => 4,
    }
}

fn cat_name(cat: Category) -> &'static str {
    match cat {
        Category::Start => "Getting started",
        Category::Mastery => "Mastery",
        Category::Accident => "Happy accidents",
        Category::Secret => "Secrets",
        Category::Ladder => "Ladders",
    }
}

const RACK_UNITS: usize = 5;

fn ease(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

/// One laid-out unit.
struct UnitL {
    unit: Unit,
    r: Rect,
    title: String,
    sub: String,
    /// (lit, of, colour per lit LED)
    leds: (u8, u8, [Rgba; 7]),
    bar: Option<(f32, Rgba)>,
    live: bool,
}

struct RackL {
    r: Rect,
    tape: &'static str,
    stamp: String,
    stamp_col: Rgba,
    hint: Option<String>,
}

/// One line of the tray.
enum TrayLine {
    Text { x: i32, y: i32, s: String, dim: bool, semi: bool },
    /// The large view's heading (head font).
    Title { x: i32, y: i32, s: String },
    Single { x: i32, y: i32, w: i32, got: bool, name: String, xp: String, line: String },
}

pub struct Room {
    dpi: u32,
    head: Rect,
    level: (u32, u64, u64, &'static str),
    head_right: String,
    floor: Rect,
    racks: Vec<RackL>,
    units: Vec<UnitL>,
    tray: Rect,
    tray_lines: Vec<TrayLine>,
    /// The tray's medal disc: centre, radius, tier, progress to the next tier.
    disc: Option<(i32, i32, i32, u8, f32, u8)>,
    /// The tray's "Polish" button (empty when the medal is clean or there is none).
    pub polish_btn: Rect,
    /// The tray's "View large" button (empty when there is no coin).
    pub big_btn: Rect,
    /// The large medal view: the whole page is one medal. Its buttons: back to the racks, previous, next.
    pub big: bool,
    /// Open the large view at the first layout (CI screenshots: `RESMON_ACH_BIG`).
    want_big: bool,
    pub close_btn: Rect,
    pub prev_btn: Rect,
    pub next_btn: Rect,
    /// The live coin, where it sits (content coordinates, square) and its side in px.
    pub coin: Stage,
    pub coin_rect: Rect,
    coin_n: u32,
    /// The badge the tray's coin shows (ladder, or the picked single).
    pub shown: Option<&'static str>,
    /// Singles tray: the picked single, and the earned rows that take clicks (id, rect).
    single: Option<&'static str>,
    pub single_rects: Vec<(&'static str, Rect)>,
    /// Selected unit (index into the units) and how far it is pulled out (0..1, animated).
    pub sel: usize,
    pull: f32,
}

impl Room {
    pub fn new() -> Room {
        Room {
            dpi: 96,
            head: Rect::default(),
            level: (1, 0, 1, ""),
            head_right: String::new(),
            floor: Rect::default(),
            racks: Vec::new(),
            units: Vec::new(),
            tray: Rect::default(),
            tray_lines: Vec::new(),
            disc: None,
            polish_btn: Rect::default(),
            big_btn: Rect::default(),
            big: false,
            want_big: std::env::var_os("RESMON_ACH_BIG").is_some(),
            close_btn: Rect::default(),
            prev_btn: Rect::default(),
            next_btn: Rect::default(),
            coin: Stage::new(),
            coin_rect: Rect::default(),
            coin_n: 0,
            shown: None,
            single: None,
            single_rects: Vec::new(),
            sel: usize::MAX,
            pull: 0.0,
        }
    }

    fn sc(&self, v: i32) -> i32 {
        (v * self.dpi as i32 + 48) / 96
    }

    /// The units in page order: rack by rack, top to bottom.
    pub fn plan() -> Vec<Unit> {
        let mut v = Vec::new();
        for set in Catalog::sets() {
            v.extend(set.members.iter().map(|m| Unit::Ladder(m)));
            v.extend(std::iter::repeat_n(Unit::Blank, RACK_UNITS - set.members.len()));
        }
        v.push(Unit::Ladder("stillhere"));
        v.extend([Category::Start, Category::Mastery, Category::Accident, Category::Secret].map(Unit::Singles));
        v
    }

    /// Index of the unit for badge `id` (ladder or single).
    pub fn index_of(id: &str) -> Option<usize> {
        let b = Catalog::get(id)?;
        Room::plan().iter().position(|u| match u {
            Unit::Ladder(x) => *x == id,
            Unit::Singles(cat) => *cat == b.category && !b.is_ladder(),
            Unit::Blank => false,
        })
    }

    /// Unit rectangles that take clicks: (index, rect), content coordinates.
    pub fn hit_rects(&self) -> Vec<(usize, Rect)> {
        self.units.iter().enumerate().filter(|(_, u)| u.unit != Unit::Blank).map(|(i, u)| (i, u.r)).collect()
    }

    /// Pick a unit (pulls it out). Returns true when the selection changed.
    pub fn select(&mut self, i: usize) -> bool {
        if i == self.sel || self.units.get(i).is_none_or(|u| u.unit == Unit::Blank) {
            return false;
        }
        self.sel = i;
        self.pull = 0.0;
        self.single = None;
        true
    }

    /// Pick an earned single in the tray (its coin shows). True when it changed.
    pub fn pick_single(&mut self, id: &'static str) -> bool {
        if self.single == Some(id) {
            return false;
        }
        self.single = Some(id);
        true
    }

    /// One animation step (`dt` seconds): the pull-out and the coin. True while anything still moves.
    pub fn tick(&mut self, dt: f32, animate: bool, s: &State) -> bool {
        let coin = self.coin.tick(s, self.coin_n);
        if self.pull >= 1.0 {
            return coin;
        }
        self.pull = if animate { (self.pull + dt / 0.22).min(1.0) } else { 1.0 };
        true
    }

    /// Lay the page out from `s` between `l` and `r`, starting at `y0`. Returns the bottom.
    #[allow(clippy::too_many_arguments)]
    /// `vh`: the height of the visible page area (the large view fills it).
    pub fn layout(&mut self, s: &State, mdc: HDC, f: &Fonts, dpi: u32, l: i32, r: i32, y0: i32, vh: i32) -> i32 {
        self.dpi = dpi;
        self.big_btn = Rect::default();
        self.close_btn = Rect::default();
        self.prev_btn = Rect::default();
        self.next_btn = Rect::default();
        if self.big && self.shown.is_some() {
            return self.layout_big(s, mdc, f, l, r, y0, vh);
        }
        self.big = false;
        let lh = |font| measure_dc(mdc, font, "Ag").1;
        let w = r - l;

        // Header: level ring, title, XP; badges and certificates on the right.
        let xp = c::total_xp(s);
        let (lv, into, need) = c::level_progress(xp);
        self.level = (lv, into, need, c::title_of(lv));
        let earned = Catalog::all().iter().filter(|b| if b.is_ladder() { s.tier(b.id) > 0 } else { s.earned(b.id) }).count();
        let certs = Catalog::sets().iter().filter(|set| c::rack_status(s, set).cert > 0).count();
        self.head_right = format!("{} of {} badges \u{00B7} {} of {} racks certified", earned, Catalog::all().len(), certs, Catalog::sets().len());
        self.head = Rect { l, t: y0, r, b: y0 + self.sc(76) };

        // Racks: 4 across when they fit, else 2, else 1.
        let pad = self.sc(16);
        let gap = self.sc(12);
        let min_w = self.sc(184);
        let inner = w - 2 * pad;
        let cols = if inner >= 4 * min_w + 3 * gap { 4 } else if inner >= 2 * min_w + gap { 2 } else { 1 };
        let rack_w = (inner - gap * (cols - 1)) / cols;
        let unit_h = self.sc(46);
        let unit_gap = self.sc(6);
        let rack_top = self.sc(36);
        let hint_h = self.sc(20);
        let rack_h = rack_top + RACK_UNITS as i32 * (unit_h + unit_gap) + hint_h + self.sc(30);
        let floor_t = self.head.b + self.sc(16);

        let plan = Room::plan();
        self.units.clear();
        self.racks.clear();
        let n_racks = Catalog::sets().len() + 1;
        for k in 0..n_racks {
            let (col, row) = (k as i32 % cols, k as i32 / cols);
            let rx = l + pad + col * (rack_w + gap);
            let ry = floor_t + pad + row * (rack_h + gap);
            let rr = Rect { l: rx, t: ry, r: rx + rack_w, b: ry + rack_h };
            let units = &plan[k * RACK_UNITS..(k + 1) * RACK_UNITS];
            let (tape, stamp, stamp_col, hint) = if let Some(set) = Catalog::sets().get(k) {
                let st = c::rack_status(s, set);
                let stamp = if st.cert > 0 { format!("{} CERT", c::TIER_NAMES[(st.cert - 1) as usize].to_uppercase()) } else {
                    let nx = st.next.unwrap_or(1);
                    format!("{}/4 {}", 4 - st.blockers.len(), c::TIER_NAMES[(nx - 1) as usize].to_uppercase())
                };
                let col = if st.cert > 0 { tier_col(st.cert) } else { INK_DIM };
                let hint = match (st.next, st.blockers.as_slice()) {
                    (Some(nx), [one]) => Some(format!("{} holds the {} cert", Catalog::get(one).map_or(*one, |b| b.name), c::TIER_NAMES[(nx - 1) as usize])),
                    (Some(nx), b) if b.len() == 2 => Some(format!("2 units from {} cert", c::TIER_NAMES[(nx - 1) as usize])),
                    _ => None,
                };
                (set.name, stamp, col, hint)
            } else {
                let got = Catalog::all().iter().filter(|b| !b.is_ladder() && s.earned(b.id)).count();
                let all = Catalog::all().iter().filter(|b| !b.is_ladder()).count();
                ("Rack 04 \u{00B7} Records", format!("{got}/{all}"), INK_DIM, None)
            };
            self.racks.push(RackL { r: rr, tape, stamp, stamp_col, hint });
            for (j, u) in units.iter().enumerate() {
                let ut = ry + rack_top + j as i32 * (unit_h + unit_gap);
                let ur = Rect { l: rx + self.sc(10), t: ut, r: rx + rack_w - self.sc(10), b: ut + unit_h };
                self.units.push(unit_l(s, *u, ur));
            }
        }
        let rows = (n_racks as i32 + cols - 1) / cols;
        let racks_b = floor_t + pad + rows * rack_h + (rows - 1) * gap;

        // Selection: keep it; first time, the next goal's unit.
        if self.sel >= self.units.len() || self.units[self.sel].unit == Unit::Blank {
            self.sel = c::next_goal(s).and_then(|g| Room::index_of(g.id)).unwrap_or(0);
            self.pull = 1.0;
        }

        // Tray: the selected unit, pulled out.
        let tl = l + pad;
        let tt = racks_b + self.sc(14);
        let tr = r - pad;
        self.tray_lines.clear();
        self.disc = None;
        self.polish_btn = Rect::default();
        self.coin_rect = Rect::default();
        self.single_rects.clear();
        self.shown = None;
        let mut y = tt + self.sc(16);
        // the coin grows with the window: 148 px at the default size, up to 300
        let side = ((r - l) / 6).clamp(self.sc(148), self.sc(300));
        let now = super::observer::unix_ms();
        match self.units[self.sel].unit {
            Unit::Ladder(id) => {
                let b = Catalog::get(id).unwrap();
                let k = s.tier(id);
                let goal = c::goal_of(s, id);
                let rad = side / 2;
                let dcx = tl + self.sc(14) + rad;
                self.disc = Some((dcx, y + rad, rad, k, goal.map_or(1.0, |g| g.frac()), goal.map_or(k, |g| g.tier)));
                if k > 0 {
                    self.coin_rect = Rect { l: dcx - rad, t: y, r: dcx + rad, b: y + side };
                    self.shown = Some(id);
                }
                let x = dcx + rad + self.sc(22);
                let title = if k > 0 { format!("{} \u{00B7} {}", b.name, c::TIER_NAMES[(k - 1) as usize]) } else { format!("{} \u{00B7} not started", b.name) };
                self.tray_lines.push(TrayLine::Text { x, y, s: title, dim: false, semi: true });
                y += lh(f.semi) + self.sc(2);
                self.tray_lines.push(TrayLine::Text { x, y, s: b.line.to_string(), dim: true, semi: false });
                y += lh(f.small) + self.sc(8);
                let have = c::amount_of(s, b);
                let mut kv = format!("NOW {}", c::fmt_amount(have, b.unit));
                if let Some(g) = goal {
                    kv.push_str(&format!("   \u{00B7}   NEXT {} at {}   \u{00B7}   {} to go   \u{00B7}   +{} XP", g.tier_name(), c::fmt_amount(g.to as f64, g.unit), c::fmt_amount(g.left(), g.unit), g.xp));
                } else {
                    kv.push_str("   \u{00B7}   top tier reached");
                }
                self.tray_lines.push(TrayLine::Text { x, y, s: kv, dim: false, semi: false });
                y += lh(f.small) + self.sc(12);
                if k > 0 {
                    y = self.wear_lines(s, id, now, mdc, f, x, y);
                }
                y = y.max(tt + self.sc(16) + side) + self.sc(16);
            }
            Unit::Singles(cat) => {
                let all: Vec<_> = Catalog::all().iter().filter(|b| b.category == cat).collect();
                let got = all.iter().filter(|b| s.earned(b.id)).count();
                // the coin of the picked single (first earned one by default), left of the list
                if self.single.is_none_or(|id| !all.iter().any(|b| b.id == id && s.earned(id))) {
                    self.single = all.iter().find(|b| s.earned(b.id)).map(|b| b.id);
                }
                let top = y;
                let mut x = tl + self.sc(18);
                if let Some(id) = self.single {
                    self.coin_rect = Rect { l: tl + self.sc(14), t: y, r: tl + self.sc(14) + side, b: y + side };
                    let k = single_tier(cat);
                    self.disc = Some(((self.coin_rect.l + self.coin_rect.r) / 2, y + side / 2, side / 2, k, 1.0, k));
                    self.shown = Some(id);
                    x = self.coin_rect.r + self.sc(22);
                }
                self.tray_lines.push(TrayLine::Text { x, y, s: format!("{} \u{00B7} {} of {}", cat_name(cat), got, all.len()), dim: false, semi: true });
                y += lh(f.semi) + self.sc(10);
                let cols2 = if tr - x >= self.sc(520) { 2 } else { 1 };
                let cw = (tr - x - self.sc(18) - self.sc(16) * (cols2 - 1)) / cols2;
                let row_h = lh(f.body) + lh(f.small) + self.sc(10);
                for (i, b) in all.iter().enumerate() {
                    let (cx, cy) = (x + (i as i32 % cols2) * (cw + self.sc(16)), y + (i as i32 / cols2) * row_h);
                    let e = s.earned(b.id);
                    let hidden = cat == Category::Secret && !e;
                    if e {
                        self.single_rects.push((b.id, Rect { l: cx - self.sc(6), t: cy - self.sc(3), r: cx + cw + self.sc(6), b: cy + row_h - self.sc(5) }));
                    }
                    self.tray_lines.push(TrayLine::Single {
                        x: cx,
                        y: cy,
                        w: cw,
                        got: e,
                        name: if hidden { "? ? ?".into() } else { b.name.to_string() },
                        xp: format!("+{} XP", b.xp),
                        line: if hidden { "Hidden until earned".into() } else { b.line.to_string() },
                    });
                }
                y += ((all.len() as i32 + cols2 - 1) / cols2) * row_h + self.sc(8);
                if let Some(id) = self.single {
                    y = self.wear_lines(s, id, now, mdc, f, x, y + self.sc(4));
                    y = y.max(top + side) + self.sc(16);
                }
            }
            Unit::Blank => {}
        }
        match self.shown {
            Some(id) => {
                let tier = Catalog::get(id).map_or(1, |b| if b.is_ladder() { s.tier(id) } else { single_tier(b.category) });
                self.coin.show(id, tier.clamp(1, 7));
                self.coin_n = self.coin_rect.w().max(0) as u32;
            }
            None => self.coin.hide(),
        }
        self.tray = Rect { l: tl, t: tt, r: tr, b: y };
        self.floor = Rect { l, t: floor_t, r, b: self.tray.b + pad };
        if std::mem::take(&mut self.want_big) && self.open_big() {
            return self.layout_big(s, mdc, f, l, r, y0, vh);
        }
        self.floor.b
    }

    /// The medal's age (dust, cobwebs) and, when it is not clean, the Polish button. Returns the bottom.
    #[allow(clippy::too_many_arguments)]
    fn wear_lines(&mut self, s: &State, id: &'static str, now: u64, mdc: HDC, f: &Fonts, x: i32, mut y: i32) -> i32 {
        let w = s.wear(id, now);
        self.tray_lines.push(TrayLine::Text { x, y, s: wear_words(&w), dim: true, semi: false });
        y += measure_dc(mdc, f.small, "Ag").1 + self.sc(10);
        let mut bx = x;
        if !self.big {
            let bw = measure_dc(mdc, f.body, "View large").0 + self.sc(32);
            self.big_btn = Rect { l: bx, t: y, r: bx + bw, b: y + self.sc(32) };
            bx = self.big_btn.r + self.sc(8);
        }
        if !w.clean {
            let bw = measure_dc(mdc, f.body, "Polish").0 + self.sc(32);
            self.polish_btn = Rect { l: bx, t: y, r: bx + bw, b: y + self.sc(32) };
        }
        if !self.big || !w.clean {
            y += self.sc(32);
        }
        y
    }

    /// The medals that can be shown large, in page order: ladders with a tier, then the earned singles.
    fn earned_order(s: &State) -> Vec<&'static str> {
        let mut v = Vec::new();
        for u in Room::plan() {
            match u {
                Unit::Ladder(id) if s.tier(id) > 0 => v.push(id),
                Unit::Singles(cat) => v.extend(Catalog::all().iter().filter(|b| b.category == cat && !b.is_ladder() && s.earned(b.id)).map(|b| b.id)),
                _ => {}
            }
        }
        v
    }

    /// Open the large view on the medal the tray shows. False when there is none.
    pub fn open_big(&mut self) -> bool {
        if self.shown.is_none() {
            return false;
        }
        self.big = true;
        true
    }

    pub fn close_big(&mut self) {
        self.big = false;
    }

    /// Large view: the previous (-1) or next (+1) earned medal; the racks follow (unit and single picked).
    pub fn step(&mut self, s: &State, d: i32) {
        let order = Room::earned_order(s);
        if order.is_empty() {
            return;
        }
        let cur = self.shown.and_then(|id| order.iter().position(|x| *x == id)).unwrap_or(0) as i32;
        let id = order[(cur + d).rem_euclid(order.len() as i32) as usize];
        if let Some(i) = Room::index_of(id) {
            self.sel = i;
            self.pull = 1.0;
        }
        self.single = Catalog::get(id).filter(|b| !b.is_ladder()).map(|b| b.id);
        self.shown = Some(id);
    }

    /// The large view: one medal filling the page (coin as big as fits), its name, the way to the next tier, its age.
    #[allow(clippy::too_many_arguments)]
    fn layout_big(&mut self, s: &State, mdc: HDC, f: &Fonts, l: i32, r: i32, y0: i32, vh: i32) -> i32 {
        let lh = |font| measure_dc(mdc, font, "Ag").1;
        let id = self.shown.unwrap();
        let Some(b) = Catalog::get(id) else {
            self.big = false;
            return y0;
        };
        self.units.clear();
        self.racks.clear();
        self.tray_lines.clear();
        self.single_rects.clear();
        self.polish_btn = Rect::default();
        self.head = Rect::default();
        let pad = self.sc(20);
        let bottom = y0 + vh.max(self.sc(420)) - self.sc(12);
        self.floor = Rect { l, t: y0, r, b: bottom };

        // top row: back to the racks on the left, previous / next on the right
        let bh = self.sc(32);
        let y = y0 + pad;
        let b32 = self.sc(32);
        let bw = |t: &str| measure_dc(mdc, f.body, t).0 + b32;
        self.close_btn = Rect { l: l + pad, t: y, r: l + pad + bw("Back to racks"), b: y + bh };
        if Room::earned_order(s).len() > 1 {
            let nw = bw("Next");
            self.next_btn = Rect { l: r - pad - nw, t: y, r: r - pad, b: y + bh };
            let pw = bw("Previous");
            self.prev_btn = Rect { l: self.next_btn.l - self.sc(8) - pw, t: y, r: self.next_btn.l - self.sc(8), b: y + bh };
        }
        let top = y + bh + pad;

        // the facts, measured first so the coin gets the rest
        let ladder = b.is_ladder();
        let k = if ladder { s.tier(id) } else { single_tier(b.category) };
        let goal = if ladder { c::goal_of(s, id) } else { None };
        let title = format!("{} \u{00B7} {}", b.name, c::TIER_NAMES[(k.clamp(1, 7) - 1) as usize]);
        let mut kv = String::new();
        if ladder {
            kv = format!("NOW {}", c::fmt_amount(c::amount_of(s, b), b.unit));
            match goal {
                Some(g) => kv.push_str(&format!(
                    "   \u{00B7}   NEXT {} at {}   \u{00B7}   {} to go   \u{00B7}   +{} XP",
                    g.tier_name(),
                    c::fmt_amount(g.to as f64, g.unit),
                    c::fmt_amount(g.left(), g.unit),
                    g.xp
                )),
                None => kv.push_str("   \u{00B7}   top tier reached"),
            }
        }
        let info_h = lh(f.head) + self.sc(6) + lh(f.small) + self.sc(10) + if kv.is_empty() { 0 } else { lh(f.small) + self.sc(12) } + lh(f.small) + self.sc(10) + bh;
        let w = r - l - 2 * pad;
        let h = bottom - pad - top;
        // wide: coin left, facts right; narrow: coin on top, facts below
        let info_w = self.sc(380);
        let side_by_side = w - h >= info_w;
        let side = if side_by_side { h.min(w - info_w - pad) } else { (h - info_h - pad).min(w) }.max(self.sc(160));
        let (stage, x, mut ty) = if side_by_side {
            let st = Rect { l: l + pad, t: top, r: l + pad + side, b: top + side };
            (st, st.r + self.sc(32), top + (side - info_h).max(0) / 2)
        } else {
            let sl = l + (r - l - side) / 2;
            let st = Rect { l: sl, t: top, r: sl + side, b: top + side };
            (st, l + pad, st.b + pad)
        };
        self.coin_rect = stage;
        self.disc = Some(((stage.l + stage.r) / 2, (stage.t + stage.b) / 2, side / 2, k, goal.map_or(1.0, |g| g.frac()), goal.map_or(k, |g| g.tier)));
        self.tray_lines.push(TrayLine::Title { x, y: ty, s: title });
        ty += lh(f.head) + self.sc(6);
        self.tray_lines.push(TrayLine::Text { x, y: ty, s: b.line.to_string(), dim: true, semi: false });
        ty += lh(f.small) + self.sc(10);
        if !kv.is_empty() {
            self.tray_lines.push(TrayLine::Text { x, y: ty, s: kv, dim: false, semi: false });
            ty += lh(f.small) + self.sc(12);
        }
        let now = super::observer::unix_ms();
        self.wear_lines(s, id, now, mdc, f, x, ty);
        self.tray = self.floor;
        self.coin.show(id, k.clamp(1, 7));
        self.coin_n = side as u32;
        self.floor.b + self.sc(12)
    }

    /// Draw. `hover` = unit index under the mouse. The host draws the "Polish" button (`polish_btn`).
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&self, cv: &mut Canvas, f: &Fonts, col: &Colors, dy: i32, vt: i32, vb: i32, hover: Option<usize>) {
        let sh = |r: Rect| Rect { l: r.l, t: r.t + dy, r: r.r, b: r.b + dy };
        let vis = |r: Rect| r.b + dy >= vt && r.t + dy <= vb;
        let rad = self.sc(6) as f32;
        if self.big {
            // the large view: the room's floor, one medal under a cold light
            let fl = sh(self.floor);
            cv.fill_round(fl, rad, hx(0x0d0f12));
            cv.fill_rect(Rect { l: fl.l + self.sc(6), t: fl.t, r: fl.r - self.sc(6), b: fl.t + self.sc(2) }, COLD);
            self.draw_medal(cv, f, dy, vt, vb, 255);
            self.draw_lines(cv, f, dy, fl.r);
            return;
        }

        // Header card.
        if vis(self.head) {
            let h = sh(self.head);
            cv.fill_round(h, rad, col.card);
            cv.stroke_round(h, rad, col.card_border);
            let (lv, into, need, title) = self.level;
            let rr = self.sc(26) as f32;
            let (cx, cy) = (h.l as f32 + self.sc(20) as f32 + rr, (h.t + h.b) as f32 / 2.0);
            cv.ring(cx, cy, rr, self.sc(5) as f32, 1.0, col.line);
            cv.ring(cx, cy, rr, self.sc(5) as f32, into as f32 / need.max(1) as f32, tier_col(1));
            let n = lv.to_string();
            let (nw, nh) = cv.measure(f.semi, &n);
            cv.text(f.semi, &n, cx as i32 - nw / 2, cy as i32 - nh / 2, col.fg);
            let x = (cx + rr) as i32 + self.sc(16);
            let th = cv.measure(f.semi, "Ag").1;
            let sh2 = cv.measure(f.small, "Ag").1;
            let ty = (h.t + h.b) / 2 - (th + sh2) / 2;
            cv.text(f.semi, &format!("Level {lv} \u{00B7} {title}"), x, ty, col.fg);
            cv.text(f.small, &format!("{into} / {need} XP to level {}", lv + 1), x, ty + th, col.dim);
            let rw = cv.measure(f.small, &self.head_right).0;
            if h.r - self.sc(20) - rw > x + self.sc(220) {
                cv.text_right(f.small, &self.head_right, h.r - self.sc(20), (h.t + h.b) / 2 - sh2 / 2, col.dim);
            }
        }

        // The room.
        if vis(self.floor) {
            let fl = sh(self.floor);
            cv.fill_round(fl, rad, FLOOR);
            // Raised-floor tiles under the racks, cold aisle light from above.
            let tile = self.sc(30);
            let mut ty = fl.t + tile;
            while ty < fl.b {
                cv.fill_rect(Rect { l: fl.l + self.sc(2), t: ty, r: fl.r - self.sc(2), b: ty + 1 }, FLOOR_LINE);
                ty += tile;
            }
            cv.fill_round(Rect { l: fl.l + fl.w() / 6, t: fl.t, r: fl.r - fl.w() / 6, b: fl.t + self.sc(3) }, 2.0, COLD.with_a(90));
        }
        for rk in &self.racks {
            if !vis(rk.r) {
                continue;
            }
            let r = sh(rk.r);
            cv.fill_round(r, self.sc(3) as f32, STEEL);
            cv.stroke_round(r, self.sc(3) as f32, STEEL_EDGE);
            // rails
            for x in [r.l + self.sc(4), r.r - self.sc(6)] {
                cv.fill_rect(Rect { l: x, t: r.t + self.sc(32), r: x + self.sc(2), b: r.b - self.sc(8) }, VENT);
            }
            // label-maker tape + certificate stamp
            let tw = cv.measure(f.small, rk.tape).0 + self.sc(12);
            let tape = Rect { l: r.l + self.sc(10), t: r.t + self.sc(9), r: (r.l + self.sc(10) + tw).min(r.r - self.sc(60)), b: r.t + self.sc(27) };
            cv.fill_round(tape, 2.0, TAPE);
            let tape_s = cv.ellipsize(f.small, rk.tape, tape.w() - self.sc(12));
            let tsh = cv.measure(f.small, "Ag").1;
            cv.text(f.small, &tape_s, tape.l + self.sc(6), (tape.t + tape.b) / 2 - tsh / 2, TAPE_INK);
            let sw = cv.measure(f.tiny, &rk.stamp).0 + self.sc(10);
            let st = Rect { l: r.r - self.sc(10) - sw, t: r.t + self.sc(9), r: r.r - self.sc(10), b: r.t + self.sc(27) };
            cv.stroke_round(st, 2.0, rk.stamp_col);
            let tyh = cv.measure(f.tiny, "Ag").1;
            cv.text(f.tiny, &rk.stamp, st.l + self.sc(5), (st.t + st.b) / 2 - tyh / 2, rk.stamp_col);
            // vents
            for k in 0..2 {
                let vy = r.b - self.sc(10) - (k + 1) * self.sc(9);
                let mut x = r.l + self.sc(12);
                while x < r.r - self.sc(12) {
                    cv.fill_rect(Rect { l: x, t: vy, r: x + self.sc(2), b: vy + self.sc(6) }, VENT);
                    x += self.sc(5);
                }
            }
            if let Some(hint) = &rk.hint {
                let hy = r.b - self.sc(10) - 2 * self.sc(9) - self.sc(4) - tyh;
                let s = cv.ellipsize(f.tiny, hint, r.w() - self.sc(24));
                cv.text(f.tiny, &s, r.l + self.sc(12), hy, AMBER);
            }
        }
        for (i, u) in self.units.iter().enumerate() {
            if !vis(u.r) {
                continue;
            }
            let selected = i == self.sel;
            let p = if selected { self.pull } else { 0.0 };
            let out = (ease(p) * self.sc(5) as f32) as i32;
            let r = Rect { l: u.r.l - out, t: u.r.t + dy - out / 2, r: u.r.r + out, b: u.r.b + dy + out / 2 };
            if u.unit == Unit::Blank {
                cv.fill_round(r, 2.0, hx(0x202329));
                let mut x = r.l + self.sc(14);
                while x < r.r - self.sc(14) {
                    cv.fill_rect(Rect { l: x, t: r.t + self.sc(14), r: x + self.sc(2), b: r.b - self.sc(14) }, VENT);
                    x += self.sc(6);
                }
                continue;
            }
            if selected {
                cv.fill_round(Rect { l: r.l - 2, t: r.t - 2, r: r.r + 2, b: r.b + 4 }, 4.0, Rgba(0, 0, 0, 120));
            }
            cv.fill_round(r, 2.0, if hover == Some(i) || selected { UNIT_HOT } else { UNIT });
            if selected {
                cv.stroke_round(r, 2.0, COLD);
            }
            // tier LEDs, a column at the left
            let (lit, of, cols) = u.leds;
            let lx = r.l + self.sc(8);
            let led_h = self.sc(3).max(2);
            let step = led_h + self.sc(2);
            let top = (r.t + r.b) / 2 - (of as i32 * step) / 2;
            for k in 0..of {
                let lr = Rect { l: lx, t: top + k as i32 * step, r: lx + self.sc(7), b: top + k as i32 * step + led_h };
                // the top LED is the highest tier
                let idx = of - 1 - k;
                let on = idx < lit;
                cv.fill_rect(lr, if on { cols[idx as usize] } else { LED_OFF });
            }
            // activity LED, right
            let al = Rect { l: r.r - self.sc(14), t: (r.t + r.b) / 2 - self.sc(3), r: r.r - self.sc(8), b: (r.t + r.b) / 2 + self.sc(3) };
            cv.fill_round(al, self.sc(3) as f32, if u.live { LINK } else { hx(0x4a4f58) });
            // text
            let tx = lx + self.sc(16);
            let maxw = al.l - tx - self.sc(6);
            let th = cv.measure(f.body, "Ag").1;
            let sh2 = cv.measure(f.tiny, "Ag").1;
            let bar_h = if u.bar.is_some() { self.sc(3) + self.sc(4) } else { 0 };
            let ty = (r.t + r.b) / 2 - (th + sh2 + bar_h) / 2;
            let t = cv.ellipsize(f.body, &u.title, maxw);
            cv.text(f.body, &t, tx, ty, INK);
            let s = cv.ellipsize(f.tiny, &u.sub, maxw);
            cv.text(f.tiny, &s, tx, ty + th, INK_DIM);
            if let Some((frac, bc)) = u.bar {
                let by = ty + th + sh2 + self.sc(4);
                let br = Rect { l: tx, t: by, r: tx + maxw, b: by + self.sc(3) };
                cv.fill_rect(br, hx(0x11141a));
                let fw = ((maxw as f32) * frac).round() as i32;
                if fw > 0 {
                    cv.fill_rect(Rect { r: tx + fw, ..br }, bc);
                }
            }
        }

        // Tray.
        if vis(self.tray) {
            let t = sh(self.tray);
            let a = (ease(self.pull) * 255.0) as u8;
            cv.fill_round(t, rad, hx(0x14171c));
            cv.fill_rect(Rect { l: t.l + self.sc(6), t: t.t, r: t.r - self.sc(6), b: t.t + self.sc(2) }, COLD.with_a(a));
            self.draw_medal(cv, f, dy, vt, vb, a);
            self.draw_lines(cv, f, dy, t.r);
        }
    }

    /// The medal: the live coin, or the flat disc without a GPU; round it, the way to the next tier (`a`: fade in).
    #[allow(clippy::too_many_arguments)]
    fn draw_medal(&self, cv: &mut Canvas, f: &Fonts, dy: i32, vt: i32, vb: i32, a: u8) {
        let sh = |r: Rect| Rect { l: r.l, t: r.t + dy, r: r.r, b: r.b + dy };
        let live = !self.coin_rect.is_empty() && {
            let cr = sh(self.coin_rect);
            if let Some((_, _, r0, _, frac, next)) = self.disc {
                // the way to the next tier, round the coin
                let (cx, cy) = ((cr.l + cr.r) as f32 / 2.0, (cr.t + cr.b) as f32 / 2.0);
                cv.ring(cx, cy, r0 as f32, self.sc(3) as f32, 1.0, hx(0x23272e));
                cv.ring(cx, cy, r0 as f32, self.sc(3) as f32, frac * (a as f32 / 255.0), tier_col(next.max(1)));
            }
            self.coin.draw(cv, (cr.l + cr.r) / 2, (cr.t + cr.b) / 2, vt, vb)
        };
        if let (false, Some((cx, cy, r0, k, frac, next))) = (live, self.disc) {
            let (cx, cy, r0) = (cx as f32, (cy + dy) as f32, r0 as f32);
            // the medal: a disc in its metal, a dark well, the tier initial; the ring is the way to the next tier
            let metal = if k > 0 { tier_col(k) } else { hx(0x3a3f48) };
            cv.ring(cx, cy, r0, self.sc(4) as f32, 1.0, hx(0x23272e));
            cv.ring(cx, cy, r0, self.sc(4) as f32, frac * (a as f32 / 255.0), tier_col(next.max(1)));
            let d = r0 - self.sc(10) as f32;
            cv.fill_round(Rect { l: (cx - d) as i32, t: (cy - d) as i32, r: (cx + d) as i32, b: (cy + d) as i32 }, d, metal);
            let d2 = d * 0.62;
            cv.fill_round(Rect { l: (cx - d2) as i32, t: (cy - d2) as i32, r: (cx + d2) as i32, b: (cy + d2) as i32 }, d2, Rgba(12, 14, 18, 235));
            let s = if k > 0 { &c::TIER_NAMES[(k - 1) as usize][..1] } else { "?" };
            let (sw, shh) = cv.measure(f.head, s);
            cv.text(f.head, s, cx as i32 - sw / 2, cy as i32 - shh / 2, if k > 0 { metal } else { INK_DIM });
        }
    }

    /// The tray's text lines (and the singles list), clipped at `right`.
    fn draw_lines(&self, cv: &mut Canvas, f: &Fonts, dy: i32, right: i32) {
        for ln in &self.tray_lines {
            match ln {
                TrayLine::Title { x, y, s } => {
                    let s2 = cv.ellipsize(f.head, s, right - self.sc(16) - x);
                    cv.text(f.head, &s2, *x, y + dy, INK);
                }
                TrayLine::Text { x, y, s, dim, semi } => {
                    let font = if *semi { f.semi } else { f.small };
                    let s2 = cv.ellipsize(font, s, right - self.sc(16) - x);
                    cv.text(font, &s2, *x, y + dy, if *dim { INK_DIM } else { INK });
                }
                TrayLine::Single { x, y, w, got, name, xp, line } => {
                    let y = y + dy;
                    let th = cv.measure(f.body, "Ag").1;
                    let d = self.sc(10);
                    let dot = Rect { l: *x, t: y + th / 2 - d / 2, r: x + d, b: y + th / 2 + d / 2 };
                    if *got {
                        cv.fill_round(dot, d as f32 / 2.0, LINK);
                    } else {
                        cv.stroke_round(dot, d as f32 / 2.0, INK_DIM);
                    }
                    let nx = x + d + self.sc(10);
                    let xw = cv.measure(f.small, xp).0;
                    let n = cv.ellipsize(f.body, name, w - d - self.sc(20) - xw);
                    cv.text(f.body, &n, nx, y, if *got { INK } else { INK_DIM });
                    cv.text_right(f.small, xp, x + w, y + self.sc(1), if *got { AMBER } else { INK_DIM });
                    let l2 = cv.ellipsize(f.small, line, w - d - self.sc(10));
                    cv.text(f.small, &l2, nx, y + th, INK_DIM);
                }
            }
        }
    }
}

fn unit_l(s: &State, u: Unit, r: Rect) -> UnitL {
    let off = [LED_OFF; 7];
    match u {
        Unit::Blank => UnitL { unit: u, r, title: String::new(), sub: String::new(), leds: (0, 0, off), bar: None, live: false },
        Unit::Ladder(id) => {
            let b = Catalog::get(id).unwrap();
            let k = s.tier(id).min(7);
            let g = c::goal_of(s, id);
            let mut cols = off;
            for (i, cl) in cols.iter_mut().enumerate() {
                *cl = tier_col(i as u8 + 1);
            }
            let state = if k > 0 { c::TIER_NAMES[(k - 1) as usize] } else { "offline" };
            let sub = match g {
                Some(g) => format!("{} \u{00B7} {} to {}", state, c::fmt_amount(g.left(), g.unit), g.tier_name()),
                None => format!("{} \u{00B7} top tier", state),
            };
            UnitL { unit: u, r, title: b.name.to_string(), sub, leds: (k, 7, cols), bar: g.map(|g| (g.frac(), tier_col(g.tier))), live: k > 0 }
        }
        Unit::Singles(cat) => {
            let all: Vec<_> = Catalog::all().iter().filter(|b| b.category == cat).collect();
            let got = all.iter().filter(|b| s.earned(b.id)).count();
            let tc = tier_col(single_tier(cat));
            let next = all.iter().find(|b| !s.earned(b.id));
            let sub = match next {
                _ if cat == Category::Secret => format!("{} of {} \u{00B7} hidden", got, all.len()),
                Some(b) => format!("{} of {} \u{00B7} next: {}", got, all.len(), b.name),
                None => format!("all {} earned", all.len()),
            };
            UnitL { unit: u, r, title: cat_name(cat).to_string(), sub, leds: (got as u8, all.len().min(7) as u8, [tc; 7]), bar: None, live: got > 0 }
        }
    }
}

// -------------------------------------------------------------------- Overview card and nav chip

/// The Overview "Next goal" card: laid out by the host as a fixed-height clickable card.
pub fn goal_card_h(dpi: u32) -> i32 {
    (84 * dpi as i32 + 48) / 96
}

/// Draw the Overview "Next goal" card into `r` (client coordinates).
#[allow(clippy::too_many_arguments)]
pub fn draw_goal_card(cv: &mut Canvas, f: &Fonts, col: &Colors, dpi: u32, r: Rect, s: &State, g: &Goal, hover: bool) {
    let sc = |v: i32| (v * dpi as i32 + 48) / 96;
    let rad = sc(6) as f32;
    cv.fill_round(r, rad, if hover { col.card.mix(col.fg, 0.04) } else { col.card });
    cv.stroke_round(r, rad, col.card_border);
    let rr = sc(26) as f32;
    let (cx, cy) = (r.l as f32 + sc(18) as f32 + rr, (r.t + r.b) as f32 / 2.0);
    cv.ring(cx, cy, rr, sc(5) as f32, 1.0, col.line);
    cv.ring(cx, cy, rr, sc(5) as f32, g.frac(), tier_col(g.tier));
    let pct = format!("{}%", (g.frac() * 100.0).floor() as i32);
    let (pw, ph) = cv.measure(f.small, &pct);
    cv.text(f.small, &pct, cx as i32 - pw / 2, cy as i32 - ph / 2, col.fg);
    let x = (cx + rr) as i32 + sc(16);
    let maxw = r.r - sc(16) - x;
    let lh = |cv: &Canvas, font| cv.measure(font, "Ag").1;
    let (h1, h2, h3) = (lh(cv, f.tiny), lh(cv, f.semi), lh(cv, f.small));
    let mut y = (r.t + r.b) / 2 - (h1 + h2 + h3) / 2;
    cv.text(f.tiny, "NEXT GOAL", x, y, col.dim);
    y += h1;
    let t = cv.ellipsize(f.semi, &format!("{} \u{00B7} {}", g.name, g.tier_name()), maxw);
    cv.text(f.semi, &t, x, y, col.fg);
    y += h2;
    let mut line = format!("{} of {} \u{00B7} {} to go \u{00B7} +{} XP", c::fmt_amount(g.have, g.unit), c::fmt_amount(g.to as f64, g.unit), c::fmt_amount(g.left(), g.unit), g.xp);
    if let Some(set) = Catalog::get(g.id).and_then(|b| b.set).and_then(Catalog::set) {
        let st = c::rack_status(s, set);
        if st.blockers == [g.id] {
            line.push_str(&format!(" \u{00B7} completes the {} cert", c::TIER_NAMES[(st.next.unwrap_or(1) - 1) as usize]));
        }
    }
    let l2 = cv.ellipsize(f.small, &line, maxw);
    cv.text(f.small, &l2, x, y, col.dim);
}

/// Height of the nav pane's level chip (expanded pane / compact rail).
pub fn chip_h(dpi: u32, labels: bool) -> i32 {
    let v = if labels { 52 } else { 44 };
    (v * dpi as i32 + 48) / 96
}

/// Draw the level chip in `r`: a ring with the level, and (with labels) the title, XP and next goal.
#[allow(clippy::too_many_arguments)]
pub fn draw_chip(cv: &mut Canvas, f: &Fonts, col: &Colors, dpi: u32, r: Rect, s: &State, labels: bool, hover: bool) {
    let sc = |v: i32| (v * dpi as i32 + 48) / 96;
    let (lv, into, need) = c::level_progress(c::total_xp(s));
    if labels {
        cv.fill_round(r, sc(6) as f32, if hover { col.card.mix(col.fg, 0.06) } else { col.card });
        cv.stroke_round(r, sc(6) as f32, col.card_border);
    } else if hover {
        cv.fill_round(r, sc(4) as f32, col.card.mix(col.fg, 0.06));
    }
    let rr = sc(14) as f32;
    let cx = if labels { r.l as f32 + sc(10) as f32 + rr } else { (r.l + r.r) as f32 / 2.0 };
    let cy = (r.t + r.b) as f32 / 2.0;
    cv.ring(cx, cy, rr, sc(3) as f32, 1.0, col.line);
    cv.ring(cx, cy, rr, sc(3) as f32, into as f32 / need.max(1) as f32, tier_col(1));
    let n = lv.to_string();
    let (nw, nh) = cv.measure(f.small, &n);
    cv.text(f.small, &n, cx as i32 - nw / 2, cy as i32 - nh / 2, col.fg);
    if !labels {
        return;
    }
    let x = (cx + rr) as i32 + sc(10);
    let maxw = r.r - sc(8) - x;
    let (h1, h2) = (cv.measure(f.small, "Ag").1, cv.measure(f.tiny, "Ag").1);
    let y = (r.t + r.b) / 2 - (h1 + h2) / 2;
    let t = cv.ellipsize(f.small, &format!("Level {lv} \u{00B7} {}", c::title_of(lv)), maxw);
    cv.text(f.small, &t, x, y, col.fg);
    let mut sub = format!("{into} / {need} XP");
    if let Some(g) = c::next_goal(s) {
        sub.push_str(&format!(" \u{00B7} next: {}", g.name));
    }
    let s2 = cv.ellipsize(f.tiny, &sub, maxw);
    cv.text(f.tiny, &s2, x, y + h1, col.dim);
}
