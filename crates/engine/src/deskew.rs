//! Organize ▸ Layout (and Scan & OCR) ▸ Straighten scans (execution plan PL9): find how far each
//! scanned page is tilted, then turn it straight.
//!
//! **Detection** ([`detect_skew`]) is pure and works on a grey raster of the page as shown:
//! 1. the raster is split into ink and paper at Otsu's threshold (a page without two clearly
//!    different tones has no text to go by);
//! 2. a **projection-profile search** shears the ink pixels by each candidate angle and scores the
//!    variance of the row sums: text lines pile up into a few full rows exactly when the shear
//!    matches their slope. Angles −15…15° are tried at 1°, then ±1° around the best at 0.05°, and
//!    the best score is refined between its neighbours;
//! 3. the **confidence** is how much the best angle stands out from a typical one (1 − median
//!    score ÷ best score): near 1 for lines of text, near 0 for a photo or a few scattered marks.
//!
//! **Straightening** ([`Edit::straighten`]) turns each page with PL2's rotation in "keep page
//! size, crop" mode: the content (scan image and any OCR text layer alike) is wrapped in a
//! transform, so the image is not resampled and nothing is lost. Pages with comments or form
//! fields are not straightened: their appearances can't turn inside an axis-aligned `/Rect`, so
//! they are reported instead (flatten them first, or use Rotate by angle).
//!
//! Rendering every page takes a while, so finding the tilt is a job like Remove blank pages':
//! [`Session::deskew_job`] captures what it needs and [`DeskewJob::run`] works anywhere (the UI runs
//! it on a worker thread), reporting progress and stopping when asked.

use std::sync::Arc;

use pdfcraft_render::{PRINT_TAG, PageRenderer, RenderConfig, RenderRequest};

use crate::{DocId, Edit, EditError, RotateFit, Session, guard};

/// The resolution pages are drawn at to find their tilt.
pub const DPI: f32 = 100.0;
/// The largest tilt looked for, in degrees either way.
pub const MAX_ANGLE: f64 = 15.0;
/// The default smallest tilt worth correcting, in degrees.
pub const DEFAULT_MIN_ANGLE: f64 = 0.2;
/// Below this confidence a page is not straightened: what it shows is not lines of text.
pub const MIN_CONFIDENCE: f64 = 0.35;
/// The coarse search step, the half-width of the fine search around the coarse best, and the fine
/// step, in degrees.
const COARSE_STEP: f64 = 1.0;
const FINE_SPAN: f64 = 1.0;
const FINE_STEP: f64 = 0.05;
/// A page with fewer ink pixels than this (at [`DPI`]), or a smaller share of ink, has too little
/// text to go by; one with a larger share of ink is a picture, not text on paper.
const MIN_INK_PIXELS: usize = 1500;
const MIN_INK_SHARE: f64 = 0.001;
const MAX_INK_SHARE: f64 = 0.35;
/// Ink and paper must differ by at least this much grey (0–255) on average.
const MIN_CONTRAST: f64 = 64.0;
/// The most raster pixels read: a larger raster is sampled on a coarser grid.
const MAX_PIXELS: usize = 16 << 20;
/// The most ink pixels scored per angle: more are thinned out evenly.
const MAX_POINTS: usize = 400_000;
/// A page is a scan when one image covers at least this share of it.
const SCAN_COVERAGE: f64 = 0.8;

/// A page's tilt, as [`detect_skew`] finds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Skew {
    /// Degrees, positive when the lines rise to the right (tilted counterclockwise as shown):
    /// turning the page this much clockwise straightens it.
    pub angle: f64,
    /// 0 to 1: how clearly the lines line up at `angle` compared with a typical angle.
    pub confidence: f64,
}

/// The grey level (0–255) of each pixel of a premultiplied RGBA8 raster composed over white
/// paper. A trailing partial pixel is ignored.
pub fn grey_over_white(rgba: &[u8]) -> Vec<u8> {
    // `luma_over_white` is at most 255, so the narrowing is exact.
    rgba.as_chunks::<4>().0.iter().map(|px| crate::blank::luma_over_white(px).min(255) as u8).collect()
}

