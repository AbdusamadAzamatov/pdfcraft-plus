//! Scan & OCR ▸ Enhance ▸ Camera image: straighten a photographed document. Four corners on the
//! page (PDF user space) are warped to a rectangle, and the page's content becomes that picture
//! (one undoable step, through Replace Pages, so the page's comments, links and bookmarks stay).

use std::sync::Arc;

use pdfcraft_geom::perspective::{Homography, Point, order_quad};
use pdfcraft_render::{RenderConfig, RenderRequest};

use crate::{DocId, Edit, EditError, ImageResolution, Session};

/// The densest the page is read at: 600 dpi, as for OCR.
const MAX_SCALE: f32 = 600.0 / 72.0;
/// The resolution a page without a picture is read at.
const DEFAULT_SCALE: f32 = 300.0 / 72.0;
/// The most pixels the page is rendered with, and the straightened picture has.
const MAX_PIXELS: f64 = 40_000_000.0;
/// JPEG quality of the straightened picture.
const QUALITY: u8 = 90;

/// A straightened picture: RGB8, row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Warped {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// Warp the quadrilateral `quad` (raster pixels, any corner order) of a premultiplied RGBA
/// raster to an upright rectangle as large as the quadrilateral's longer sides, composited over
/// white. Errors say what was wrong with the corners or the raster.
pub fn warp(rgba: &[u8], width: u32, height: u32, quad: [Point; 4]) -> Result<Warped, String> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || w.checked_mul(h).and_then(|n| n.checked_mul(4)) != Some(rgba.len()) {
        return Err("the page could not be read as a picture".into());
    }
    let [tl, tr, br, bl] = order_quad(quad).ok_or("the four corners must enclose an area (a convex quadrilateral)")?;
    let dist = |a: Point, b: Point| (a[0] - b[0]).hypot(a[1] - b[1]);
    let mut ow = dist(tl, tr).max(dist(bl, br));
    let mut oh = dist(tl, bl).max(dist(tr, br));
    let k = (MAX_PIXELS / (ow * oh).max(1.0)).sqrt().min(1.0);
    ow = (ow * k).floor().max(1.0);
    oh = (oh * k).floor().max(1.0);
    if !(ow.is_finite() && oh.is_finite()) {
        return Err("the corners are out of range".into());
    }
    // Both are at least 1 and their product is at most MAX_PIXELS, so the casts are exact.
    let (ow, oh) = (ow as u32, oh as u32);
    let rect = [[0.0, 0.0], [ow as f64, 0.0], [ow as f64, oh as f64], [0.0, oh as f64]];
    let to_src = Homography::from_points(rect, [tl, tr, br, bl]).ok_or("the four corners must not be in a line")?;

    let px = |x: usize, y: usize| -> [f64; 4] {
        let i = (y * w + x) * 4;
        match rgba.get(i..i + 4) {
            Some(p) => [p[0] as f64, p[1] as f64, p[2] as f64, p[3] as f64],
            None => [0.0; 4],
        }
    };
    let mut rgb = Vec::with_capacity(ow as usize * oh as usize * 3);
    for y in 0..oh {
        for x in 0..ow {
            // Sample at the pixel's centre; outside the page is white.
            let mut c = [0.0f64; 4];
            if let Some([sx, sy]) = to_src.apply([x as f64 + 0.5, y as f64 + 0.5]) {
                let (fx, fy) = (sx - 0.5, sy - 0.5);
                if fx > -1.0 && fy > -1.0 && fx < w as f64 && fy < h as f64 {
                    let (x0, y0) = (fx.floor(), fy.floor());
                    let (ax, ay) = (fx - x0, fy - y0);
                    // Clamped to the raster, so every index below is in range.
                    let clamp = |v: f64, n: usize| v.clamp(0.0, (n - 1) as f64) as usize;
                    let (xa, xb) = (clamp(x0, w), clamp(x0 + 1.0, w));
                    let (ya, yb) = (clamp(y0, h), clamp(y0 + 1.0, h));
                    let (p00, p10, p01, p11) = (px(xa, ya), px(xb, ya), px(xa, yb), px(xb, yb));
                    for (i, v) in c.iter_mut().enumerate() {
                        let top = p00[i] * (1.0 - ax) + p10[i] * ax;
                        let bottom = p01[i] * (1.0 - ax) + p11[i] * ax;
                        *v = top * (1.0 - ay) + bottom * ay;
                    }
                }
            }
            // Premultiplied over white: colour + (1 - alpha) × white.
            for v in &c[..3] {
                rgb.push((v + 255.0 - c[3]).round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    Ok(Warped { width: ow, height: oh, rgb })
}

/// What is repaired on a straightened photo before its look is given: both off by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Repairs {
    /// Remove moiré, the pattern in photos of screens and printed pictures
    /// ([`crate::descreen`]).
    pub moire: bool,
    /// Straighten lines of text that bend, as on a book page ([`crate::dewarp`]); a photo
    /// without clear lines of text is left as it is.
    pub flatten: bool,
}

/// The long side, in pixels, a page is rendered at to look for the document's edges.
const DETECT_SIDE: f32 = 1200.0;

impl Session {
    /// Enhance ▸ Camera image: the corners of the photographed document on page `page`
    /// (0-based) of document `id`, in PDF user space (top-left, top-right, bottom-right,
    /// bottom-left as displayed), or `None` when its edges can't be found with confidence.
    pub fn detect_camera_corners(&self, id: DocId, page: usize) -> Option<[Point; 4]> {
        let doc = self.get(id)?;
        let info = doc.info.pages.get(page)?;
        let scale = (DETECT_SIDE / info.width.max(info.height).max(1.0)).clamp(0.05, MAX_SCALE);
        let config = RenderConfig { password: doc.password.as_deref().map(Arc::from), ..Default::default() };
        let shot = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), config).render(RenderRequest { page, scale, ..Default::default() });
        if shot.error.is_some() {
            return None;
        }
        let quad = crate::camera_edges::detect_document(&shot.rgba, shot.width, shot.height)?;
        let k = shot.width as f64 / (info.width as f64).max(1e-3);
        Some(quad.map(|[x, y]| {
            let [u, v] = info.view_to_user((x / k) as f32, (y / k) as f32);
            [u as f64, v as f64]
        }))
    }

    /// Enhance ▸ Camera image: replace page `page` (0-based) of document `id` with the part of it
    /// inside `corners` (PDF user space, any order), straightened to a rectangle. The page is
    /// read at its picture's own resolution (at most 600 dpi) and keeps that physical size.
    pub fn enhance_camera_page(&mut self, id: DocId, page: usize, corners: [Point; 4]) -> Result<(), EditError> {
        self.enhance_camera_page_at(id, page, corners, crate::camera_clean::Look::Original, 0)
    }

    /// [`Session::enhance_camera_page`], then give the straightened photo a `look` at `level`
    /// (0 = as photographed … 100 = white paper, strong contrast, no specks; for black and white
    /// and eco the threshold; see [`crate::camera_clean`]). Still one undoable step.
    pub fn enhance_camera_page_at(
        &mut self,
        id: DocId,
        page: usize,
        corners: [Point; 4],
        look: crate::camera_clean::Look,
        level: u8,
    ) -> Result<(), EditError> {
        self.enhance_camera_page_with(id, page, corners, look, level, Repairs::default())
    }

    /// [`Session::enhance_camera_page_at`], with `repairs` made to the straightened photo before
    /// its look (moiré removal first, then flattening). Still one undoable step.
    pub fn enhance_camera_page_with(
        &mut self,
        id: DocId,
        page: usize,
        corners: [Point; 4],
        look: crate::camera_clean::Look,
        level: u8,
        repairs: Repairs,
    ) -> Result<(), EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        if let Some(why) = &doc.read_only_reason {
            return Err(EditError::ReadOnly(why.clone()));
        }
        let info = doc.info.pages.get(page).ok_or_else(|| EditError::Invalid(format!("there is no page {}", page.saturating_add(1))))?;
        let native = doc
            .page_images(page)
            .iter()
            .filter(|i| i.width > 1 && i.height > 1)
            .map(|i| {
                let m = i.matrix;
                let (w, h) = (m[0].hypot(m[1]), m[2].hypot(m[3]));
                (i.width as f64 / w.max(1e-6)).max(i.height as f64 / h.max(1e-6)) as f32
            })
            .filter(|s| s.is_finite())
            .reduce(f32::max);
        let area = (info.width.max(1.0) as f64) * (info.height.max(1.0) as f64);
        let cap = (MAX_PIXELS / area).sqrt() as f32;
        let scale = native.unwrap_or(DEFAULT_SCALE).clamp(1.0, MAX_SCALE).min(cap).max(0.01);

        let config = RenderConfig { password: doc.password.as_deref().map(Arc::from), ..Default::default() };
        let mut r = pdfcraft_render::PageRenderer::new(doc.bytes.clone(), config);
        let shot = r.render(RenderRequest { page, scale, ..Default::default() });
        if let Some(e) = shot.error {
            return Err(EditError::Invalid(format!("page {} could not be rendered: {e}", page + 1)));
        }
        let k = shot.width as f64 / (info.width as f64).max(1e-3);
        let quad = corners.map(|[x, y]| {
            let [vx, vy] = info.user_to_view(x as f32, y as f32);
            [vx as f64 * k, vy as f64 * k]
        });
        let mut out = warp(&shot.rgba, shot.width, shot.height, quad).map_err(EditError::Invalid)?;
        trim_background_edges(&mut out.rgb, out.width, out.height);
        if repairs.moire {
            out.rgb = crate::descreen::descreen(&out.rgb, out.width, out.height)
                .ok_or_else(|| EditError::Invalid("the straightened page could not be descreened".into()))?;
        }
        if repairs.flatten
            && let Some(flat) = crate::dewarp::flatten(&out.rgb, out.width, out.height)
        {
            out.rgb = flat;
        }
        let pixels = crate::camera_clean::apply(&out.rgb, out.width, out.height, look, level)
            .ok_or_else(|| EditError::Invalid("the straightened page could not be cleaned up".into()))?;
        // Black and white (and eco) is lossless (PNG): JPEG would smear grey around every letter.
        let mut encoded = Vec::new();
        let (name, written) = match &pixels {
            crate::camera_clean::Pixels::Rgb(rgb) => (
                "camera.jpg",
                image::ImageEncoder::write_image(
                    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, QUALITY),
                    rgb,
                    out.width,
                    out.height,
                    image::ExtendedColorType::Rgb8,
                ),
            ),
            crate::camera_clean::Pixels::Grey(grey) if look.is_bilevel() => (
                "camera.png",
                image::ImageEncoder::write_image(
                    image::codecs::png::PngEncoder::new(&mut encoded),
                    grey,
                    out.width,
                    out.height,
                    image::ExtendedColorType::L8,
                ),
            ),
            crate::camera_clean::Pixels::Grey(grey) => (
                "camera.jpg",
                image::ImageEncoder::write_image(
                    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, QUALITY),
                    grey,
                    out.width,
                    out.height,
                    image::ExtendedColorType::L8,
                ),
            ),
        };
        written.map_err(|e| EditError::Invalid(format!("the straightened page could not be encoded: {e}")))?;
        // One device pixel per `k` points keeps the page's physical size.
        let dpi = (k * 72.0).clamp(1.0, 1200.0);
        let bytes = self.create_from_images_with_resolution(&[(name.into(), encoded)], ImageResolution::Dpi(dpi))?;
        let replace = Edit::ReplacePages { pages: vec![page], name: "camera.pdf".into(), bytes, src_pages: vec![0] };
        self.apply(id, Edit::Batch { label: "Enhance camera image".into(), edits: vec![replace] })
    }
}

