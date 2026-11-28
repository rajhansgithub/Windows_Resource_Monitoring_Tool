//! Fonts that ship inside the exe (SIL Open Font License, see res/fonts):
//! JetBrains Mono (labels, ids, Lime Terminal), Barlow Semi Condensed
//! (Tactile Hardware) and Inter (Warm Bento). All are hinted TrueType, so
//! small labels stay sharp in GDI. They are loaded privately for this process
//! with AddFontMemResourceEx: nothing is installed on the system.
//!
//! GDI only groups Regular and Bold under one family name; Light, Medium and
//! SemiBold are families of their own ("Inter SemiBold"). `pick` maps the
//! family + weight the looks ask for to the bundled face that is closest, so
//! GDI never fakes a weight by smearing the regular outlines.

use std::sync::OnceLock;

pub const MONO: &str = "JetBrains Mono";
pub const DIN: &str = "Barlow Semi Condensed";
pub const SANS: &str = "Inter";

/// (family, weight, GDI face name, file bytes).
const FACES: &[(&str, i32, &str, &[u8])] = &[
    (MONO, 300, "JetBrains Mono Light", include_bytes!("../res/fonts/JetBrainsMono-Light.ttf")),
    (MONO, 400, "JetBrains Mono", include_bytes!("../res/fonts/JetBrainsMono-Regular.ttf")),
    (MONO, 600, "JetBrains Mono SemiBold", include_bytes!("../res/fonts/JetBrainsMono-SemiBold.ttf")),
    (DIN, 400, "Barlow Semi Condensed", include_bytes!("../res/fonts/BarlowSemiCondensed-Regular.ttf")),
    (DIN, 500, "Barlow Semi Condensed Medium", include_bytes!("../res/fonts/BarlowSemiCondensed-Medium.ttf")),
    (DIN, 600, "Barlow Semi Condensed SemiBold", include_bytes!("../res/fonts/BarlowSemiCondensed-SemiBold.ttf")),
    (SANS, 300, "Inter Light", include_bytes!("../res/fonts/Inter-Light.ttf")),
    (SANS, 400, "Inter", include_bytes!("../res/fonts/Inter-Regular.ttf")),
    (SANS, 600, "Inter SemiBold", include_bytes!("../res/fonts/Inter-SemiBold.ttf")),
    (SANS, 700, "Inter", include_bytes!("../res/fonts/Inter-Bold.ttf")),
];

/// Load the bundled fonts once. True when all of them loaded.
pub fn ready() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Graphics::Gdi::AddFontMemResourceEx;
            FACES.iter().all(|&(_, _, _, data)| {
                let mut n = 0u32;
                let h = unsafe {
                    AddFontMemResourceEx(data.as_ptr() as *const _, data.len() as u32, std::ptr::null(), &mut n)
                };
                !h.is_null() && n > 0
            })
        }
        #[cfg(not(windows))]
        false
    })
}

/// The GDI face name and weight to ask for when a look wants `family` at
/// `weight`. Other families pass through unchanged. A tie goes to the
/// heavier face.
pub fn pick<'a>(family: &'a str, weight: i32) -> (&'a str, i32) {
    let mut best: Option<(i32, &'static str, i32)> = None;
    for &(fam, w, face, _) in FACES {
        if !fam.eq_ignore_ascii_case(family) {
            continue;
        }
        let d = (w - weight).abs() * 2 - (w > weight) as i32;
        if best.is_none_or(|(bd, _, _)| d < bd) {
            best = Some((d, face, w));
        }
    }
    match best {
        Some((_, face, w)) if ready() => (face, w),
        _ => (family, weight),
    }
}

/// True for the face names `pick` hands out.
pub fn is_bundled(face: &str) -> bool {
    FACES.iter().any(|&(_, _, f, _)| f.eq_ignore_ascii_case(face))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_nearest_real_weight() {
        assert!(ready(), "bundled fonts did not load");
        assert_eq!(pick(MONO, 600), ("JetBrains Mono SemiBold", 600));
        assert_eq!(pick(MONO, 300), ("JetBrains Mono Light", 300));
        assert_eq!(pick(MONO, 500), ("JetBrains Mono SemiBold", 600));
        assert_eq!(pick(MONO, 700), ("JetBrains Mono SemiBold", 600));
        assert_eq!(pick(SANS, 700), ("Inter", 700));
        assert_eq!(pick(SANS, 650), ("Inter", 700));
        assert_eq!(pick(DIN, 450), ("Barlow Semi Condensed Medium", 500));
        assert_eq!(pick("Segoe UI", 600), ("Segoe UI", 600));
    }

    /// GDI must find every bundled face by name, at its own weight.
    #[test]
    fn gdi_finds_every_bundled_face() {
        use windows_sys::Win32::Graphics::Gdi::*;
        assert!(ready());
        for &(_, w, face, _) in FACES {
            let f = crate::render::create_font(14, w, face, true);
            let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
            let old = unsafe { SelectObject(dc, f) };
            let mut buf = [0u16; 64];
            let n = unsafe { GetTextFaceW(dc, buf.len() as i32, buf.as_mut_ptr()) };
            let mut tm: TEXTMETRICW = unsafe { std::mem::zeroed() };
            unsafe { GetTextMetricsW(dc, &mut tm) };
            unsafe {
                SelectObject(dc, old);
                DeleteObject(f);
                DeleteDC(dc);
            }
            let got = String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize]);
            assert_eq!(got, face, "asked for {face}");
            assert_eq!(tm.tmWeight, w, "{face} weight");
        }
    }
}
