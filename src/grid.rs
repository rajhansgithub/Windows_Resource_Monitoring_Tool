//! Responsive grid math of the main window. Pure (no Win32), unit-tested.
//!
//! The content column of a page used to be a fixed 1000 DIP strip on the left.
//! Now the page decides how wide it is and how many columns its cards flow in
//! from the width of the window, in DIPs (device-independent pixels, 96 per
//! inch), so the same breakpoints hold at every display scale:
//!
//! * below 1100 DIP of content every page is the old column, exactly as before;
//! * Overview: summary cards in 2 columns from 1100, 3 from 1700;
//! * Settings pages: cards in 2 columns from 1300;
//! * Insights pages: 60 / 40 split from 1300 (headline, visual and chart on
//!   the left, the items list on the right);
//! * nothing is wider than `MAX_DIP` (2000), except the Server Room
//!   (Achievements), which fills the window as it always has.
//!
//! Layout runs on resize, navigation and data change only, never per frame.

/// Width of the single column below `WIDE_FROM` (DIP): the old fixed column.
pub const COLUMN_DIP: i32 = 1000;
/// No page is wider than this (DIP), the Server Room aside.
pub const MAX_DIP: i32 = 2000;
/// The Server Room is capped far out so a huge screen never stretches a rack.
pub const FILL_MAX_DIP: i32 = 4000;
/// Overview summary cards: 2 columns from here ...
pub const SUMMARY_2: i32 = 1100;
/// ... 3 columns from here.
pub const SUMMARY_3: i32 = 1700;
/// Settings cards: 2 columns from here.
pub const SETTINGS_2: i32 = 1300;
/// Insights panels: two columns from here.
pub const INSIGHTS_2: i32 = 1300;
/// Gap between cards of a grid, both ways (DIP).
pub const GAP_DIP: i32 = 8;
/// Gap between the two columns of an Insights page (DIP).
pub const SPLIT_GAP_DIP: i32 = 12;
/// Share of the Insights page width the left column takes (percent).
pub const SPLIT_LEFT_PCT: i32 = 60;
/// Chart plot height range on wide windows (DIP) and its share of the column width (percent).
pub const PLOT_MIN_DIP: i32 = 160;
pub const PLOT_MAX_DIP: i32 = 320;
pub const PLOT_PCT: i32 = 22;

/// How a page uses the width.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// Overview: full-width cards on top, a grid of summary cards.
    Overview,
    /// A list of setting cards that flow in columns.
    Settings,
    /// An Insights panel: two columns when wide.
    Insights,
    /// A list with ordering semantics (Metrics): one column, never gridded.
    List,
    /// The Server Room: fills the window.
    Fill,
}

fn sc(v: i32, dpi: u32) -> i32 {
    (v * dpi as i32 + 48) / 96
}

/// Pixels to DIPs.
pub fn dip(px: i32, dpi: u32) -> i32 {
    px * 96 / dpi.max(1) as i32
}

/// Width in px of the content column of a `class` page, given the px available
/// between the page margins.
pub fn content_w(class: Class, avail: i32, dpi: u32) -> i32 {
    let a = dip(avail, dpi);
    let cap = match class {
        Class::Fill => FILL_MAX_DIP,
        Class::List => COLUMN_DIP,
        Class::Overview if a >= SUMMARY_2 => MAX_DIP,
        Class::Settings if a >= SETTINGS_2 => MAX_DIP,
        Class::Insights if a >= INSIGHTS_2 => MAX_DIP,
        _ => COLUMN_DIP,
    };
    avail.min(sc(cap, dpi))
}

/// Number of columns the grid cards of a `class` page flow in, for a content
/// column `w` px wide.
pub fn card_cols(class: Class, w: i32, dpi: u32) -> usize {
    let d = dip(w, dpi);
    match class {
        Class::Overview if d >= SUMMARY_3 => 3,
        Class::Overview if d >= SUMMARY_2 => 2,
        Class::Settings | Class::Insights if d >= SETTINGS_2 => 2,
        _ => 1,
    }
}

/// Gap between grid cards in px (horizontal and vertical) for `cols` columns:
/// the old 4 DIP between stacked cards, `GAP_DIP` between cards of a grid.
pub fn gap(cols: usize, dpi: u32) -> i32 {
    sc(if cols > 1 { GAP_DIP } else { 4 }, dpi)
}

/// Left and right column of an Insights page `w` px wide: (left width, gap,
/// right width), or None when the page stays one column.
pub fn insight_split(w: i32, dpi: u32) -> Option<(i32, i32, i32)> {
    if dip(w, dpi) < INSIGHTS_2 {
        return None;
    }
    let g = sc(SPLIT_GAP_DIP, dpi);
    let left = (w - g) * SPLIT_LEFT_PCT / 100;
    Some((left, g, w - g - left))
}