/// Otsu's threshold of a grey histogram: the level that best separates dark from light, with the
/// two classes' mean levels. `None` for an empty histogram or one with a single tone.
pub(crate) fn otsu(hist: &[u64; 256]) -> Option<(u8, f64, f64)> {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return None;
    }
    let sum: f64 = hist.iter().enumerate().map(|(v, n)| v as f64 * *n as f64).sum();
    let (mut below, mut below_sum) = (0u64, 0.0f64);
    let mut best: Option<(f64, u8, f64, f64)> = None;
    for (level, n) in hist.iter().enumerate() {
        below = below.saturating_add(*n);
        below_sum += level as f64 * *n as f64;
        let above = total - below;
        if below == 0 || above == 0 {
            continue;
        }
        let (dark, light) = (below_sum / below as f64, (sum - below_sum) / above as f64);
        let between = below as f64 * above as f64 * (light - dark) * (light - dark);
        if best.is_none_or(|b| between > b.0) {
            // `level` indexes a 256-entry array, so it fits in a u8.
            best = Some((between, level as u8, dark, light));
        }
    }
    best.map(|(_, t, dark, light)| (t, dark, light))
}

/// The variance of the row sums of `points` (x from the centre, y down) sheared by `angle`
/// degrees: row = y + x·tan(angle), in `bins` rows starting `offset` above row 0.
fn score(points: &[(f32, f32)], angle: f64, offset: f64, counts: &mut [u32]) -> f64 {
    counts.fill(0);
    let t = angle.to_radians().tan();
    let bins = counts.len();
    for &(x, y) in points {
        let row = (f64::from(y) + f64::from(x) * t + offset).floor();
        if row >= 0.0 && row < bins as f64 {
            // In range, checked just above.
            if let Some(c) = counts.get_mut(row as usize) {
                *c = c.saturating_add(1);
            }
        }
    }
    let n = bins as f64;
    let (sum, squares) = counts.iter().fold((0.0, 0.0), |(s, q), &c| {
        let c = f64::from(c);
        (s + c, q + c * c)
    });
    let mean = sum / n;
    squares / n - mean * mean
}

/// The angles from `from` to `to` (inclusive) at `step` degrees.
fn angles(from: f64, to: f64, step: f64) -> Vec<f64> {
    let count = ((to - from) / step).round().max(0.0) as usize;
    (0..=count.min(10_000)).map(|i| from + i as f64 * step).filter(|a| *a <= to + 1e-9).collect()
}

