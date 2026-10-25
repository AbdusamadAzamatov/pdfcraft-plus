//! Organize ▸ Layout ▸ Trim white margins (PL8), and the Remove white margins button of Set Page
//! Boxes: crop each page to what is drawn on it.
//!
//! Each page is drawn at [`DPI`] as it is shown (comments included: cropping one away would hide
//! it) and read in grey; the bounding box of the pixels whose luma is below [`INK_LUMA`] is the
//! ink. That box, grown by the padding and kept inside the page, is mapped back to default user
//! space through the page's crop box and `/Rotate` (the page is drawn in shown points, so
//! `/UserUnit` cancels out), and becomes its new crop box. All pages change in one undo step
//! ([`TrimPlan::edit`], a batch of [`Edit::SetPageBox`]).
//!
//! A page with no ink, a page whose ink already reaches every edge, and a page that can't be drawn
//! in full are left alone and reported ([`LeftAlone`]).

use pdfcraft_render::{PRINT_TAG, PageInfo, PageRenderer, RenderRequest};

use crate::{BoxSpec, DocId, Edit, EditError, PageBox, Session, guard};

/// The resolution pages are drawn at to find their ink.
pub const DPI: f32 = 72.0;
/// A pixel whose luma (0–255, composed over white paper) is below this is ink.
pub const INK_LUMA: u32 = 245;
/// The largest padding offered around the ink, in points.
pub const MAX_PADDING: f64 = 36.0;
/// The undo label of a trim (one of [`crate::BATCH_LABELS`]).
pub const TRIM_LABEL: &str = "Remove white margins";
/// A side moving by less than this (points) doesn't count as a margin removed.
const MIN_MARGIN: f64 = 0.5;

/// What to trim.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrimOptions {
    /// White space kept around the ink, in points (0 to [`MAX_PADDING`]).
    pub padding: f64,
    /// The pages to trim (0-based); empty trims every page.
    pub pages: Vec<usize>,
}

/// Why a page is left alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeftAlone {
    /// Nothing is drawn on it: there is nothing to crop to.
    NoInk,
    /// Its ink (with the padding) already reaches every edge.
    NoMargin,
    /// It couldn't be drawn, or was drawn only in part, so its ink is unknown.
    NotDrawn,
}

impl LeftAlone {
    /// A short name for reports: `no_ink`, `no_margin`, `not_drawn`.
    pub fn name(self) -> &'static str {
        match self {
            LeftAlone::NoInk => "no_ink",
            LeftAlone::NoMargin => "no_margin",
            LeftAlone::NotDrawn => "not_drawn",
        }
    }
}

/// What a trim does: the new crop boxes, and the pages left alone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrimPlan {
    /// (0-based page, its new crop box in default user space), in page order.
    pub crops: Vec<(usize, [f64; 4])>,
    /// (0-based page, why), in page order.
    pub left: Vec<(usize, LeftAlone)>,
}

impl TrimPlan {
    /// The edit that sets the new crop boxes, as one undo step; `None` when no page changes.
    pub fn edit(&self) -> Option<Edit> {
        if self.crops.is_empty() {
            return None;
        }
        let edits =
            self.crops.iter().map(|&(page, rect)| Edit::SetPageBox { pages: vec![page], which: PageBox::Crop, spec: BoxSpec::Rect(rect) }).collect();
        Some(Edit::Batch { label: TRIM_LABEL.into(), edits })
    }
}

/// The luma (Rec. 601, 0–255) of a premultiplied RGBA pixel composed over white paper.
fn luma_over_white(px: &[u8; 4]) -> u32 {
    let gap = 255 - u32::from(px[3]);
    let [r, g, b] = [px[0], px[1], px[2]].map(|c| (u32::from(c) + gap).min(255));
    ((299 * r + 587 * g + 114 * b) / 1000).min(255)
}

