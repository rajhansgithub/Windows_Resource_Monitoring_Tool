//! The three looks: Lime Terminal, Tactile Hardware and Warm Bento.
//!
//! A look is more than a colour theme: it sets the colours (each look has a
//! light and a dark pair), the typefaces, the corner radii, how bars and big
//! numbers are drawn and which page transition the main window plays. Every
//! surface (taskbar widget, details popup, main window, Achievements) asks
//! this module instead of hard-coding colours, so switching looks is live:
//! set the look, then repaint.
//!
//! Only the real 3D coin (`achievements::gpu`) is the same in every look.
//! Pure data plus a few font lookups; no per-frame cost.

use crate::render::Rgba;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// 01: charcoal, lime accent, monospace, dot-matrix bars, LED numerals.
    Lime,
    /// 06: cream / charcoal hardware panels, burnt orange, LCD readouts, VU meters.
    Tactile,
    /// 03: greige base, white and black rounded tiles, yellow accent.
    Bento,
}

impl Look {
    pub const ALL: [Look; 3] = [Look::Lime, Look::Tactile, Look::Bento];

    pub fn key(self) -> &'static str {
        match self {
            Look::Lime => "lime",
            Look::Tactile => "tactile",
            Look::Bento => "bento",
        }
    }

    pub fn parse(s: &str) -> Option<Look> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lime" | "lime_terminal" | "terminal" => Some(Look::Lime),
            "tactile" | "tactile_hardware" | "hardware" => Some(Look::Tactile),
            "bento" | "warm_bento" => Some(Look::Bento),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Look::Lime => "Lime Terminal",
            Look::Tactile => "Tactile Hardware",
            Look::Bento => "Warm Bento",
        }
    }

    /// One line under the name on the look cards.
    pub fn blurb(self) -> &'static str {
        match self {
            Look::Lime => "Mono type, dot-matrix bars, LED numbers",
            Look::Tactile => "LCD readouts, VU needles, keycaps",
            Look::Bento => "Rounded tiles, big numbers, warm yellow",
        }
    }

    pub fn idx(self) -> usize {
        self as usize
    }

    pub fn from_idx(i: usize) -> Look {
        Look::ALL[i.min(2)]
    }

    /// The Bluetooth page layout this look opens with (0 List, 1 Rack, 2 Hero).
    /// Used once the Bluetooth page (PR #47) is in.
    #[allow(dead_code)]
    pub fn bt_layout(self) -> u8 {
        match self {
            Look::Lime => 0,
            Look::Tactile => 1,
            Look::Bento => 2,
        }
    }

    /// The page transition of the main window and its length.
    pub fn transition(self) -> (Transition, f32) {
        match self {
            Look::Lime => (Transition::Boot, 0.550),
            Look::Tactile => (Transition::Flap, 0.813),
            Look::Bento => (Transition::Splash, 0.406),
        }
    }
}

/// Main-window page change animations (see `transition`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    /// Lime: panel frames draw as outlines, panels power on in reading order,
    /// numbers lamp-test, a segmented loading bar runs along the top.
    Boot,
    /// Tactile: split-flap rows flip to the new page, top to bottom.
    Flap,
    /// Bento: a yellow panel with a bowed edge sweeps across and lifts off.
    Splash,
}

static LOOK: AtomicU8 = AtomicU8::new(0);

pub fn current() -> Look {
    Look::from_idx(LOOK.load(Relaxed) as usize)
}

pub fn set(l: Look) {
    LOOK.store(l as u8, Relaxed);
}

static DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The app's light/dark mode, for code that has no palette handed to it.
pub fn set_dark(d: bool) {
    DARK.store(d, Relaxed);
}

/// Tokens of the current look in the app's current mode.
pub fn live() -> Tokens {
    tokens(DARK.load(Relaxed))
}

const fn hx(v: u32) -> Rgba {
    Rgba((v >> 16) as u8, (v >> 8) as u8, v as u8, 255)
}

const fn hxa(v: u32, a: u8) -> Rgba {
    Rgba((v >> 16) as u8, (v >> 8) as u8, v as u8, a)
}

/// How the big numbers are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Numerals {
    /// Lit LED digits with dim "8" ghosts behind them.
    Led,
    /// LCD segment glass: ink on a tinted well.
    Lcd,
    /// Large light sans with smaller units.
    Display,
}

/// How bars and meters are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bars {
    /// Rows of square dots, lit up to the value.
    Dots,
    /// Segment LED ladder (VU style) in a recessed well.
    Ladder,
    /// Thick rounded pill on a soft track.
    Pill,
}