/// How far the text on a grey raster (`width` × `height`, row-major, 0 black to 255 white) is
/// tilted, within ±[`MAX_ANGLE`] degrees. `None` when there is nothing to go by: a blank or flat
/// page, too little ink, a page that is mostly ink (a photo), or a buffer smaller than the size
/// says. A low [`Skew::confidence`] means the ink doesn't form lines.
pub fn detect_skew(grey: &[u8], width: usize, height: usize) -> Option<Skew> {
    let total = width.checked_mul(height)?;
    if total == 0 || grey.len() < total {
        return None;
    }
    // A huge raster is read on a coarser grid, so the work stays bounded.
    let stride = if total > MAX_PIXELS { ((total as f64 / MAX_PIXELS as f64).sqrt().ceil() as usize).max(1) } else { 1 };
    let sampled = || {
        (0..height).step_by(stride).flat_map(move |y| (0..width).step_by(stride).map(move |x| (x, y))).filter_map(|(x, y)| {
            let i = y.checked_mul(width)?.checked_add(x)?;
            grey.get(i).map(|v| (x, y, *v))
        })
    };
    let mut hist = [0u64; 256];
    let mut seen = 0usize;
    for (_, _, v) in sampled() {
        if let Some(n) = hist.get_mut(usize::from(v)) {
            *n += 1;
        }
        seen += 1;
    }
    let (threshold, dark, light) = otsu(&hist)?;
    if light - dark < MIN_CONTRAST {
        return None;
    }
    let ink = hist.iter().take(usize::from(threshold) + 1).sum::<u64>() as usize;
    let share = ink as f64 / seen.max(1) as f64;
    if ink < MIN_INK_PIXELS / stride.saturating_mul(stride) || !(MIN_INK_SHARE..=MAX_INK_SHARE).contains(&share) {
        return None;
    }
    let keep = ink.div_ceil(MAX_POINTS).max(1);
    // Points are placed on the sampled grid (one unit per `stride` pixels), so a coarser grid
    // leaves no empty rows between sampled ones to bias the score towards 0°.
    let unit = stride as f64;
    let centre = width as f64 / unit / 2.0;
    let points: Vec<(f32, f32)> = sampled()
        .filter(|(_, _, v)| *v <= threshold)
        .step_by(keep)
        .map(|(x, y, _)| ((x as f64 / unit - centre) as f32, (y as f64 / unit) as f32))
        .collect();
    let offset = (centre * MAX_ANGLE.to_radians().tan()).ceil() + 2.0;
    let bins = (height as f64 / unit + 2.0 * offset).ceil();
    if !bins.is_finite() || bins <= 0.0 || bins > (MAX_PIXELS as f64) {
        return None;
    }
    let mut counts = vec![0u32; bins as usize];

    let coarse: Vec<(f64, f64)> =
        angles(-MAX_ANGLE, MAX_ANGLE, COARSE_STEP).into_iter().map(|a| (a, score(&points, a, offset, &mut counts))).collect();
    let &(around, _) = coarse.iter().max_by(|a, b| a.1.total_cmp(&b.1))?;
    let fine: Vec<(f64, f64)> = angles((around - FINE_SPAN).max(-MAX_ANGLE), (around + FINE_SPAN).min(MAX_ANGLE), FINE_STEP)
        .into_iter()
        .map(|a| (a, score(&points, a, offset, &mut counts)))
        .collect();
    let (at, &(mut angle, best)) = fine.iter().enumerate().max_by(|a, b| a.1.1.total_cmp(&b.1.1))?;
    if !best.is_finite() || best <= 0.0 {
        return None;
    }
    // Between the neighbouring steps, where a parabola through the three scores peaks.
    if let (Some(prev), Some(next)) = (at.checked_sub(1).and_then(|i| fine.get(i)), fine.get(at + 1)) {
        let curve = prev.1 - 2.0 * best + next.1;
        if curve < 0.0 {
            let shift = (0.5 * (prev.1 - next.1) / curve).clamp(-0.5, 0.5);
            angle += shift * FINE_STEP;
        }
    }
    let mut typical: Vec<f64> = coarse.iter().map(|c| c.1).collect();
    typical.sort_by(f64::total_cmp);
    let median = typical.get(typical.len() / 2).copied().unwrap_or(best);
    let confidence = (1.0 - median / best).clamp(0.0, 1.0);
    Some(Skew { angle: angle.clamp(-MAX_ANGLE, MAX_ANGLE), confidence })
}

/// What to look for.
#[derive(Clone, Debug, PartialEq)]
pub struct DeskewOptions {
    /// Only pages that are scans: one image covering most of the page. Other pages are skipped
    /// without being drawn.
    pub only_scans: bool,
    /// The smallest tilt worth correcting, in degrees (0 to 15).
    pub min_angle: f64,
    /// The pages to check (0-based); empty checks every page.
    pub pages: Vec<usize>,
}

impl Default for DeskewOptions {
    fn default() -> Self {
        DeskewOptions { only_scans: true, min_angle: DEFAULT_MIN_ANGLE, pages: Vec::new() }
    }
}

/// Why a page is left as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeskewSkip {
    /// Not a scan (only scans were asked for).
    NotScan,
    /// No clear lines of text to go by: blank, a photo, or very little text.
    Unsure,
    /// Tilted by less than the smallest angle worth correcting.
    Straight,
    /// It has comments or form fields, which can't turn with it.
    Annotated,
    /// It couldn't be drawn.
    Unreadable,
}

