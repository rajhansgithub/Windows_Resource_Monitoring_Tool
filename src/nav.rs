//! Navigation model of the main window: the pages, their order and grouping
//! in the navigation pane, the pane's display mode, the back stack and window
//! placement clamping. Pure logic (no Win32), unit-tested.
//!
//! Follows the Fluent / WinUI NavigationView guidance: a left pane with
//! section headers and separators, About pinned in the footer, expanded
//! (icons + labels) from 1008 epx wide and compact (icons only) below.

use crate::util::Rect;

/// A destination in the main window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Overview,
    /// Detailed live view of the whole machine (opened from the tray System popup).
    Monitor,
    /// Placeholder page that opens the Achievements window. Kept as its own
    /// page so an in-window renderer can replace it without touching the nav.
    Achievements,
    Incidents,
    Sleep,
    HeldBack,
    Baseline,
    Ghost,
    /// Pick the look (Lime / Tactile / Bento) and light or dark.
    Appearance,
    General,
    Metrics,
    Colours,
    Alerts,
    Data,
    About,
}

/// Rows of the navigation pane, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Item(Page),
    Header(&'static str),
    Separator,
}

/// The scrolling part of the pane (the footer is `FOOTER`).
pub const MENU: [Entry; 17] = [
    Entry::Item(Page::Overview),
    Entry::Item(Page::Monitor),
    Entry::Item(Page::Achievements),
    Entry::Header("INSIGHTS"),
    Entry::Item(Page::Incidents),
    Entry::Item(Page::Sleep),
    Entry::Item(Page::HeldBack),
    Entry::Item(Page::Baseline),
    Entry::Item(Page::Ghost),
    Entry::Separator,
    Entry::Header("SETTINGS"),
    Entry::Item(Page::Appearance),
    Entry::Item(Page::General),
    Entry::Item(Page::Metrics),
    Entry::Item(Page::Colours),
    Entry::Item(Page::Alerts),
    Entry::Item(Page::Data),
];

/// Pinned to the bottom of the pane.
pub const FOOTER: [Page; 1] = [Page::About];

impl Page {
    /// Every page in navigation order (menu, then footer). Ctrl+Tab walks this.
    pub const ALL: [Page; 15] = [
        Page::Overview,
        Page::Monitor,
        Page::Achievements,
        Page::Incidents,
        Page::Sleep,
        Page::HeldBack,
        Page::Baseline,
        Page::Ghost,
        Page::Appearance,
        Page::General,
        Page::Metrics,
        Page::Colours,
        Page::Alerts,
        Page::Data,
        Page::About,
    ];

    pub fn index(self) -> usize {
        Page::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    pub fn title(self) -> &'static str {
        match self {
            Page::Overview => "Overview",
            Page::Monitor => "Monitor",
            Page::Achievements => "Achievements",
            Page::Incidents => "Incident Cards",
            Page::Sleep => "Sleep Verdict",
            Page::HeldBack => "Held Back",
            Page::Baseline => "Before / After",
            Page::Ghost => "Ghost Memory",
            Page::Appearance => "Appearance",
            Page::General => "General",
            Page::Metrics => "Metrics",
            Page::Colours => "Colours",
            Page::Alerts => "Alerts",
            Page::Data => "Data",
            Page::About => "About",
        }
    }

    /// Segoe Fluent Icons / Segoe MDL2 Assets glyph.
    pub fn glyph(self) -> char {
        match self {
            Page::Overview => '\u{E80F}',     // home
            Page::Monitor => '\u{E9D2}',      // area chart
            Page::Achievements => '\u{E734}', // star
            Page::Incidents => '\u{E945}',    // lightning bolt
            Page::Sleep => '\u{E708}',        // moon
            Page::HeldBack => '\u{EC4A}',     // speedometer
            Page::Baseline => '\u{E81C}',     // history
            Page::Ghost => '\u{E950}',        // chip
            Page::Appearance => '\u{E771}',   // personalize
            Page::General => '\u{E713}',      // gear
            Page::Metrics => '\u{E9D9}',      // chart
            Page::Colours => '\u{E790}',      // palette
            Page::Alerts => '\u{EA8F}',       // bell
            Page::Data => '\u{E8F1}',         // library
            Page::About => '\u{E946}',        // info
        }
    }