/// The bounding box `[x0, y0, x1, y1]` (pixels, top-left origin, `x1`/`y1` exclusive) of the ink
/// in a premultiplied RGBA8 raster `width` × `height` pixels; `None` when there is none. Rows
/// missing from a short buffer, and a trailing partial pixel, count as paper.
pub fn ink_bounds(rgba: &[u8], width: u32, height: u32) -> Option<[u32; 4]> {
    let row_len = usize::try_from(width).ok()?.checked_mul(4)?;
    if row_len == 0 {
        return None;
    }
    let rows = usize::try_from(height).ok()?;
    let mut found: Option<[u32; 4]> = None;
    for (y, row) in rgba.chunks_exact(row_len).take(rows).enumerate() {
        let pixels = row.as_chunks::<4>().0;
        let Some(first) = pixels.iter().position(|px| luma_over_white(px) < INK_LUMA) else { continue };
        // A row with ink has a last ink pixel too, at or after the first.
        let last = pixels.iter().rposition(|px| luma_over_white(px) < INK_LUMA).unwrap_or(first);
        // Every index is below `width` or `height`, both u32.
        let (x0, x1, y) = (first as u32, last as u32 + 1, y as u32);
        found = Some(match found {
            None => [x0, y, x1, y + 1],
            Some([a, b, c, _]) => [a.min(x0), b, c.max(x1), y + 1],
        });
    }
    found
}

/// A point of the shown page (points, top-left origin, y down) → default user space, for a page
/// whose crop box is `crop`, turned `rotation` degrees clockwise and shown `w` × `h` points.
fn view_to_user(crop: [f64; 4], rotation: u16, (w, h): (f64, f64), x: f64, y: f64) -> [f64; 2] {
    let [cx0, cy0, cx1, cy1] = crop;
    let (a, b) = (x / w.max(1e-3), y / h.max(1e-3));
    let (u, v) = match rotation {
        90 => (b, 1.0 - a),
        180 => (1.0 - a, 1.0 - b),
        270 => (1.0 - b, a),
        _ => (a, b),
    };
    [cx0 + u * (cx1 - cx0), cy1 - v * (cy1 - cy0)]
}

/// The new crop box of a page whose ink covers `ink` (pixels at `scale` pixels per point), with
/// `padding` points around it, or why the page keeps its crop box.
fn crop_for(info: &PageInfo, ink: [u32; 4], scale: f64, padding: f64) -> Result<[f64; 4], LeftAlone> {
    let (w, h) = (f64::from(info.width), f64::from(info.height));
    if !(scale.is_finite() && scale > 0.0 && w > 0.0 && h > 0.0) {
        return Err(LeftAlone::NotDrawn);
    }
    let [x0, y0, x1, y1] = ink.map(|v| f64::from(v) / scale);
    let view = [(x0 - padding).max(0.0), (y0 - padding).max(0.0), (x1 + padding).min(w), (y1 + padding).min(h)];
    if view[0] < MIN_MARGIN && view[1] < MIN_MARGIN && view[2] > w - MIN_MARGIN && view[3] > h - MIN_MARGIN {
        return Err(LeftAlone::NoMargin);
    }
    let crop = info.crop.map(f64::from);
    let (a, b) = (view_to_user(crop, info.rotation, (w, h), view[0], view[1]), view_to_user(crop, info.rotation, (w, h), view[2], view[3]));
    let mut r = [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])];
    if !r.iter().all(|v| v.is_finite()) {
        return Err(LeftAlone::NotDrawn);
    }
    // A page box must be at least a unit a side (a speck on a page with a large /UserUnit may
    // map to less): grow it about its centre.
    for (lo, hi) in [(0, 2), (1, 3)] {
        let short = 1.0 - (r[hi] - r[lo]);
        if short > 0.0 {
            r[lo] -= short / 2.0;
            r[hi] += short / 2.0;
        }
    }
    Ok(r)
}