impl DeskewSkip {
    /// The name the automation tool reports.
    pub fn name(self) -> &'static str {
        match self {
            DeskewSkip::NotScan => "not_scan",
            DeskewSkip::Unsure => "unsure",
            DeskewSkip::Straight => "straight",
            DeskewSkip::Annotated => "annotated",
            DeskewSkip::Unreadable => "unreadable",
        }
    }

    /// Why, in a sentence.
    pub fn reason(self) -> &'static str {
        match self {
            DeskewSkip::NotScan => "not a scan (one image covering the page)",
            DeskewSkip::Unsure => "no clear lines of text to measure the tilt by",
            DeskewSkip::Straight => "tilted by less than the smallest angle to correct",
            DeskewSkip::Annotated => "it has comments or form fields; flatten them first, or turn it with Rotate by angle",
            DeskewSkip::Unreadable => "the page couldn't be drawn",
        }
    }
}

/// What was found on one page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeskewPage {
    /// 0-based.
    pub page: usize,
    /// The tilt in degrees, positive counterclockwise as shown (turning the page this much
    /// clockwise straightens it), rounded to 0.01°; `None` when it couldn't be measured.
    pub angle: Option<f64>,
    /// 0 to 1 (see [`Skew::confidence`]); 0 when it couldn't be measured.
    pub confidence: f64,
    /// Why the page is left as it is; `None` when it will be straightened.
    pub skip: Option<DeskewSkip>,
}

impl DeskewPage {
    /// Whether Straighten turns this page.
    pub fn straightens(&self) -> bool {
        self.skip.is_none() && self.angle.is_some()
    }
}

/// The pages of `found` to straighten, with their angles, for [`Edit::straighten`].
pub fn to_straighten(found: &[DeskewPage]) -> Vec<(usize, f64)> {
    found.iter().filter(|p| p.straightens()).filter_map(|p| p.angle.map(|a| (p.page, a))).collect()
}

/// What one page is, from its objects.
#[derive(Clone, Copy, Debug, Default)]
struct Facts {
    scan: bool,
    annotated: bool,
}

/// Whether page `page` is a scan: it draws one image (not counting Form artwork or 1-pixel
/// images) and that image covers at least [`SCAN_COVERAGE`] of the crop box.
fn is_scan(cos: &pdfcraft_cos::Document, page: usize, crop: [f32; 4]) -> bool {
    let [cx0, cy0, cx1, cy1] = crop.map(f64::from);
    let area = (cx1 - cx0) * (cy1 - cy0);
    if !area.is_finite() || area <= 0.0 {
        return false;
    }
    let images = pdfcraft_edit::page_images(cos, page).unwrap_or_default();
    let mut pictures = images.iter().filter(|i| !i.is_form && i.width > 1 && i.height > 1);
    let (Some(only), None) = (pictures.next(), pictures.next()) else { return false };
    let r = only.visible_rect.unwrap_or(only.rect);
    let covered = (r[2].min(cx1) - r[0].max(cx0)).max(0.0) * (r[3].min(cy1) - r[1].max(cy0)).max(0.0);
    covered.is_finite() && covered >= SCAN_COVERAGE * area
}

/// Everything finding the tilt of pages needs from a document, detached from the session.
pub struct DeskewJob {
    cos: pdfcraft_cos::Document,
    bytes: Arc<Vec<u8>>,
    config: RenderConfig,
    /// Each page's displayed size in points, and its crop box in user space.
    sizes: Vec<(f32, f32)>,
    crops: Vec<[f32; 4]>,
    /// The pages to check (0-based, in order, each once).
    pages: Vec<usize>,
    only_scans: bool,
    min_angle: f64,
}

impl DeskewJob {
    /// How many pages are checked.
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// No pages are checked.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Check the pages, one [`DeskewPage`] each, in page order. `progress(done, total)` is called
    /// before each page and once at the end; returning `false` stops with
    /// [`EditError::Cancelled`]. A panic is returned as an error, so a worker thread always
    /// reports back.
    pub fn run(self, mut progress: impl FnMut(usize, usize) -> bool) -> Result<Vec<DeskewPage>, EditError> {
        guard(move || self.work(&mut progress)).map_err(|e| EditError::Invalid(format!("couldn't measure the pages: {e}")))?
    }