    /// Stable name for the settings file (`window_page = ...`).
    pub fn key(self) -> &'static str {
        match self {
            Page::Overview => "overview",
            Page::Monitor => "monitor",
            Page::Achievements => "achievements",
            Page::Incidents => "incidents",
            Page::Sleep => "sleep",
            Page::HeldBack => "heldback",
            Page::Baseline => "baseline",
            Page::Ghost => "ghost",
            Page::Appearance => "appearance",
            Page::General => "general",
            Page::Metrics => "metrics",
            Page::Colours => "colours",
            Page::Alerts => "alerts",
            Page::Data => "data",
            Page::About => "about",
        }
    }

    /// Span name for the frame-time probe (`perf.rs`).
    pub fn perf_key(self) -> &'static str {
        match self {
            Page::Overview => "draw_overview",
            Page::Monitor => "draw_monitor",
            Page::Achievements => "draw_achievements",
            Page::Incidents | Page::Sleep | Page::HeldBack | Page::Baseline | Page::Ghost => "draw_insights",
            _ => "draw_settings",
        }
    }

    pub fn from_key(k: &str) -> Option<Page> {
        Page::ALL.iter().copied().find(|p| p.key().eq_ignore_ascii_case(k.trim()))
    }

    /// Position of this page's panel in `Insights::panels()` (Insights pages only).
    pub fn panel(self) -> Option<usize> {
        match self {
            Page::Incidents => Some(0),
            Page::Sleep => Some(1),
            Page::HeldBack => Some(2),
            Page::Baseline => Some(3),
            Page::Ghost => Some(4),
            _ => None,
        }
    }

    /// The Insights page showing panel `i`.
    pub fn of_panel(i: usize) -> Option<Page> {
        [Page::Incidents, Page::Sleep, Page::HeldBack, Page::Baseline, Page::Ghost].get(i).copied()
    }

    /// The Insights page of a panel / notice key ("incidents", "sleep", ...).
    pub fn of_panel_key(k: &str) -> Option<Page> {
        Page::from_key(k).filter(|p| p.panel().is_some())
    }

    /// Shows Insights data (and so wants the 2 s refresh while visible).
    pub fn shows_insights(self) -> bool {
        self == Page::Overview || self.panel().is_some()
    }

    /// A settings page (General .. Data).
    pub fn is_settings(self) -> bool {
        matches!(self, Page::Appearance | Page::General | Page::Metrics | Page::Colours | Page::Alerts | Page::Data)
    }

    /// The next (`dir` > 0) or previous page in navigation order, wrapping.
    pub fn step(self, dir: i32) -> Page {
        let n = Page::ALL.len() as i32;
        Page::ALL[(self.index() as i32 + dir.signum()).rem_euclid(n) as usize]
    }
}

/// The pages a keyboard user walks with Up / Down in the pane (menu, then footer).
pub fn nav_order() -> impl Iterator<Item = Page> {
    MENU.iter().filter_map(|e| match e {
        Entry::Item(p) => Some(*p),
        _ => None,
    })
    .chain(FOOTER)
}

/// How the pane is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Icons and labels, beside the content.
    Expanded,
    /// Icons only; the hamburger opens the full pane as an overlay.
    Compact,
}

/// WinUI's `ExpandedModeThresholdWidth`.
pub const EXPANDED_MIN_EPX: i32 = 1008;

/// Display mode for a window `width_epx` wide (effective pixels = DIP).
pub fn mode_for(width_epx: i32) -> Mode {
    if width_epx >= EXPANDED_MIN_EPX {
        Mode::Expanded
    } else {
        Mode::Compact
    }
}

/// Visited pages for Back (Alt+Left, mouse back button), newest last.
pub struct History {
    stack: Vec<Page>,
}

impl History {
    pub const CAP: usize = 20;

    pub fn new() -> History {
        History { stack: Vec::with_capacity(History::CAP) }
    }

    /// Leaving `from` for another page.
    pub fn push(&mut self, from: Page) {
        if self.stack.last() == Some(&from) {
            return;
        }
        if self.stack.len() == History::CAP {
            self.stack.remove(0);
        }
        self.stack.push(from);
    }