impl Session {
    /// What Trim white margins would do to document `id`, without changing it: each page drawn
    /// at [`DPI`], its ink found, and its new crop box worked out. Refuses a padding outside 0 to
    /// [`MAX_PADDING`] points and pages that don't exist.
    pub fn plan_trim_margins(&self, id: DocId, opts: &TrimOptions) -> Result<TrimPlan, EditError> {
        if !opts.padding.is_finite() || !(0.0..=MAX_PADDING).contains(&opts.padding) {
            return Err(EditError::Invalid(format!("the padding must be between 0 and {MAX_PADDING} points, not {}", opts.padding)));
        }
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let infos = doc.info.pages.clone();
        let count = infos.len();
        let mut pages: Vec<usize> = if opts.pages.is_empty() { (0..count).collect() } else { opts.pages.clone() };
        if let Some(&bad) = pages.iter().find(|&&p| p >= count) {
            return Err(EditError::Invalid(format!("page {} doesn't exist: the document has {count} pages", bad.saturating_add(1))));
        }
        pages.sort_unstable();
        pages.dedup();
        let src = doc.export_source();
        let mut config = src.config;
        // The page as it prints and shows: comments drawn, so none is cropped away.
        config.hide_comments = false;
        let padding = opts.padding;
        guard(move || {
            let mut renderer = PageRenderer::new(src.bytes, config);
            let mut plan = TrimPlan::default();
            for page in pages {
                let Some(info) = infos.get(page) else { continue };
                let scale = pdfcraft_render::effective_scale(info.width, info.height, DPI / 72.0);
                let r = renderer.render(RenderRequest { page, scale, tag: PRINT_TAG, ..Default::default() });
                // A page drawn only in part may have ink in the part that wasn't drawn.
                let found = if r.error.is_some() || !r.warnings.is_empty() {
                    Err(LeftAlone::NotDrawn)
                } else {
                    match ink_bounds(r.rgba.as_ref(), r.width, r.height) {
                        None => Err(LeftAlone::NoInk),
                        Some(ink) => crop_for(info, ink, f64::from(scale), padding),
                    }
                };
                match found {
                    Ok(rect) => plan.crops.push((page, rect)),
                    Err(why) => plan.left.push((page, why)),
                }
            }
            plan
        })
        .map_err(|e| EditError::Invalid(format!("couldn't look for the white margins: {e}")))
    }