/// Replace the lines along each edge of a straightened page that still show what the paper lay
/// on (corners found on, or set a little outside, the paper's edge sample the table too) with
/// the first clean line inside them. Clean-up would otherwise stretch that sliver into a dark,
/// dashed frame. The outermost 1/400 of the shorter side (2 to 6 lines) always goes: a corner on
/// a tilted edge leaves a wedge of table, mixed with paper, too faint to measure but not to
/// stretch. Further in, lines go while they are not paper, up to 1/150 (4 to 16 lines).
pub(crate) fn trim_background_edges(rgb: &mut [u8], width: u32, height: u32) {
    let (w, h) = (width as usize, height as usize);
    if w < 8 || h < 8 || w.checked_mul(h).and_then(|n| n.checked_mul(3)) != Some(rgb.len()) {
        return;
    }
    let lum = |p: &[u8]| {
        (p.first().copied().unwrap_or(0) as u32 * 299 + p.get(1).copied().unwrap_or(0) as u32 * 587 + p.get(2).copied().unwrap_or(0) as u32 * 114)
            / 1000
    };
    // The paper: the 90th percentile of the page's brightness. Darker than 85 % of it is not paper.
    let mut hist = [0usize; 256];
    for p in rgb.chunks_exact(3) {
        if let Some(slot) = hist.get_mut(lum(p) as usize) {
            *slot += 1;
        }
    }
    let want = w * h * 9 / 10;
    let mut seen = 0;
    let paper = hist.iter().position(|&n| {
        seen += n;
        seen > want
    });
    let dark = paper.unwrap_or(255) as u32 * 85 / 100;
    let always = (w.min(h) / 400).clamp(2, 6);
    let depth = (w.min(h) / 150).clamp(4, 16).max(always + 2);
    // Pixel index of the `i`-th pixel along line `line` (0 at the edge) of `side`.
    let at = |side: u8, line: usize, i: usize| -> usize {
        match side {
            0 => line * w + i,           // top: rows
            1 => (h - 1 - line) * w + i, // bottom
            2 => i * w + line,           // left: columns
            _ => i * w + (w - 1 - line), // right
        }
    };
    for side in 0..4u8 {
        let len = if side < 2 { w } else { h };
        let dirty = |rgb: &[u8], line: usize| {
            let n = (0..len).filter(|&i| rgb.get(at(side, line, i) * 3..at(side, line, i) * 3 + 3).is_some_and(|p| lum(p) < dark)).count();
            // More than 1 % of the line: a letter's stroke crossing it is less.
            n * 100 > len
        };
        let measured = (always..depth).take_while(|&line| dirty(rgb, line)).count();
        // Not paper all the way in: content (a bar, a frame) reaches the edge, not a sliver of
        // table, so only the outermost lines go.
        let bad = if always + measured >= depth { always } else { always + measured };
        for line in 0..bad {
            for i in 0..len {
                let (from, to) = (at(side, bad, i) * 3, at(side, line, i) * 3);
                if let Some(px) = rgb.get(from..from + 3).map(|p| [p[0], p[1], p[2]])
                    && let Some(dst) = rgb.get_mut(to..to + 3)
                {
                    dst.copy_from_slice(&px);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An opaque premultiplied raster filled by `f(x, y) -> [r, g, b]`.
    fn raster(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let [r, g, b] = f(x, y);
                [r, g, b, 255]
            })
            .collect()
    }

    #[test]
    fn a_tilted_quad_comes_out_upright() {
        // A dark quadrilateral on white; warping exactly its corners leaves a dark picture.
        let quad = [[20.0, 30.0], [170.0, 10.0], [180.0, 150.0], [10.0, 160.0]];
        let inside = |x: f64, y: f64| {
            let q = order_quad(quad).unwrap();
            (0..4).all(|i| {
                let (a, b) = (q[i], q[(i + 1) % 4]);
                (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]) >= 0.0
            })
        };
        let src = raster(200, 180, |x, y| if inside(x as f64 + 0.5, y as f64 + 0.5) { [20, 20, 20] } else { [255, 255, 255] });
        let out = warp(&src, 200, 180, quad).unwrap();
        // The longer of each pair of opposite sides: 170 × 140.
        assert_eq!((out.width, out.height), (170, 140));
        // Away from the anti-aliased edges, everything is the dark fill.
        let at = |x: u32, y: u32| out.rgb[((y * out.width + x) * 3) as usize];
        for (x, y) in [(5, 5), (85, 70), (164, 134), (5, 134), (164, 5)] {
            assert!(at(x, y) < 60, "({x}, {y}) = {}", at(x, y));
        }
    }

    #[test]
    fn the_corner_order_does_not_matter() {
        let src = raster(64, 48, |x, y| [(x * 4) as u8, (y * 5) as u8, 128]);
        let q = [[4.0, 3.0], [60.0, 6.0], [58.0, 44.0], [2.0, 40.0]];
        let a = warp(&src, 64, 48, q).unwrap();
        let b = warp(&src, 64, 48, [q[2], q[0], q[3], q[1]]).unwrap();
        assert_eq!(a, b);
        // Left to right the red channel grows, top to bottom the green one does.
        let px = |x: u32, y: u32| &a.rgb[((y * a.width + x) * 3) as usize..][..3];
        assert!(px(a.width - 3, 10)[0] > px(2, 10)[0] + 100);
        assert!(px(10, a.height - 3)[1] > px(10, 2)[1] + 100);
    }

    #[test]
    fn a_sliver_of_table_along_the_edges_becomes_paper() {
        // Paper with a line of text, and a dashed 2-pixel band of dark table along every edge.
        let (w, h) = (300u32, 400u32);
        let mut rgb: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let edge = x < 2 || y < 2 || x >= w - 2 || y >= h - 2;
                let text = (40..48).contains(&y) && (30..270).contains(&x) && x % 6 < 3;
                if (edge && (x + y) % 4 < 2) || text { [40, 35, 30] } else { [235, 230, 220] }
            })
            .collect();
        let before = rgb.clone();
        trim_background_edges(&mut rgb, w, h);
        let px = |rgb: &[u8], x: u32, y: u32| rgb[((y * w + x) * 3) as usize];
        for (x, y) in [(0, 0), (1, 150), (150, 0), (150, 1), (299, 200), (298, 200), (150, 399), (299, 399)] {
            assert_eq!(px(&rgb, x, y), 235, "({x}, {y}) is paper again");
        }
        // The text and the rest of the page are untouched.
        for y in 2..h - 2 {
            for x in 2..w - 2 {
                assert_eq!(px(&rgb, x, y), px(&before, x, y), "({x}, {y})");
            }
        }
    }

    #[test]
    fn a_faint_wedge_of_table_on_a_tilted_edge_goes_too() {
        // Grey, part table and part paper, 0 pixels wide at the top of the left edge and 4 at the
        // bottom: too faint to call dark, but clean-up would stretch it into a black wedge.
        let (w, h) = (600u32, 800u32);
        let mut rgb: Vec<u8> = (0..w * h).flat_map(|i| if i % w < (i / w) * 4 / h { [170, 165, 160] } else { [235, 230, 220] }).collect();
        trim_background_edges(&mut rgb, w, h);
        for y in [0, 400, 600, 799] {
            for x in 0..4 {
                assert_eq!(rgb[((y * w + x) * 3) as usize], 235, "({x}, {y})");
            }
        }
    }

    #[test]
    fn content_reaching_the_edge_is_left_alone() {
        // A clean page, and one with a dark bar across the top as deep as the search: unchanged.
        let (w, h) = (300u32, 400u32);
        let clean: Vec<u8> = vec![240; (w * h * 3) as usize];
        let mut out = clean.clone();
        trim_background_edges(&mut out, w, h);
        assert_eq!(out, clean);
        let bar: Vec<u8> = (0..w * h).flat_map(|i| if i / w < 30 { [20, 40, 120] } else { [240, 240, 240] }).collect();
        let mut out = bar.clone();
        trim_background_edges(&mut out, w, h);
        assert_eq!(out, bar);
        // Too small, or the wrong length: nothing happens and nothing panics.
        trim_background_edges(&mut [0u8; 12], 2, 2);
        trim_background_edges(&mut [0u8; 10], 300, 400);
    }

    #[test]
    fn transparency_becomes_white() {
        let src = vec![0u8; 8 * 8 * 4];
        let out = warp(&src, 8, 8, [[0.0, 0.0], [8.0, 0.0], [8.0, 8.0], [0.0, 8.0]]).unwrap();
        assert!(out.rgb.iter().all(|&v| v == 255));
    }

    #[test]
    fn bad_corners_and_rasters_are_errors_not_panics() {
        let src = raster(10, 10, |_, _| [0, 0, 0]);
        let line = [[0.0, 0.0], [5.0, 5.0], [10.0, 10.0], [2.0, 2.0]];
        assert!(warp(&src, 10, 10, line).is_err());
        assert!(warp(&src, 10, 10, [[f64::NAN, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]).is_err());
        let ok = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        assert!(warp(&src, 11, 10, ok).is_err(), "size doesn't match the buffer");
        assert!(warp(&[], 0, 0, ok).is_err());
        // Corners far outside the raster: a capped, white picture, not a huge allocation.
        let far = [[-1e9, -1e9], [1e9, -1e9], [1e9, 1e9], [-1e9, 1e9]];
        let out = warp(&src, 10, 10, far).unwrap();
        assert!((out.width as f64) * (out.height as f64) <= MAX_PIXELS);
    }
}