    fn facts(&self, page: usize) -> Facts {
        let scan = self.crops.get(page).is_some_and(|crop| is_scan(&self.cos, page, *crop));
        let annotated = pdfcraft_organize::page_has_drawn_annotations(&self.cos, page).unwrap_or(false);
        Facts { scan, annotated }
    }

    fn work(self, progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<Vec<DeskewPage>, EditError> {
        let mut renderer = PageRenderer::new(self.bytes.clone(), self.config.clone());
        let total = self.pages.len();
        let mut out = Vec::with_capacity(total);
        for (done, &page) in self.pages.iter().enumerate() {
            if !progress(done, total) {
                return Err(EditError::Cancelled);
            }
            let skipped = |skip| DeskewPage { page, angle: None, confidence: 0.0, skip: Some(skip) };
            let facts = self.facts(page);
            if self.only_scans && !facts.scan {
                out.push(skipped(DeskewSkip::NotScan));
                continue;
            }
            let Some(&(w, h)) = self.sizes.get(page) else {
                out.push(skipped(DeskewSkip::Unreadable));
                continue;
            };
            let scale = pdfcraft_render::effective_scale(w, h, DPI / 72.0);
            let r = renderer.render(RenderRequest { page, scale, tag: PRINT_TAG, ..Default::default() });
            if r.error.is_some() {
                out.push(skipped(DeskewSkip::Unreadable));
                continue;
            }
            let grey = grey_over_white(r.rgba.as_ref());
            let Some(skew) = detect_skew(&grey, r.width as usize, r.height as usize) else {
                out.push(skipped(DeskewSkip::Unsure));
                continue;
            };
            let angle = (skew.angle * 100.0).round() / 100.0;
            let skip = if skew.confidence < MIN_CONFIDENCE {
                Some(DeskewSkip::Unsure)
            } else if angle.abs() < self.min_angle || angle == 0.0 {
                Some(DeskewSkip::Straight)
            } else if facts.annotated {
                Some(DeskewSkip::Annotated)
            } else {
                None
            };
            out.push(DeskewPage { page, angle: Some(angle), confidence: skew.confidence, skip });
        }
        progress(total, total);
        Ok(out)
    }
}

impl Session {
    /// Capture what finding the tilt of the pages of document `id` needs (see
    /// [`DeskewJob::run`]). Refuses a smallest angle outside 0–15° and pages that don't exist.
    pub fn deskew_job(&self, id: DocId, opts: &DeskewOptions) -> Result<DeskewJob, EditError> {
        if !opts.min_angle.is_finite() || !(0.0..=MAX_ANGLE).contains(&opts.min_angle) {
            return Err(EditError::Invalid(format!(
                "the smallest angle to correct must be from 0 to {MAX_ANGLE} degrees (default {DEFAULT_MIN_ANGLE}), not {}",
                opts.min_angle
            )));
        }
        let cos = self.cos(id)?.clone();
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let src = doc.export_source();
        let count = src.pages;
        let mut pages: Vec<usize> = if opts.pages.is_empty() { (0..count).collect() } else { opts.pages.clone() };
        if let Some(&bad) = pages.iter().find(|&&p| p >= count) {
            return Err(EditError::Invalid(format!("page {} doesn't exist: the document has {count} pages", bad.saturating_add(1))));
        }
        pages.sort_unstable();
        pages.dedup();
        let mut config = src.config;
        // The page's own content: comments are not part of the scan.
        config.hide_comments = true;
        Ok(DeskewJob {
            cos,
            bytes: src.bytes,
            config,
            sizes: src.sizes,
            crops: doc.info.pages.iter().map(|p| p.crop).collect(),
            pages,
            only_scans: opts.only_scans,
            min_angle: opts.min_angle,
        })
    }