/// Every colour and shape decision of a look, for one light/dark pair.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct Tokens {
    pub look: Look,
    pub dark: bool,
    /// Window / widget base.
    pub bg: Rgba,
    /// Panels and cards.
    pub panel: Rgba,
    /// Raised fills: hovered rows, chips, controls.
    pub raised: Rgba,
    /// Pressed fills and the strongest neutral fill.
    pub raised2: Rgba,
    /// Recessed wells (LCD glass, meter tracks).
    pub well: Rgba,
    pub border: Rgba,
    pub border2: Rgba,
    pub fg: Rgba,
    pub dim: Rgba,
    pub off: Rgba,
    /// The one accent of the look (readable on `panel`).
    pub accent: Rgba,
    /// Accent used as a large fill (Lime light uses the bright lime here).
    pub accent_fill: Rgba,
    /// Text on `accent_fill`.
    pub on_accent: Rgba,
    pub good: Rgba,
    pub warn: Rgba,
    pub crit: Rgba,
    /// Unlit dots / segments / empty track.
    pub track: Rgba,
    /// LCD readout ink and the unlit-segment ghost (Tactile; others reuse fg).
    pub lcd: Rgba,
    pub lcd_ink: Rgba,
    pub lcd_ghost: Rgba,
    /// Bento's inverted tile (black in light, cream in dark) and its text.
    pub alt: Rgba,
    pub alt_fg: Rgba,
    pub alt_dim: Rgba,
    /// Corner radii in epx: panels/cards, standard controls, the widget pill.
    pub r_card: i32,
    pub r_ctl: i32,
    pub r_widget: i32,
    pub numerals: Numerals,
    pub bars: Bars,
    /// Labels in capitals with wide tracking (mono labels).
    pub caps: bool,
}

pub fn tokens(dark: bool) -> Tokens {
    tokens_for(current(), dark)
}

