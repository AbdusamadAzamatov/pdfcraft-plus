//! Edit a PDF ▸ Paging seal: one stamp picture cut into vertical strips, a strip on the edge of
//! each page, so the seal reads whole when the pages are fanned out along that edge (a
//! cross-page or "riding" seal, which shows that no page was swapped).
//!
//! Each strip is an ordinary added image drawn from one embedded picture and cropped to its
//! share, so it can be moved or deleted like any added image and the picture is stored once.

use pdfcraft_edit::AddedImage;

/// The most pages one seal is split across: a strip keeps at least the share of the picture a
/// crop may keep ([`AddedImage::MIN_CROP_KEEP`], 5 %). Longer runs of pages are split into
/// groups of about the same size, each with the whole seal.
pub const MAX_PAGES_PER_SEAL: usize = 20;

/// The edge of the page the seal goes on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SealEdge {
    #[default]
    Right,
    Left,
}

impl SealEdge {
    pub const ALL: [SealEdge; 2] = [SealEdge::Right, SealEdge::Left];

    /// The name tools use.
    pub fn name(self) -> &'static str {
        match self {
            SealEdge::Right => "right",
            SealEdge::Left => "left",
        }
    }

    pub fn from_name(name: &str) -> Option<SealEdge> {
        SealEdge::ALL.into_iter().find(|e| e.name() == name)
    }
}

/// Where a paging seal goes and how large it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PagingSeal {
    /// The whole seal's width, in points (each page shows its share of it).
    pub width: f64,
    /// The height of the seal's centre, as a share of the page's height from the top (0–1).
    pub position: f64,
    pub edge: SealEdge,
}

impl Default for PagingSeal {
    fn default() -> Self {
        // About 4 cm, the size of a common company seal, halfway down the page.
        PagingSeal { width: 113.0, position: 0.5, edge: SealEdge::Right }
    }
}

/// One page's part of a seal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strip {
    /// Its box, in display space (points, y up).
    pub rect: [f64; 4],
    /// The fractions of the picture trimmed from the left, bottom, right and top so that only
    /// this strip shows (an [`AddedImage`] crop).
    pub crop: [f64; 4],
}