    /// Straighten scans: how far each page of document `id` is tilted, in page order.
    /// Read-only; straighten the pages with [`Edit::straighten`] of [`to_straighten`].
    pub fn find_skew(&self, id: DocId, opts: &DeskewOptions) -> Result<Vec<DeskewPage>, EditError> {
        self.deskew_job(id, opts)?.run(|_, _| true)
    }
}

impl Edit {
    /// Straighten scans: turn each listed page (0-based) clockwise by its angle in degrees, as
    /// shown, about its centre, keeping the page size and cutting off the turned corners
    /// ([`RotateFit::Crop`]), as one undo step. The content is wrapped, not resampled. Pages with
    /// comments or form fields are refused (see [`Edit::RotateContent`]).
    pub fn straighten(pages: Vec<(usize, f64)>) -> Edit {
        Edit::Batch {
            label: "Straighten pages".into(),
            edits: pages.into_iter().map(|(page, degrees)| Edit::RotateContent { pages: vec![page], degrees, fit: RotateFit::Crop }).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny deterministic random sequence (no dependency, the same on every run).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    /// A grey `w`×`h` page of "text" (lines of word blocks made of letter strokes, on white
    /// paper), turned `angle` degrees counterclockwise as shown about its centre.
    fn text_page(w: usize, h: usize, angle: f64) -> Vec<u8> {
        let mut upright = vec![255u8; w * h];
        let mut rng = Lcg(7);
        let (margin, line, x_height) = (70, 22, 11);
        let mut y = margin;
        while y + x_height < h - margin {
            let mut x = margin;
            let end = w - margin - rng.next(120) as usize;
            while x < end {
                let letters = 2 + rng.next(8) as usize;
                for _ in 0..letters {
                    let tall = rng.next(4) == 0;
                    let top = if tall { y.saturating_sub(4) } else { y };
                    for yy in top..y + x_height {
                        for xx in x..(x + 5).min(w) {
                            upright[yy * w + xx] = 20;
                        }
                    }
                    x += 7;
                }
                x += 9;
            }
            y += line;
        }
        turned(&upright, w, h, angle, 255)
    }

    /// `src` turned `angle` degrees counterclockwise as shown (y down) about its centre, nearest
    /// pixel; uncovered pixels are `fill`.
    fn turned(src: &[u8], w: usize, h: usize, angle: f64, fill: u8) -> Vec<u8> {
        let (s, c) = angle.to_radians().sin_cos();
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let mut out = vec![fill; w * h];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
                let (sx, sy) = (dx * c - dy * s + cx, dx * s + dy * c + cy);
                if sx >= 0.0 && sy >= 0.0 && (sx as usize) < w && (sy as usize) < h {
                    out[y * w + x] = src[sy as usize * w + sx as usize];
                }
            }
        }
        out
    }

    #[test]
    fn a_text_page_turned_three_degrees_is_found_within_a_tenth() {
        let (w, h) = (850, 1100);
        for angle in [3.0, -3.0] {
            let skew = detect_skew(&text_page(w, h, angle), w, h).unwrap();
            assert!((skew.angle - angle).abs() <= 0.1, "turned {angle}°, found {skew:?}");
            assert!(skew.confidence >= MIN_CONFIDENCE, "{skew:?}");
        }
    }

    #[test]
    fn a_straight_page_reads_zero_and_wide_tilts_are_found_too() {
        let (w, h) = (850, 1100);
        let skew = detect_skew(&text_page(w, h, 0.0), w, h).unwrap();
        assert!(skew.angle.abs() <= 0.05 && skew.confidence >= MIN_CONFIDENCE, "{skew:?}");
        for angle in [0.4, -7.25, 12.5] {
            let skew = detect_skew(&text_page(w, h, angle), w, h).unwrap();
            assert!((skew.angle - angle).abs() <= 0.1, "turned {angle}°, found {skew:?}");
        }
    }