pub fn tokens_for(look: Look, dark: bool) -> Tokens {
    match (look, dark) {
        (Look::Lime, true) => Tokens {
            look,
            dark,
            bg: hx(0x101112),
            panel: hx(0x17191B),
            raised: hx(0x1D1F21),
            raised2: hx(0x26292C),
            well: hx(0x0C0D0E),
            border: hx(0x2A2D30),
            border2: hx(0x3B4044),
            fg: hx(0xE8EAE3),
            dim: hx(0x7D8279),
            off: hx(0x575C54),
            accent: hx(0xC6F24E),
            accent_fill: hx(0xC6F24E),
            on_accent: hx(0x101112),
            good: hx(0xC6F24E),
            warn: hx(0xFFB340),
            crit: hx(0xFF5A4A),
            track: hxa(0xE8EAE3, 0x1A),
            lcd: hx(0x0C0D0E),
            lcd_ink: hx(0xC6F24E),
            lcd_ghost: hxa(0xC6F24E, 0x14),
            alt: hx(0xC6F24E),
            alt_fg: hx(0x101112),
            alt_dim: hx(0x3B4A12),
            r_card: 4,
            r_ctl: 2,
            r_widget: 3,
            numerals: Numerals::Led,
            bars: Bars::Dots,
            caps: true,
        },
        (Look::Lime, false) => Tokens {
            look,
            dark,
            bg: hx(0xECEEE6),
            panel: hx(0xF7F8F3),
            raised: hx(0xEEF0E7),
            raised2: hx(0xE2E5D8),
            well: hx(0xE2E5D8),
            border: hx(0xD2D6C6),
            border2: hx(0xB9BEAA),
            fg: hx(0x16180F),
            dim: hx(0x636858),
            off: hx(0x979D8A),
            accent: hx(0x5B7D00),
            accent_fill: hx(0xC6F24E),
            on_accent: hx(0x101112),
            good: hx(0x5B7D00),
            warn: hx(0xA45F00),
            crit: hx(0xC42F1C),
            track: hxa(0x16180F, 0x21),
            lcd: hx(0x101112),
            lcd_ink: hx(0xC6F24E),
            lcd_ghost: hxa(0xC6F24E, 0x14),
            alt: hx(0x101112),
            alt_fg: hx(0xC6F24E),
            alt_dim: hx(0x7D8279),
            r_card: 4,
            r_ctl: 2,
            r_widget: 3,
            numerals: Numerals::Led,
            bars: Bars::Dots,
            caps: true,
        },
        (Look::Tactile, true) => Tokens {
            look,
            dark,
            bg: hx(0x1D1D1F),
            panel: hx(0x262628),
            raised: hx(0x2E2E31),
            raised2: hx(0x36363A),
            well: hx(0x141415),
            border: hx(0x36363A),
            border2: hx(0x0C0C0D),
            fg: hx(0xEBE7DF),
            dim: hx(0x8D887F),
            off: hx(0x5C5953),
            accent: hx(0xE8622C),
            accent_fill: hx(0xE8622C),
            on_accent: hx(0xFFF6EE),
            good: hx(0x7FAE62),
            warn: hx(0xE0A43C),
            crit: hx(0xE5483A),
            track: hx(0x343438),
            lcd: hx(0x0D0C0A),
            lcd_ink: hx(0xF0A064),
            lcd_ghost: hxa(0xF0A064, 0x13),
            alt: hx(0x141415),
            alt_fg: hx(0xEBE7DF),
            alt_dim: hx(0x8D887F),
            r_card: 10,
            r_ctl: 6,
            r_widget: 6,
            numerals: Numerals::Lcd,
            bars: Bars::Ladder,
            caps: true,
        },
        (Look::Tactile, false) => Tokens {
            look,
            dark,
            bg: hx(0xECE6DA),
            panel: hx(0xF5F0E6),
            raised: hx(0xE9E2D4),
            raised2: hx(0xDDD5C6),
            well: hx(0xDDD5C6),
            border: hx(0xD3CAB8),
            border2: hx(0xC9BFAB),
            fg: hx(0x1F1D1A),
            dim: hx(0x7A7367),
            off: hx(0xA69E90),
            accent: hx(0xD4561F),
            accent_fill: hx(0xE8622C),
            on_accent: hx(0xFFF6EE),
            good: hx(0x5F8A4A),
            warn: hx(0xB07A1C),
            crit: hx(0xC8372D),
            track: hx(0xCFC5B0),
            lcd: hx(0xC9CDB8),
            lcd_ink: hx(0x2A2D22),
            lcd_ghost: hxa(0x2A2D22, 0x17),
            alt: hx(0x1F1D1A),
            alt_fg: hx(0xF5F0E6),
            alt_dim: hx(0x8D887F),
            r_card: 10,
            r_ctl: 6,
            r_widget: 6,
            numerals: Numerals::Lcd,
            bars: Bars::Ladder,
            caps: true,
        },
        (Look::Bento, true) => Tokens {
            look,
            dark,
            bg: hx(0x161513),
            panel: hx(0x22201D),
            raised: hx(0x2B2825),
            raised2: hx(0x34302C),
            well: hx(0x34302C),
            border: hxa(0xFFFFFF, 0x14),
            border2: hxa(0xFFFFFF, 0x24),
            fg: hx(0xF1ECE4),
            dim: hx(0x948E85),
            off: hx(0x625D56),
            accent: hx(0xF5C842),
            accent_fill: hx(0xF5C842),
            on_accent: hx(0x1A1816),
            good: hx(0x5DB27A),
            warn: hx(0xEE8F3A),
            crit: hx(0xE2523F),
            track: hx(0x34302C),
            lcd: hx(0x22201D),
            lcd_ink: hx(0xF1ECE4),
            lcd_ghost: hxa(0xF1ECE4, 0x10),
            alt: hx(0xEFE9DF),
            alt_fg: hx(0x1A1816),
            alt_dim: hx(0x6F695F),
            r_card: 22,
            r_ctl: 999,
            r_widget: 12,
            numerals: Numerals::Display,
            bars: Bars::Pill,
            caps: false,
        },
        (Look::Bento, false) => Tokens {
            look,
            dark,
            bg: hx(0xE6E1DA),
            panel: hx(0xFFFFFF),
            raised: hx(0xF3F0EA),
            raised2: hx(0xECE7DF),
            well: hx(0xECE7DF),
            border: hxa(0x1A1816, 0x17),
            border2: hxa(0x1A1816, 0x2A),
            fg: hx(0x1A1816),
            dim: hx(0x85807A),
            off: hx(0xB3ADA4),
            accent: hx(0xB98A00),
            accent_fill: hx(0xF5C842),
            on_accent: hx(0x1A1816),
            good: hx(0x4F9D68),
            warn: hx(0xD9782A),
            crit: hx(0xE2523F),
            track: hx(0xECE7DF),
            lcd: hx(0xFFFFFF),
            lcd_ink: hx(0x1A1816),
            lcd_ghost: hxa(0x1A1816, 0x0C),
            alt: hx(0x111111),
            alt_fg: hx(0xFFFFFF),
            alt_dim: hx(0xA39D94),
            r_card: 22,
            r_ctl: 999,
            r_widget: 12,
            numerals: Numerals::Display,
            bars: Bars::Pill,
            caps: false,
        },
    }
}