    pub fn pop(&mut self) -> Option<Page> {
        self.stack.pop()
    }

    pub fn can_go_back(&self) -> bool {
        !self.stack.is_empty()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.stack.len()
    }
}

/// Saved main-window placement: the normal (restored) rectangle in screen
/// pixels and whether it was maximized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub max: bool,
}

impl Placement {
    /// "x,y,w,h,max" (as in settings.ini). None if malformed or too small.
    pub fn parse(s: &str) -> Option<Placement> {
        let v: Vec<&str> = s.split(',').map(|x| x.trim()).collect();
        if v.len() < 4 {
            return None;
        }
        let n = |i: usize| v[i].parse::<i32>().ok();
        let (x, y, w, h) = (n(0)?, n(1)?, n(2)?, n(3)?);
        let max = v.get(4).is_some_and(|m| matches!(*m, "1" | "true" | "yes" | "on"));
        (w >= 100 && h >= 100 && w <= 100_000 && h <= 100_000).then_some(Placement { x, y, w, h, max })
    }

    pub fn format(&self) -> String {
        format!("{},{},{},{},{}", self.x, self.y, self.w, self.h, self.max as u8)
    }

    pub fn rect(&self) -> Rect {
        Rect { l: self.x, t: self.y, r: self.x + self.w, b: self.y + self.h }
    }
}

/// Fit a window rectangle into a monitor work area: shrink it to fit, then
/// move it fully inside (a window saved on a monitor that is gone, or larger
/// than the screen now is, comes back whole and visible).
pub fn clamp_rect(r: Rect, work: Rect) -> Rect {
    let w = r.w().min(work.w()).max(1);
    let h = r.h().min(work.h()).max(1);
    let l = r.l.clamp(work.l, work.r - w);
    let t = r.t.clamp(work.t, work.b - h);
    Rect { l, t, r: l + w, b: t + h }
}