    /// Organize ▸ Layout ▸ Trim white margins: crop the pages of document `id` to their ink plus
    /// the padding, as one undo step (none when no page changes). Returns what was done, with the
    /// pages left alone. Needs the permission to change pages.
    pub fn trim_margins(&mut self, id: DocId, opts: &TrimOptions) -> Result<TrimPlan, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        if doc.editor.is_none() {
            return Err(EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()));
        }
        if !doc.allows_assembly() {
            return Err(EditError::NotPermitted("page changes"));
        }
        let plan = self.plan_trim_margins(id, opts)?;
        if let Some(edit) = plan.edit() {
            self.apply(id, edit)?;
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];

    /// A `w` × `h` white raster with black pixels at `ink`.
    fn raster(w: usize, h: usize, ink: &[(usize, usize)]) -> Vec<u8> {
        let mut px = vec![WHITE; w * h];
        for &(x, y) in ink {
            px[y * w + x] = BLACK;
        }
        px.into_iter().flatten().collect()
    }

    #[test]
    fn ink_bounds_cover_every_dark_pixel_and_nothing_else() {
        assert_eq!(ink_bounds(&raster(10, 8, &[]), 10, 8), None);
        assert_eq!(ink_bounds(&raster(10, 8, &[(3, 2)]), 10, 8), Some([3, 2, 4, 3]));
        assert_eq!(ink_bounds(&raster(10, 8, &[(3, 2), (7, 5), (1, 4)]), 10, 8), Some([1, 2, 8, 6]));
        assert_eq!(ink_bounds(&raster(10, 8, &[(0, 0), (9, 7)]), 10, 8), Some([0, 0, 10, 8]));
        // Light grey (luma 245) is paper, a shade darker is ink; clear pixels are paper.
        let mut px = raster(4, 1, &[]);
        px[..4].copy_from_slice(&[245, 245, 245, 255]);
        px[4..8].copy_from_slice(&[0, 0, 0, 0]);
        assert_eq!(ink_bounds(&px, 4, 1), None);
        px[8..12].copy_from_slice(&[244, 244, 244, 255]);
        assert_eq!(ink_bounds(&px, 4, 1), Some([2, 0, 3, 1]));
    }

    #[test]
    fn ink_bounds_never_panic_on_odd_sizes() {
        assert_eq!(ink_bounds(&[], 0, 0), None);
        assert_eq!(ink_bounds(&[0, 0, 0, 255], 0, 5), None);
        assert_eq!(ink_bounds(&[0, 0, 0, 255], u32::MAX, u32::MAX), None, "a short buffer has no full row");
        // Rows beyond the buffer are paper; rows beyond `height` are ignored.
        assert_eq!(ink_bounds(&raster(2, 3, &[(1, 2)]), 2, 2), None);
        assert_eq!(ink_bounds(&raster(2, 1, &[(1, 0)]), 2, 9), Some([1, 0, 2, 1]));
    }

    fn info(width: f32, height: f32, crop: [f32; 4], rotation: u16) -> PageInfo {
        PageInfo { width, height, label: String::new(), crop, rotation }
    }

    /// `got` is a crop box within a millionth of a point of `want`.
    #[track_caller]
    fn near(got: Result<[f64; 4], LeftAlone>, want: [f64; 4]) {
        let got = got.unwrap();
        assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-6), "{got:?} != {want:?}");
    }

    #[test]
    fn the_ink_box_maps_back_through_the_crop_origin_and_rotation() {
        // Unturned, crop box away from the origin: x 100..400, y 50..450 (300 × 400 shown).
        let p = info(300.0, 400.0, [100.0, 50.0, 400.0, 450.0], 0);
        // Ink from (10, 20) to (110, 60) shown points, top-left origin.
        near(crop_for(&p, [10, 20, 110, 60], 1.0, 0.0), [110.0, 390.0, 210.0, 430.0]);
        near(crop_for(&p, [10, 20, 110, 60], 1.0, 5.0), [105.0, 385.0, 215.0, 435.0]);
        // The padding stops at the page's edge; at 2 pixels a point the box is the same.
        near(crop_for(&p, [0, 40, 220, 120], 2.0, 5.0), [100.0, 385.0, 215.0, 435.0]);
        // Turned 90° clockwise: the page shows 400 × 300; its shown top-left is the crop's
        // bottom-left, and shown x runs up the user y axis.
        let p = info(400.0, 300.0, [100.0, 50.0, 400.0, 450.0], 90);
        near(crop_for(&p, [10, 20, 110, 60], 1.0, 0.0), [120.0, 60.0, 160.0, 160.0]);
        let p = info(300.0, 400.0, [100.0, 50.0, 400.0, 450.0], 180);
        near(crop_for(&p, [10, 20, 110, 60], 1.0, 0.0), [290.0, 70.0, 390.0, 110.0]);
        let p = info(400.0, 300.0, [100.0, 50.0, 400.0, 450.0], 270);
        near(crop_for(&p, [10, 20, 110, 60], 1.0, 0.0), [340.0, 340.0, 380.0, 440.0]);
    }

    #[test]
    fn ink_at_every_edge_or_odd_sizes_leave_the_page_alone() {
        let p = info(300.0, 400.0, [0.0, 0.0, 300.0, 400.0], 0);
        assert_eq!(crop_for(&p, [0, 0, 300, 400], 1.0, 0.0), Err(LeftAlone::NoMargin));
        assert_eq!(crop_for(&p, [4, 4, 296, 396], 1.0, 4.0), Err(LeftAlone::NoMargin), "the padding fills the margin");
        assert!(crop_for(&p, [4, 4, 296, 396], 1.0, 3.0).is_ok());
        assert_eq!(crop_for(&p, [1, 1, 2, 2], 0.0, 0.0), Err(LeftAlone::NotDrawn));
        assert_eq!(crop_for(&p, [1, 1, 2, 2], f64::NAN, 0.0), Err(LeftAlone::NotDrawn));
        assert_eq!(crop_for(&info(0.0, 400.0, [0.0; 4], 0), [1, 1, 2, 2], 1.0, 0.0), Err(LeftAlone::NotDrawn));
        // A speck smaller than a unit (a large /UserUnit) grows to a unit about its centre.
        let big = info(300.0, 400.0, [0.0, 0.0, 30.0, 40.0], 0);
        let r = crop_for(&big, [10, 10, 11, 11], 1.0, 0.0).unwrap();
        assert!((r[2] - r[0] - 1.0).abs() < 1e-9 && (r[3] - r[1] - 1.0).abs() < 1e-9, "{r:?}");
        assert!((r[0] + r[2] - 2.1).abs() < 1e-9, "centred on the speck: {r:?}");
    }

    /// A PDF whose pages are the given page dictionary entries (each with its content stream).
    fn pdf(pages: &[(&str, &str)]) -> Vec<u8> {
        let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
        let mut objs = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        for (i, (dict, content)) in pages.iter().enumerate() {
            objs.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> {dict} >>", 5 + 2 * i));
            objs.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
        }
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
        out
    }

    /// Each side of `got` is within `tol` points of `want`.
    #[track_caller]
    fn within(got: [f64; 4], want: [f64; 4], tol: f64) {
        assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() <= tol), "{got:?} is not within {tol} of {want:?}");
    }

    /// Trim white margins on a large page with a box in its middle, a page of text, a page turned
    /// 90° with a crop box away from the origin, a blank page and a page inked to its edges: the
    /// first three are cropped to their ink plus the padding, the blank page and the full page are
    /// left alone and reported, all in one undo step.
    #[test]
    fn trim_crops_to_the_ink_plus_padding_and_leaves_blank_pages_alone() {
        let bytes = pdf(&[
            ("/MediaBox [0 0 1000 1200]", "0 g 300 400 200 100 re f"),
            ("/MediaBox [0 0 1000 1200]", "BT /F1 24 Tf 450 600 Td (Hello) Tj ET"),
            ("/MediaBox [0 0 600 800] /CropBox [50 100 550 700] /Rotate 90", "0 g 100 200 50 100 re f"),
            ("/MediaBox [0 0 600 800]", ""),
            ("/MediaBox [0 0 200 300]", "0 g 0 0 200 300 re f"),
        ]);
        let mut s = Session::new();
        let id = s.open("trim.pdf", None, std::sync::Arc::new(bytes), None).unwrap();
        let before = s.get(id).unwrap().page_boxes();
        let plan = s.trim_margins(id, &TrimOptions { padding: 10.0, pages: Vec::new() }).unwrap();
        assert_eq!(plan.crops.iter().map(|c| c.0).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(plan.left, [(3, LeftAlone::NoInk), (4, LeftAlone::NoMargin)]);
        let doc = s.get(id).unwrap();
        let boxes = doc.page_boxes();
        // The box: exactly its edges (a pixel is a point at 72 dpi), plus 10 pt.
        within(boxes[0][1], [290.0, 390.0, 510.0, 510.0], 1.01);
        // The text: Helvetica 24 pt "Hello" is 54.7 pt wide (widths 722 556 222 222 556), its
        // capitals 17 pt tall above the baseline; the crop hugs it.
        within(boxes[1][1], [440.0, 590.0, 450.0 + 54.7 + 10.0, 600.0 + 17.2 + 10.0], 2.0);
        // The turned page: the crop box is in user space, the page's own coordinates.
        within(boxes[2][1], [90.0, 190.0, 160.0, 310.0], 1.01);
        assert_eq!((doc.info.pages[2].width.round(), doc.info.pages[2].height.round()), (120.0, 70.0), "shown turned");
        assert_eq!(boxes[3], before[3], "the blank page is unchanged");
        assert_eq!(boxes[4], before[4], "the full page is unchanged");
        assert_eq!(doc.can_undo(), Some(TRIM_LABEL));
        assert!(crate::BATCH_LABELS.contains(&TRIM_LABEL));
        s.undo(id).unwrap();
        assert_eq!(s.get(id).unwrap().page_boxes(), before, "one undo step puts every page back");
        assert_eq!(s.get(id).unwrap().can_undo(), None);

        // Only some pages; no padding hugs the ink.
        let plan = s.plan_trim_margins(id, &TrimOptions { padding: 0.0, pages: vec![3, 0, 0] }).unwrap();
        assert_eq!(plan.left, [(3, LeftAlone::NoInk)]);
        assert_eq!(plan.crops.len(), 1);
        within(plan.crops[0].1, [300.0, 400.0, 500.0, 500.0], 1.01);
        // A blank page alone changes nothing at all.
        let plan = s.trim_margins(id, &TrimOptions { padding: 0.0, pages: vec![3] }).unwrap();
        assert!(plan.crops.is_empty() && plan.edit().is_none());
        assert_eq!(s.get(id).unwrap().can_undo(), None, "no undo step");
        // Wild values are refused with what is allowed.
        for bad in [-1.0, 36.5, f64::NAN, f64::INFINITY] {
            let e = s.plan_trim_margins(id, &TrimOptions { padding: bad, pages: Vec::new() }).unwrap_err();
            assert!(e.to_string().contains("between 0 and 36"), "{e}");
        }
        let e = s.plan_trim_margins(id, &TrimOptions { padding: 0.0, pages: vec![5] }).unwrap_err();
        assert!(e.to_string().contains("page 6"), "{e}");
    }
}