// ------------------------------------------------------------------ type

/// Typefaces of a look. Only fonts that ship with Windows 10/11 are used,
/// each with a fallback that is always present.
#[derive(Clone, Copy, Debug)]
pub struct Faces {
    /// Body text and controls.
    pub ui: &'static str,
    /// Page titles and headlines.
    pub display: &'static str,
    /// Labels, ids, units.
    pub mono: &'static str,
    /// Big numbers.
    pub num: &'static str,
    /// Weight of titles (Bento is bold, the others regular-ish).
    pub title_weight: i32,
    /// Weight of the big numbers.
    pub num_weight: i32,
}

pub fn faces() -> Faces {
    faces_for(current())
}

pub fn faces_for(look: Look) -> Faces {
    use std::sync::OnceLock;
    static MONO: OnceLock<&'static str> = OnceLock::new();
    static DIN: OnceLock<&'static str> = OnceLock::new();
    static DISPLAY: OnceLock<&'static str> = OnceLock::new();
    let mono = *MONO.get_or_init(|| has_face("Cascadia Mono").unwrap_or("Consolas"));
    let din = *DIN.get_or_init(|| has_face("Bahnschrift").unwrap_or_else(crate::render::ui_face));
    let display = *DISPLAY.get_or_init(|| has_face("Segoe UI Variable Display").unwrap_or_else(crate::render::ui_face));
    match look {
        Look::Lime => Faces { ui: mono, display: mono, mono, num: mono, title_weight: 600, num_weight: 300 },
        Look::Tactile => Faces { ui: din, display: din, mono, num: mono, title_weight: 600, num_weight: 400 },
        Look::Bento => {
            Faces { ui: crate::render::ui_face(), display, mono, num: display, title_weight: 700, num_weight: 600 }
        }
    }
}

/// `Some(name)` when the system has that font.
fn has_face(want: &'static str) -> Option<&'static str> {
    #[cfg(windows)]
    unsafe {
        use crate::render::create_font;
        use windows_sys::Win32::Graphics::Gdi::*;
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let f = create_font(12, 400, want, true);
        let old = SelectObject(dc, f);
        let mut buf = [0u16; 64];
        let n = GetTextFaceW(dc, buf.len() as i32, buf.as_mut_ptr());
        SelectObject(dc, old);
        DeleteObject(f);
        DeleteDC(dc);
        let got = String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize]);
        got.eq_ignore_ascii_case(want).then_some(want)
    }
    #[cfg(not(windows))]
    {
        let _ = want;
        None
    }
}

/// Card radius (epx) of the current look; the same in light and dark.
pub fn current_card_radius() -> i32 {
    tokens_for(current(), true).r_card
}

/// A label as the look writes it: capitals for Lime and Tactile.
pub fn label(s: &str) -> String {
    if current() == Look::Bento {
        s.to_string()
    } else {
        s.to_uppercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        for l in Look::ALL {
            assert_eq!(Look::parse(l.key()), Some(l));
            assert_eq!(Look::from_idx(l.idx()), l);
        }
        assert_eq!(Look::parse("nope"), None);
    }

    #[test]
    fn text_contrast() {
        // Relative luminance contrast of fg / dim on the panel colour.
        fn lum(c: Rgba) -> f32 {
            let f = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c.0) + 0.7152 * f(c.1) + 0.0722 * f(c.2)
        }
        fn ratio(a: Rgba, b: Rgba) -> f32 {
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        }
        for l in Look::ALL {
            for dark in [true, false] {
                let t = tokens_for(l, dark);
                assert!(ratio(t.fg, t.panel) >= 7.0, "{:?} {} fg", l, dark);
                assert!(ratio(t.dim, t.panel) >= 3.0, "{:?} {} dim", l, dark);
                assert!(ratio(t.accent, t.panel) >= 3.0, "{:?} {} accent", l, dark);
                assert!(ratio(t.on_accent, t.accent_fill) >= 4.5, "{:?} {} on_accent", l, dark);
            }
        }
    }

    #[test]
    fn picked_transitions() {
        assert_eq!(Look::Lime.transition(), (Transition::Boot, 0.550));
        assert_eq!(Look::Tactile.transition(), (Transition::Flap, 0.813));
        assert_eq!(Look::Bento.transition(), (Transition::Splash, 0.406));
        assert_eq!([Look::Lime.bt_layout(), Look::Tactile.bt_layout(), Look::Bento.bt_layout()], [0, 1, 2]);
    }
}