/// The strips of a seal of `natural` shape (width, height, any unit) on pages of the displayed
/// sizes `pages` (points), one per page in order. Errors say what is wrong with the seal.
pub fn strips(pages: &[(f64, f64)], natural: (f64, f64), seal: &PagingSeal) -> Result<Vec<Strip>, String> {
    if pages.is_empty() {
        return Err("choose at least one page".into());
    }
    if !(seal.width.is_finite() && seal.width > 0.0) {
        return Err("the seal's width must be more than 0 points".into());
    }
    if !(seal.position.is_finite() && (0.0..=1.0).contains(&seal.position)) {
        return Err("the seal's position must be between 0 (top) and 1 (bottom)".into());
    }
    let (nw, nh) = natural;
    if !(nw.is_finite() && nh.is_finite() && nw > 0.0 && nh > 0.0) {
        return Err("the seal picture has no size".into());
    }
    let groups = pages.len().div_ceil(MAX_PAGES_PER_SEAL);
    let (base, extra) = (pages.len() / groups, pages.len() % groups);
    let mut out = Vec::with_capacity(pages.len());
    let mut sizes = pages.iter();
    for g in 0..groups {
        // The first `extra` groups take one page more, so no group is much shorter.
        let n = base + usize::from(g < extra);
        for i in 0..n {
            let Some(&(pw, ph)) = sizes.next() else { break };
            if !(pw.is_finite() && ph.is_finite() && pw > 0.0 && ph > 0.0) {
                return Err("a page has no size".into());
            }
            // The seal keeps its shape, fits the page's height and its strip the page's width.
            let share = 1.0 / n as f64;
            let k = (seal.width / nw).min(ph / nh).min(pw / (nw * share));
            let (sw, sh) = (nw * k, nh * k);
            let strip = sw * share;
            let cy = (ph * (1.0 - seal.position)).clamp(sh / 2.0, ph - sh / 2.0);
            let (y0, y1) = (cy - sh / 2.0, cy + sh / 2.0);
            // Fanned out to the right, the first page's edge is the leftmost: it shows the first
            // strip. Fanned out to the left, it is the rightmost.
            let from_left = match seal.edge {
                SealEdge::Right => i,
                SealEdge::Left => n - 1 - i,
            };
            let (l, r) = (from_left as f64 * share, (n - 1 - from_left) as f64 * share);
            let rect = match seal.edge {
                SealEdge::Right => [pw - strip, y0, pw, y1],
                SealEdge::Left => [0.0, y0, strip, y1],
            };
            out.push(Strip { rect, crop: AddedImage::clamp_crop([l, 0.0, r, 0.0]) });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A4: (f64, f64) = (595.0, 842.0);

    #[test]
    fn a_square_seal_on_four_pages_is_four_quarter_strips_on_the_right_edge() {
        let seal = PagingSeal { width: 120.0, position: 0.5, edge: SealEdge::Right };
        let s = strips(&[A4; 4], (400.0, 400.0), &seal).unwrap();
        assert_eq!(s.len(), 4);
        for (i, Strip { rect, crop }) in s.iter().enumerate() {
            assert_eq!(*rect, [565.0, 361.0, 595.0, 481.0], "page {i}: a 30 pt strip, 120 pt tall, centred");
            let expect = [i as f64 * 0.25, 0.0, (3 - i) as f64 * 0.25, 0.0];
            assert!(crop.iter().zip(expect).all(|(a, b)| (a - b).abs() < 1e-9), "page {i}: {crop:?}");
            assert!(AddedImage::crop_valid(*crop));
        }
    }

    #[test]
    fn on_the_left_edge_the_first_page_shows_the_rightmost_strip() {
        let seal = PagingSeal { edge: SealEdge::Left, ..Default::default() };
        let s = strips(&[A4; 2], (100.0, 100.0), &seal).unwrap();
        assert_eq!(s[0].rect[0], 0.0);
        assert_eq!((s[0].crop[0], s[0].crop[2]), (0.5, 0.0), "page 1: the right half");
        assert_eq!((s[1].crop[0], s[1].crop[2]), (0.0, 0.5), "page 2: the left half");
    }

    #[test]
    fn the_position_moves_the_seal_and_keeps_it_on_the_page() {
        let at = |position| strips(&[A4], (100.0, 100.0), &PagingSeal { width: 100.0, position, edge: SealEdge::Right }).unwrap()[0].rect;
        assert_eq!(at(0.0)[3], 842.0, "the top");
        assert_eq!(at(1.0)[1], 0.0, "the bottom");
        assert_eq!(at(0.25)[1] + 50.0, 842.0 * 0.75);
        // Taller than the page: shrunk to its height, keeping its shape.
        let tall = strips(&[(300.0, 200.0)], (100.0, 400.0), &PagingSeal::default()).unwrap()[0].rect;
        assert_eq!(tall, [250.0, 0.0, 300.0, 200.0]);
        // A one-page seal wider than the page is as wide as the page, keeping its shape.
        let wide = strips(&[(50.0, 800.0)], (100.0, 100.0), &PagingSeal::default()).unwrap()[0].rect;
        assert_eq!(wide, [0.0, 375.0, 50.0, 425.0]);
    }

    #[test]
    fn long_runs_are_split_into_even_groups_each_with_the_whole_seal() {
        let s = strips(&[A4; 45], (100.0, 100.0), &PagingSeal::default()).unwrap();
        assert_eq!(s.len(), 45);
        // 45 pages: three groups of 15, each starting from the seal's left edge.
        for start in [0, 15, 30] {
            assert_eq!(s[start].crop[0], 0.0, "group at page {start}");
            assert!((s[start + 14].crop[2]).abs() < 1e-9, "group ends at page {}", start + 14);
        }
        assert!(s.iter().all(|s| AddedImage::crop_valid(s.crop)));
        let s = strips(&[A4; 21], (100.0, 100.0), &PagingSeal::default()).unwrap();
        // 21 pages: 11 and 10.
        assert_eq!((s[10].crop[2], s[11].crop[0]), (0.0, 0.0));
        let twenty = strips(&[A4; 20], (100.0, 100.0), &PagingSeal::default()).unwrap();
        assert!(twenty.iter().all(|s| AddedImage::crop_valid(s.crop)), "20 pages are one group");
    }

    #[test]
    fn bad_seals_and_pages_are_errors_not_panics() {
        let ok = PagingSeal::default();
        assert!(strips(&[], (1.0, 1.0), &ok).is_err());
        for width in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            assert!(strips(&[A4], (1.0, 1.0), &PagingSeal { width, ..ok }).is_err(), "{width}");
        }
        for position in [-0.1, 1.1, f64::NAN] {
            assert!(strips(&[A4], (1.0, 1.0), &PagingSeal { position, ..ok }).is_err(), "{position}");
        }
        assert!(strips(&[A4], (0.0, 1.0), &ok).is_err());
        assert!(strips(&[(0.0, 842.0)], (1.0, 1.0), &ok).is_err());
        assert!(strips(&[(f64::NAN, 842.0)], (1.0, 1.0), &ok).is_err());
        for name in ["right", "left"] {
            assert_eq!(SealEdge::from_name(name).map(SealEdge::name), Some(name));
        }
        assert_eq!(SealEdge::from_name("top"), None);
    }
}
