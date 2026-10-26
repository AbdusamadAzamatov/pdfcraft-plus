//! Edit a PDF ▸ Mosaic: hide an area of a page behind coarse squares of its own colours, the
//! way a face or a number is pixelated in a photo, so the page still looks whole. The areas are
//! one given area, or the redaction marks (Redact ▸ Apply as mosaic).
//!
//! What the area showed is removed for good first, as a redaction does (its text, the image
//! pixels and the drawings under it), and a picture of the squares is placed over it. Unlike a
//! blur in a picture editor, nothing of the original stays in the saved file to be recovered:
//! the squares are averages of the area as it was drawn, one colour per square.

use std::sync::Arc;

use pdfcraft_render::{RenderConfig, RenderRequest};

use crate::{DocId, Edit, EditError, Session};

/// The side of a square, in points, when none is chosen.
pub const DEFAULT_CELL: f64 = 8.0;
/// The smallest and largest square sides, in points.
pub const MIN_CELL: f64 = 2.0;
pub const MAX_CELL: f64 = 72.0;
/// The smallest side, in points, an area may have.
const MIN_SIDE: f64 = 1.0;
/// Pixels per square side in the picture: sharp squares even where a viewer smooths pictures
/// as it enlarges them.
const PX_PER_CELL: usize = 8;
/// The most pixels the page is rendered with, and the picture has.
const MAX_PIXELS: f64 = 16_000_000.0;

/// The squares picture of the part `region` (raster pixels: x0, y0, x1, y1) of a premultiplied
/// RGBA raster, composited over white: about `cell` pixels per square, `px` picture pixels per
/// square side. Returns the picture's width, height and RGB8 pixels.
fn squares(rgba: &[u8], width: u32, height: u32, region: [f64; 4], cell: f64, px: usize) -> Result<(u32, u32, Vec<u8>), String> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || w.checked_mul(h).and_then(|n| n.checked_mul(4)) != Some(rgba.len()) {
        return Err("the page could not be read as a picture".into());
    }
    if !(region.iter().all(|v| v.is_finite()) && cell.is_finite() && cell > 0.0 && px > 0) {
        return Err("the area is out of range".into());
    }
    // Whole raster pixels, inside the raster: the casts are of clamped, finite values.
    let x0 = region[0].min(region[2]).floor().clamp(0.0, w as f64) as usize;
    let x1 = region[0].max(region[2]).ceil().clamp(0.0, w as f64) as usize;
    let y0 = region[1].min(region[3]).floor().clamp(0.0, h as f64) as usize;
    let y1 = region[1].max(region[3]).ceil().clamp(0.0, h as f64) as usize;
    let (rw, rh) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    if rw == 0 || rh == 0 {
        return Err("the area is not on the page".into());
    }
    // Equal squares that fill the area exactly: as many as fit at about `cell` pixels each.
    let cols = ((rw as f64 / cell).round() as usize).clamp(1, rw);
    let rows = ((rh as f64 / cell).round() as usize).clamp(1, rh);
    let (ow, oh) = (cols.saturating_mul(px), rows.saturating_mul(px));
    if (ow as f64) * (oh as f64) > MAX_PIXELS {
        return Err("the area is too large for squares this small: choose larger squares".into());
    }
    let mut colour = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        let (ya, yb) = (y0 + r * rh / rows, y0 + (r + 1) * rh / rows);
        for c in 0..cols {
            let (xa, xb) = (x0 + c * rw / cols, x0 + (c + 1) * rw / cols);
            let mut sum = [0u64; 3];
            let mut n = 0u64;
            for y in ya..yb {
                for x in xa..xb {
                    let i = (y * w + x) * 4;
                    if let Some(p) = rgba.get(i..i + 4) {
                        // Premultiplied over white: colour + (1 - alpha) × white.
                        let blank = 255 - u64::from(p[3]);
                        for (s, &v) in sum.iter_mut().zip(&p[..3]) {
                            *s += (u64::from(v) + blank).min(255);
                        }
                        n += 1;
                    }
                }
            }
            colour.push(sum.map(|s| (s / n.max(1)) as u8));
        }
    }
    let mut out = Vec::with_capacity(ow * oh * 3);
    for y in 0..oh {
        for x in 0..ow {
            out.extend(colour.get((y / px) * cols + x / px).copied().unwrap_or([255; 3]));
        }
    }
    // Both are at most MAX_PIXELS, so they fit in a u32.
    Ok((ow as u32, oh as u32, out))
}

/// A squares picture placed by [`Edit::Mosaic`]: a PNG over `display` (display space: points,
/// y up) on page `page` (0-based).
#[derive(Clone, Debug, PartialEq)]
pub struct Squares {
    pub page: usize,
    pub display: [f64; 4],
    pub picture: Arc<Vec<u8>>,
}

fn check_cell(cell: f64) -> Result<(), EditError> {
    if cell.is_finite() && (MIN_CELL..=MAX_CELL).contains(&cell) {
        Ok(())
    } else {
        Err(EditError::Invalid(format!("the squares must be {MIN_CELL} to {MAX_CELL} points")))
    }
}