/// Height of a chart plot in DIP for a column `col_dip` wide: it grows with the
/// column, clamped to `PLOT_MIN_DIP ..= PLOT_MAX_DIP`.
pub fn plot_h_dip(col_dip: i32) -> i32 {
    (col_dip * PLOT_PCT / 100).clamp(PLOT_MIN_DIP, PLOT_MAX_DIP)
}

/// Horizontal spans `(l, r)` of `n` equal columns in `l..r` with `gap` between
/// them. The remainder pixels go to the first columns so the last one ends
/// exactly at `r`.
pub fn columns(l: i32, r: i32, n: usize, gap: i32) -> Vec<(i32, i32)> {
    let n = n.max(1) as i32;
    let total = (r - l - gap * (n - 1)).max(n);
    let (base, extra) = (total / n, total % n);
    let mut x = l;
    (0..n)
        .map(|i| {
            let w = base + i32::from(i < extra);
            let c = (x, x + w);
            x += w + gap;
            c
        })
        .collect()
}

/// Rectangle `(l, t, r, b)` of a placed card.
pub type Cell = (i32, i32, i32, i32);

/// Flow items of the given `heights` into `n` equal columns in `l..r`, row by
/// row, left to right, starting at `y`. All items of a row get the height of
/// the tallest. Returns the cells in item order (which is the reading order,
/// so the order controls are added in is the keyboard focus order) and the
/// bottom of the last row (no trailing gap).
pub fn place(l: i32, r: i32, n: usize, gap: i32, y: i32, heights: &[i32]) -> (Vec<Cell>, i32) {
    let n = n.max(1);
    let cols = columns(l, r, n, gap);
    let mut cells = Vec::with_capacity(heights.len());
    let mut top = y;
    let mut bottom = y;
    for row in heights.chunks(n) {
        let h = row.iter().copied().max().unwrap_or(0);
        for (i, _) in row.iter().enumerate() {
            cells.push((cols[i].0, top, cols[i].1, top + h));
        }
        bottom = top + h;
        top = bottom + gap;
    }
    (cells, bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_pages_keep_the_old_column() {
        for class in [Class::Overview, Class::Settings, Class::Insights, Class::List] {
            // 900 DIP of room: the whole width, one column, as before
            assert_eq!(content_w(class, 900, 96), 900);
            assert_eq!(card_cols(class, 900, 96), 1);
            // 1050 DIP: still the old 1000 DIP column
            assert_eq!(content_w(class, 1050, 96), 1000);
            assert_eq!(card_cols(class, 1000, 96), 1);
        }
        // the Server Room always fills (to 4000 DIP)
        assert_eq!(content_w(Class::Fill, 3000, 96), 3000);
        assert_eq!(content_w(Class::Fill, 5000, 96), 4000);
    }

    #[test]
    fn column_count_by_width() {
        let n = |c, w| card_cols(c, w, 96);
        // Overview summary cards: 1 / 2 / 3
        assert_eq!(n(Class::Overview, 1099), 1);
        assert_eq!(n(Class::Overview, 1100), 2);
        assert_eq!(n(Class::Overview, 1699), 2);
        assert_eq!(n(Class::Overview, 1700), 3);
        assert_eq!(n(Class::Overview, 2000), 3);
        // settings cards: 1 / 2
        assert_eq!(n(Class::Settings, 1299), 1);
        assert_eq!(n(Class::Settings, 1300), 2);
        assert_eq!(n(Class::Settings, 2000), 2);
        // the Metrics list and the Server Room never grid
        assert_eq!(n(Class::List, 2000), 1);
        assert_eq!(n(Class::Fill, 2000), 1);
    }

    #[test]
    fn breakpoints_are_in_dips_at_every_scale() {
        // 1100 DIP is 1650 px at 150 % and 2200 px at 200 %
        assert_eq!(card_cols(Class::Overview, 1649, 144), 1);
        assert_eq!(card_cols(Class::Overview, 1650, 144), 2);
        assert_eq!(card_cols(Class::Overview, 2199, 192), 1);
        assert_eq!(card_cols(Class::Overview, 2200, 192), 2);
        // 1300 DIP at 125 % is 1625 px
        assert_eq!(card_cols(Class::Settings, 1624, 120), 1);
        assert_eq!(card_cols(Class::Settings, 1625, 120), 2);
    }

    #[test]
    fn width_is_capped_at_2000_dip_when_wide() {
        assert_eq!(content_w(Class::Overview, 3000, 96), 2000);
        assert_eq!(content_w(Class::Settings, 3000, 96), 2000);
        assert_eq!(content_w(Class::Insights, 3000, 96), 2000);
        assert_eq!(content_w(Class::Settings, 5000, 192), 4000); // 2000 DIP at 200 %
        assert_eq!(content_w(Class::Overview, 1500, 96), 1500);
        // the Metrics list stays the old column
        assert_eq!(content_w(Class::List, 3000, 96), 1000);
        // a settings page between 1100 and 1300 is still the old column
        assert_eq!(content_w(Class::Settings, 1200, 96), 1000);
        assert_eq!(content_w(Class::Insights, 1250, 96), 1000);
        assert_eq!(content_w(Class::Overview, 1200, 96), 1200);
    }

    #[test]
    fn columns_tile_the_row() {
        for (w, n, gap) in [(1000, 2, 8), (1001, 2, 8), (1999, 3, 12), (661, 3, 8), (500, 1, 4), (7, 3, 0)] {
            let cols = columns(100, 100 + w, n, gap);
            assert_eq!(cols.len(), n);
            assert_eq!(cols[0].0, 100);
            assert_eq!(cols[n - 1].1, 100 + w);
            for p in cols.windows(2) {
                assert_eq!(p[1].0 - p[0].1, gap);
            }
            let widths: Vec<i32> = cols.iter().map(|c| c.1 - c.0).collect();
            let (lo, hi) = (*widths.iter().min().unwrap(), *widths.iter().max().unwrap());
            assert!(hi - lo <= 1, "columns differ by more than a pixel: {widths:?}");
        }
    }

    #[test]
    fn rows_have_equal_height() {
        // 5 cards in 2 columns: rows (64, 64), (92, 64), (64)
        let (cells, bottom) = place(24, 1024, 2, 8, 0, &[64, 64, 92, 64, 64]);
        assert_eq!(cells.len(), 5);
        let h = |c: &Cell| c.3 - c.1;
        assert_eq!(h(&cells[0]), 64);
        assert_eq!(h(&cells[1]), 64);
        // the shorter card of a row grows to the tallest
        assert_eq!(h(&cells[2]), 92);
        assert_eq!(h(&cells[3]), 92);
        assert_eq!(cells[2].1, cells[3].1);
        assert_eq!(cells[4].1, 64 + 8 + 92 + 8);
        assert_eq!(bottom, cells[4].3);
        // rows are `gap` apart
        assert_eq!(cells[2].1 - cells[0].3, 8);
        // a lone last card keeps one column's width
        assert_eq!(cells[4].0, 24);
        assert_eq!(cells[4].2, cells[0].2);
    }

    #[test]
    fn single_column_stacks_like_before() {
        let (cells, bottom) = place(24, 1024, 1, 4, 10, &[64, 64, 64]);
        assert_eq!(cells[0], (24, 10, 1024, 74));
        assert_eq!(cells[1], (24, 78, 1024, 142));
        assert_eq!(cells[2], (24, 146, 1024, 210));
        assert_eq!(bottom, 210);
        assert_eq!(place(0, 100, 3, 8, 5, &[]), (vec![], 5));
    }

    #[test]
    fn focus_order_is_row_by_row_left_to_right() {
        // items are added to the window in item order, so that order is the Tab order:
        // it must read top to bottom, then left to right inside a row
        for n in 1..=3 {
            let heights: Vec<i32> = (0..11).map(|i| 64 + (i % 3) * 4).collect();
            let (cells, _) = place(24, 2024, n, 8, 0, &heights);
            for p in cells.windows(2) {
                let (a, b) = (p[0], p[1]);
                assert!(b.1 > a.1 || (b.1 == a.1 && b.0 > a.2), "{a:?} then {b:?} is not reading order");
            }
        }
    }

    #[test]
    fn grid_cells_never_overlap() {
        let (cells, _) = place(0, 1000, 3, 8, 0, &[50, 80, 50, 50, 70, 50, 90]);
        for (i, a) in cells.iter().enumerate() {
            for b in &cells[i + 1..] {
                let apart = a.2 <= b.0 || b.2 <= a.0 || a.3 <= b.1 || b.3 <= a.1;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn insights_split_at_1300() {
        assert_eq!(insight_split(1299, 96), None);
        let (l, g, r) = insight_split(1300, 96).unwrap();
        assert_eq!(g, 12);
        assert_eq!(l + g + r, 1300);
        assert_eq!(l, (1300 - 12) * 60 / 100);
        assert!(l > r);
        // 2000 DIP at 200 % is 4000 px
        let (l, g, r) = insight_split(4000, 192).unwrap();
        assert_eq!(l + g + r, 4000);
        assert_eq!(g, 24);
        assert_eq!(insight_split(2599, 192), None);
        assert!(insight_split(2600, 192).is_some());
    }

    #[test]
    fn chart_height_scales_with_the_column_and_is_clamped() {
        assert_eq!(plot_h_dip(100), 160);
        assert_eq!(plot_h_dip(773), 170);
        assert_eq!(plot_h_dip(1000), 220);
        assert_eq!(plot_h_dip(1193), 262);
        assert_eq!(plot_h_dip(3000), 320);
        let mut last = 0;
        for w in (200..2400).step_by(50) {
            let h = plot_h_dip(w);
            assert!((160..=320).contains(&h));
            assert!(h >= last);
            last = h;
        }
    }
}