/// A rectangle `w`×`h` centred in `work` (never above its top).
pub fn centre_in(w: i32, h: i32, work: Rect) -> Rect {
    let (w, h) = (w.min(work.w()), h.min(work.h()));
    let l = work.l + (work.w() - w) / 2;
    let t = work.t + ((work.h() - h) / 2).max(0);
    Rect { l, t, r: l + w, b: t + h }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_order_and_grouping() {
        let order: Vec<Page> = nav_order().collect();
        assert_eq!(order, Page::ALL.to_vec(), "Up/Down and Ctrl+Tab agree");
        assert_eq!(MENU[0], Entry::Item(Page::Overview));
        assert_eq!(MENU[1], Entry::Item(Page::Monitor));
        assert_eq!(MENU[2], Entry::Item(Page::Achievements));
        assert_eq!(MENU[3], Entry::Header("INSIGHTS"));
        let sep = MENU.iter().position(|e| *e == Entry::Separator).unwrap();
        assert_eq!(MENU[sep + 1], Entry::Header("SETTINGS"));
        assert_eq!(MENU[sep + 2], Entry::Item(Page::Appearance));
        assert_eq!(MENU[sep + 3], Entry::Item(Page::General));
        assert_eq!(FOOTER, [Page::About]);
        // Every page appears exactly once.
        for p in Page::ALL {
            assert_eq!(order.iter().filter(|x| **x == p).count(), 1, "{:?}", p);
        }
    }

    #[test]
    fn keys_round_trip() {
        for p in Page::ALL {
            assert_eq!(Page::from_key(p.key()), Some(p));
            assert_eq!(Page::ALL[p.index()], p);
        }
        assert_eq!(Page::from_key(" Overview "), Some(Page::Overview));
        assert_eq!(Page::from_key("insights"), None);
        assert_eq!(Page::of_panel_key("sleep"), Some(Page::Sleep));
        assert_eq!(Page::of_panel_key("general"), None);
    }

    #[test]
    fn panels_match_pages() {
        let keys = ["incidents", "sleep", "heldback", "baseline", "ghost"];
        for (i, k) in keys.iter().enumerate() {
            let p = Page::of_panel(i).unwrap();
            assert_eq!(p.panel(), Some(i));
            assert_eq!(p.key(), *k, "page key = panel key");
            assert!(p.shows_insights());
        }
        assert_eq!(Page::of_panel(5), None);
        assert!(Page::Overview.shows_insights());
        assert!(!Page::General.shows_insights());
        assert!(!Page::Achievements.shows_insights());
        assert!(Page::Alerts.is_settings() && !Page::About.is_settings() && !Page::Overview.is_settings());
    }

    #[test]
    fn next_and_previous_wrap() {
        assert_eq!(Page::Overview.step(1), Page::Monitor);
        assert_eq!(Page::Monitor.step(1), Page::Achievements);
        assert_eq!(Page::About.step(1), Page::Overview);
        assert_eq!(Page::Overview.step(-1), Page::About);
        assert_eq!(Page::Ghost.step(1), Page::Appearance);
        assert_eq!(Page::Appearance.step(1), Page::General);
        assert_eq!(Page::General.step(-1), Page::Appearance);
    }

    #[test]
    fn compact_below_1008() {
        assert_eq!(mode_for(1008), Mode::Expanded);
        assert_eq!(mode_for(1500), Mode::Expanded);
        assert_eq!(mode_for(1007), Mode::Compact);
        assert_eq!(mode_for(720), Mode::Compact);
    }

    #[test]
    fn history_is_bounded_and_skips_repeats() {
        let mut h = History::new();
        assert!(!h.can_go_back());
        h.push(Page::Overview);
        h.push(Page::Overview);
        assert_eq!(h.len(), 1);
        for i in 0..30 {
            h.push(Page::ALL[i % Page::ALL.len()]);
        }
        assert_eq!(h.len(), History::CAP);
        assert_eq!(h.pop(), Some(Page::ALL[29 % Page::ALL.len()]));
        while h.pop().is_some() {}
        assert!(!h.can_go_back());
    }

    #[test]
    fn placement_parse() {
        let p = Placement::parse("10, -20, 1200, 800, 1").unwrap();
        assert_eq!(p, Placement { x: 10, y: -20, w: 1200, h: 800, max: true });
        assert_eq!(Placement::parse(&p.format()), Some(p));
        assert_eq!(Placement::parse("1,2,300,400").map(|p| p.max), Some(false));
        assert_eq!(Placement::parse("1,2,3"), None);
        assert_eq!(Placement::parse("a,b,c,d"), None);
        assert_eq!(Placement::parse("0,0,10,10,0"), None, "too small");
        assert_eq!(Placement::parse(""), None);
    }

    #[test]
    fn clamp_keeps_window_visible() {
        let work = Rect { l: 0, t: 0, r: 1920, b: 1040 };
        let inside = Rect { l: 100, t: 100, r: 1100, b: 800 };
        assert_eq!(clamp_rect(inside, work), inside);
        // Off the right / bottom: moved back in, size kept.
        let r = clamp_rect(Rect { l: 1500, t: 900, r: 2500, b: 1600 }, work);
        assert_eq!((r.w(), r.h()), (1000, 700));
        assert!(r.l >= 0 && r.r <= 1920 && r.t >= 0 && r.b <= 1040);
        // On a monitor that is gone (far left): comes onto this one.
        let r = clamp_rect(Rect { l: -3000, t: -500, r: -2000, b: 200 }, work);
        assert_eq!((r.l, r.t), (0, 0));
        // Larger than the work area: shrunk to it.
        let r = clamp_rect(Rect { l: -10, t: -10, r: 4000, b: 3000 }, work);
        assert_eq!(r, work);
        // Work area not at the origin (taskbar on the left, second monitor).
        let w2 = Rect { l: 1920, t: 48, r: 3840, b: 1080 };
        let r = clamp_rect(Rect { l: 1900, t: 0, r: 2900, b: 700 }, w2);
        assert_eq!((r.l, r.t), (1920, 48));
    }

    #[test]
    fn centred_default() {
        let work = Rect { l: 0, t: 0, r: 1920, b: 1040 };
        let r = centre_in(1000, 700, work);
        assert_eq!((r.l, r.t, r.w(), r.h()), (460, 170, 1000, 700));
        let r = centre_in(3000, 2000, work);
        assert_eq!(r, work);
    }
}