/// An area in view space (points from the top-left, any corner order) brought onto a page of
/// `pw` × `ph` points, or `None` when less than [`MIN_SIDE`] of it is on the page.
fn on_page(area: [f64; 4], pw: f64, ph: f64) -> Option<[f64; 4]> {
    if !area.iter().all(|v| v.is_finite()) {
        return None;
    }
    let x0 = area[0].min(area[2]).clamp(0.0, pw);
    let x1 = area[0].max(area[2]).clamp(0.0, pw);
    let y0 = area[1].min(area[3]).clamp(0.0, ph);
    let y1 = area[1].max(area[3]).clamp(0.0, ph);
    (x1 - x0 >= MIN_SIDE && y1 - y0 >= MIN_SIDE).then_some([x0, y0, x1, y1])
}

impl Session {
    /// The squares pictures of `areas` (view space, already on the page) of page `page`,
    /// rendering the page once.
    fn page_squares(&self, id: DocId, page: usize, areas: &[[f64; 4]], cell: f64) -> Result<Vec<Squares>, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let info = doc.info.pages.get(page).ok_or_else(|| EditError::Invalid(format!("there is no page {}", page.saturating_add(1))))?;
        let (pw, ph) = (f64::from(info.width), f64::from(info.height));
        // At least four raster pixels per square, at most 4× (288 dpi).
        let page_area = pw.max(1.0) * ph.max(1.0);
        let scale = (4.0 / cell).clamp(1.0, 4.0).min((MAX_PIXELS / page_area).sqrt()).max(0.01) as f32;
        let config = RenderConfig { password: doc.password.as_deref().map(Arc::from), ..Default::default() };
        let shot = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), config).render(RenderRequest { page, scale, ..Default::default() });
        if let Some(e) = shot.error {
            return Err(EditError::Invalid(format!("page {} could not be rendered: {e}", page + 1)));
        }
        let k = f64::from(shot.width) / pw.max(1e-3);
        areas
            .iter()
            .map(|&[x0, y0, x1, y1]| {
                let (w, h, rgb) = squares(&shot.rgba, shot.width, shot.height, [x0 * k, y0 * k, x1 * k, y1 * k], cell * k, PX_PER_CELL)
                    .map_err(EditError::Invalid)?;
                let mut picture = Vec::new();
                image::ImageEncoder::write_image(image::codecs::png::PngEncoder::new(&mut picture), &rgb, w, h, image::ExtendedColorType::Rgb8)
                    .map_err(|e| EditError::Invalid(format!("the squares could not be encoded: {e}")))?;
                // Display space is view space with y up.
                Ok(Squares { page, display: [x0, ph - y1, x1, ph - y0], picture: Arc::new(picture) })
            })
            .collect()
    }

    /// Edit a PDF ▸ Mosaic: the edit that covers `area` (view space: points from the top-left
    /// of the displayed page, any corner order) of page `page` (0-based) with squares of about
    /// `cell` points ([`MIN_CELL`]–[`MAX_CELL`]) in its own colours, after removing what it showed
    /// for good (see the module). Refused on a page with redaction marks waiting to be applied,
    /// which this would apply too.
    pub fn mosaic_area_edit(&self, id: DocId, page: usize, area: [f64; 4], cell: f64) -> Result<Edit, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        if let Some(why) = &doc.read_only_reason {
            return Err(EditError::ReadOnly(why.clone()));
        }
        let info = doc.info.pages.get(page).ok_or_else(|| EditError::Invalid(format!("there is no page {}", page.saturating_add(1))))?;
        check_cell(cell)?;
        let (pw, ph) = (f64::from(info.width), f64::from(info.height));
        let [x0, y0, x1, y1] =
            on_page(area, pw, ph).ok_or_else(|| EditError::Invalid("the area must be on the page and at least 1 point wide and tall".into()))?;
        if doc.info.annotations.iter().any(|a| a.page == page && a.subtype == "Redact") {
            return Err(EditError::Invalid(format!(
                "page {} has redaction marks waiting to be applied: apply or remove them first",
                page.saturating_add(1)
            )));
        }
        // The area's corners in user space (axis-aligned: pages turn by quarter turns).
        let corners = [info.view_to_user(x0 as f32, y0 as f32), info.view_to_user(x1 as f32, y1 as f32)];
        let user = [
            f64::from(corners[0][0].min(corners[1][0])),
            f64::from(corners[0][1].min(corners[1][1])),
            f64::from(corners[0][0].max(corners[1][0])),
            f64::from(corners[0][1].max(corners[1][1])),
        ];
        let squares = self.page_squares(id, page, &[[x0, y0, x1, y1]], cell)?;
        Ok(Edit::Mosaic { area: Some((page, user)), pages: vec![page], squares })
    }

    /// [`Session::mosaic_area_edit`], applied: one undoable step.
    pub fn mosaic_area(&mut self, id: DocId, page: usize, area: [f64; 4], cell: f64) -> Result<(), EditError> {
        let edit = self.mosaic_area_edit(id, page, area, cell)?;
        self.apply(id, edit)
    }

    /// Redact ▸ Apply as mosaic: the edit that applies the redaction marks (all, or those on
    /// `only` pages, 0-based), as Apply redactions does, and covers each marked area with squares
    /// of about `cell` points in its own colours instead of leaving the marks' boxes showing.
    pub fn mosaic_marks_edit(&self, id: DocId, only: Option<&[usize]>, cell: f64) -> Result<Edit, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        if let Some(why) = &doc.read_only_reason {
            return Err(EditError::ReadOnly(why.clone()));
        }
        check_cell(cell)?;
        let marks = pdfcraft_redact::marks(self.cos(id)?);
        let mut pages: Vec<usize> = marks.iter().map(|m| m.page).filter(|p| only.is_none_or(|o| o.contains(p))).collect();
        pages.sort_unstable();
        pages.dedup();
        if pages.is_empty() {
            return Err(EditError::Invalid("there are no redaction marks to apply".into()));
        }
        let mut squares = Vec::new();
        for &page in &pages {
            let Some(info) = doc.info.pages.get(page) else { continue };
            let (pw, ph) = (f64::from(info.width), f64::from(info.height));
            let areas: Vec<[f64; 4]> = marks
                .iter()
                .filter(|m| m.page == page)
                .flat_map(|m| m.rects.iter())
                .filter_map(|r| {
                    let [ax, ay] = info.user_to_view(r[0] as f32, r[1] as f32);
                    let [bx, by] = info.user_to_view(r[2] as f32, r[3] as f32);
                    on_page([f64::from(ax), f64::from(ay), f64::from(bx), f64::from(by)], pw, ph)
                })
                .collect();
            squares.extend(self.page_squares(id, page, &areas, cell)?);
        }
        Ok(Edit::Mosaic { area: None, pages, squares })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).flat_map(|(x, y)| f(x, y)).collect()
    }

    #[test]
    fn each_square_is_the_average_of_its_part_of_the_area() {
        // Left half black, right half white; two squares across, one down.
        let src = raster(40, 20, |x, _| if x < 20 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
        let (w, h, rgb) = squares(&src, 40, 20, [0.0, 0.0, 40.0, 20.0], 20.0, 4).unwrap();
        assert_eq!((w, h), (8, 4));
        let at = |x: u32, y: u32| &rgb[((y * w + x) * 3) as usize..][..3];
        assert_eq!((at(0, 0), at(3, 3)), (&[0, 0, 0][..], &[0, 0, 0][..]));
        assert_eq!((at(4, 0), at(7, 3)), (&[255, 255, 255][..], &[255, 255, 255][..]));
        // One square over both halves: grey, and no stroke of the original survives.
        let (_, _, one) = squares(&src, 40, 20, [0.0, 0.0, 40.0, 20.0], 40.0, 2).unwrap();
        assert!(one.iter().all(|&v| v == 127), "{one:?}");
    }

    #[test]
    fn transparency_is_white_and_the_area_is_kept_on_the_raster() {
        let src = vec![0u8; 10 * 10 * 4];
        let (w, h, rgb) = squares(&src, 10, 10, [-5.0, -5.0, 50.0, 50.0], 5.0, 1).unwrap();
        assert_eq!((w, h), (2, 2), "clamped to the 10 × 10 raster");
        assert!(rgb.iter().all(|&v| v == 255));
    }

    #[test]
    fn bad_rasters_and_areas_are_errors_not_panics() {
        let src = vec![255u8; 10 * 10 * 4];
        assert!(squares(&src, 11, 10, [0.0, 0.0, 5.0, 5.0], 2.0, 1).is_err(), "size doesn't match");
        assert!(squares(&[], 0, 0, [0.0, 0.0, 5.0, 5.0], 2.0, 1).is_err());
        assert!(squares(&src, 10, 10, [f64::NAN, 0.0, 5.0, 5.0], 2.0, 1).is_err());
        assert!(squares(&src, 10, 10, [0.0, 0.0, 5.0, 5.0], 0.0, 1).is_err());
        assert!(squares(&src, 10, 10, [20.0, 20.0, 30.0, 30.0], 2.0, 1).is_err(), "off the raster");
        assert!(squares(&src, 10, 10, [0.0, 0.0, 10.0, 10.0], 2.0, 0).is_err());
        // Squares far smaller than a pixel are one pixel each, so the picture stays small.
        let (w, h, _) = squares(&src, 10, 10, [0.0, 0.0, 10.0, 10.0], 1e-9, 1).unwrap();
        assert_eq!((w, h), (10, 10));
        // A picture larger than the cap is refused, not allocated.
        let big = vec![255u8; 100 * 100 * 4];
        assert!(squares(&big, 100, 100, [0.0, 0.0, 100.0, 100.0], 1.0, 50).is_err());
    }
}