    #[test]
    fn blank_pages_and_photos_are_skipped() {
        let (w, h) = (600, 800);
        assert_eq!(detect_skew(&vec![250u8; w * h], w, h), None, "blank paper");
        // A few specks on white.
        let mut specks = vec![255u8; w * h];
        for i in (0..w * h).step_by(9973) {
            specks[i] = 0;
        }
        assert_eq!(detect_skew(&specks, w, h), None, "specks are too little ink");
        // A photo: smooth light and shade with grain.
        let mut rng = Lcg(3);
        let photo: Vec<u8> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f64, (i / w) as f64);
                let v = 128.0 + 60.0 * (x / 37.0).sin() * (y / 53.0).cos() + 40.0 * ((x + y) / 91.0).sin() + rng.next(30) as f64 - 15.0;
                v.clamp(0.0, 255.0) as u8
            })
            .collect();
        assert!(detect_skew(&photo, w, h).is_none_or(|s| s.confidence < MIN_CONFIDENCE), "{:?}", detect_skew(&photo, w, h));
        // Dark blobs on white, below the ink share of a photo, still don't make lines.
        let blobs: Vec<u8> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f64, (i / w) as f64);
                if ((x - 300.0).powi(2) + (y - 400.0).powi(2)).sqrt() < 140.0 { 30 } else { 250 }
            })
            .collect();
        assert!(detect_skew(&blobs, w, h).is_none_or(|s| s.confidence < MIN_CONFIDENCE), "{:?}", detect_skew(&blobs, w, h));
    }

    #[test]
    fn short_empty_and_huge_rasters_say_nothing_and_never_panic() {
        assert_eq!(detect_skew(&[], 0, 0), None);
        assert_eq!(detect_skew(&[0, 255, 0], 2, 2), None, "the buffer is smaller than the size");
        assert_eq!(detect_skew(&[0; 16], usize::MAX, 2), None, "the size overflows");
        assert_eq!(detect_skew(&[0, 255, 0, 255], 4, 1), None, "too little ink to go by");
        assert_eq!(grey_over_white(&[0, 0, 0, 255, 0, 0, 0, 0, 9]), [0, 255]);
        assert!(angles(1.0, 0.0, 0.5).is_empty(), "an empty range has no angles");
    }

    #[test]
    fn a_raster_too_large_to_read_in_full_is_sampled_without_a_bias_to_zero() {
        // 4300 × 4300 is over MAX_PIXELS, so every second pixel of every second row is read.
        let (w, h) = (4300, 4300);
        assert!(w * h > MAX_PIXELS);
        let skew = detect_skew(&text_page(w, h, 2.0), w, h).unwrap();
        assert!((skew.angle - 2.0).abs() <= 0.15 && skew.confidence >= MIN_CONFIDENCE, "{skew:?}");
    }

    /// Thirty lines of prose on a Letter page, as PdfCraft writes text.
    fn prose(s: &Session) -> Arc<Vec<u8>> {
        let line = "The quick brown fox jumps over the lazy dog while the scanner hums along.";
        s.create_from_text("t", &vec![line; 30].join("\n")).unwrap()
    }

    /// A two-page scan at 100 dpi: the prose turned 3° clockwise, then a photo.
    fn tilted_scan(s: &mut Session) -> DocId {
        let bytes = prose(s);
        let text = s.open("text.pdf", None, bytes, None).unwrap();
        s.apply(text, Edit::RotateContent { pages: vec![0], degrees: 3.0, fit: RotateFit::Crop }).unwrap();
        let page = crate::export::Exporter::new(s.get(text).unwrap()).png(0, 100.0).unwrap();
        let (w, h) = (850u32, 1100u32);
        let photo = image::RgbImage::from_fn(w, h, |x, y| {
            let (x, y) = (f64::from(x), f64::from(y));
            let v = (128.0 + 70.0 * (x / 41.0).sin() * (y / 57.0).cos() + 40.0 * ((x + y) / 97.0).sin()).clamp(0.0, 255.0) as u8;
            image::Rgb([v, v.saturating_add(20), v / 2])
        });
        let mut png = Vec::new();
        photo.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let scan =
            s.create_from_images_with_resolution(&[("a.png".into(), page), ("b.png".into(), png)], crate::ImageResolution::Dpi(100.0)).unwrap();
        s.open("scan.pdf", None, scan, None).unwrap()
    }

    #[test]
    fn a_tilted_scan_is_found_straightened_in_one_step_and_then_reads_straight() {
        let mut s = Session::new().with_clock(|| 1_700_000_000);
        let id = tilted_scan(&mut s);
        let before = s.get(id).unwrap().bytes.clone();
        let size = |s: &Session| s.get(id).unwrap().info.pages.iter().map(|p| (p.width, p.height)).collect::<Vec<_>>();
        let sizes = size(&s);
        let found = s.find_skew(id, &DeskewOptions::default()).unwrap();
        assert_eq!(found.len(), 2);
        let tilt = found[0].angle.unwrap();
        // Turned 3° clockwise, so it is straightened by 3° counterclockwise.
        assert!((tilt + 3.0).abs() <= 0.1, "{:?}", found[0]);
        assert!(found[0].straightens() && found[0].confidence >= MIN_CONFIDENCE, "{:?}", found[0]);
        assert_eq!(found[1].skip, Some(DeskewSkip::Unsure), "the photo is left alone: {:?}", found[1]);
        assert_eq!(to_straighten(&found), [(0, tilt)]);

        s.apply(id, Edit::straighten(to_straighten(&found))).unwrap();
        assert_eq!(s.get(id).unwrap().can_undo(), Some("Straighten pages"));
        assert_eq!(size(&s), sizes, "the pages keep their size");
        let again = s.find_skew(id, &DeskewOptions::default()).unwrap();
        assert!(again[0].angle.unwrap().abs() <= 0.1, "straight now: {:?}", again[0]);
        assert_eq!(again[0].skip, Some(DeskewSkip::Straight), "and still a scan: {:?}", again[0]);
        assert!(to_straighten(&again).is_empty());
        s.undo(id).unwrap();
        assert_eq!(s.get(id).unwrap().bytes, before, "one undo step restores the scan");
    }

    #[test]
    fn text_pages_comments_and_bad_options_are_reported_not_turned() {
        let mut s = Session::new().with_clock(|| 1_700_000_000);
        let bytes = prose(&s);
        let text = s.open("text.pdf", None, bytes, None).unwrap();
        s.apply(text, Edit::RotateContent { pages: vec![0], degrees: -2.0, fit: RotateFit::Crop }).unwrap();
        let found = s.find_skew(text, &DeskewOptions::default()).unwrap();
        assert_eq!(found[0].skip, Some(DeskewSkip::NotScan), "a page of text is not a scan");
        assert_eq!(found[0].angle, None, "and is not even drawn");
        let all = DeskewOptions { only_scans: false, ..Default::default() };
        let found = s.find_skew(text, &all).unwrap();
        assert!(found[0].straightens() && (found[0].angle.unwrap() - 2.0).abs() <= 0.1, "{:?}", found[0]);
        // A large smallest angle leaves it alone.
        let found = s.find_skew(text, &DeskewOptions { min_angle: 5.0, ..all.clone() }).unwrap();
        assert_eq!(found[0].skip, Some(DeskewSkip::Straight));

        // A comment can't turn with the page: measured, reported, not straightened.
        let comment = crate::NewAnnotation {
            page: 0,
            shape: crate::Shape::Rectangle { rect: [20.0, 20.0, 60.0, 60.0] },
            style: crate::Style { color: [0.0, 0.0, 1.0], opacity: 1.0, width: 2.0, fill: None },
            contents: "Look".into(),
            author: "Reviewer".into(),
        };
        s.apply(text, Edit::AddAnnotation(comment)).unwrap();
        let found = s.find_skew(text, &all).unwrap();
        assert_eq!(found[0].skip, Some(DeskewSkip::Annotated), "{:?}", found[0]);
        assert!(found[0].angle.is_some());

        for min_angle in [f64::NAN, -1.0, 16.0] {
            let err = s.deskew_job(text, &DeskewOptions { min_angle, ..Default::default() }).err().unwrap();
            assert!(err.to_string().contains("from 0 to 15 degrees"), "{err}");
        }
        let err = s.deskew_job(text, &DeskewOptions { pages: vec![3], ..Default::default() }).err().unwrap();
        assert!(err.to_string().contains("page 4 doesn't exist"), "{err}");
        // Cancelling stops the search.
        let job = s.deskew_job(text, &all).unwrap();
        assert_eq!(job.run(|_, _| false).err(), Some(EditError::Cancelled));
    }
}
