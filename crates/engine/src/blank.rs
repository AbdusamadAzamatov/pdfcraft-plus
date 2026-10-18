//! Organize ▸ Layout ▸ Remove blank pages (PL5): find the pages that look empty.
//!
//! A page is a candidate when either test holds:
//! - **structural**: its content draws nothing (no painting operator: no `Do`, `sh`, inline
//!   image, text showing, fill or stroke), it has no annotations and no XObjects;
//! - **rendered**: drawn at [`DPI`] and read in grey, at most [`BlankOptions::sensitivity`] of its
//!   pixels are "ink" (luma below [`INK_LUMA`]), so a scanned page with a few specks counts.
//!
//! Finding is read-only. Removing the pages is the ordinary [`crate::Edit::DeletePages`] (one undo
//! step), which refuses to remove every page. Rendering every page takes a while, so the search is
//! a job like OCR's: [`Session::blank_job`] captures what it needs, and [`BlankJob::run`] works
//! anywhere (the UI runs it on a worker thread), reporting progress and stopping when asked.

use std::sync::Arc;

use pdfcraft_cos::Object;
use pdfcraft_render::{PRINT_TAG, PageRenderer, RenderConfig, RenderRequest};

use crate::{DocId, EditError, Session, guard};

/// The resolution pages are drawn at for the rendered test.
pub const DPI: f32 = 36.0;
/// A pixel whose luma (0–255, composed over white paper) is below this is ink.
pub const INK_LUMA: u32 = 230;
/// The default sensitivity: at most 0.2% of a page's pixels may be ink.
pub const DEFAULT_SENSITIVITY: f32 = 0.002;

/// Operators that paint (ISO 32000-2 §8.5.3 path painting, §8.8 `Do`, §8.7.4.2 `sh`, §8.9.7
/// inline images, §9.4.3 text showing). `n` ends a path without painting and is not here.
const PAINTING: &[&str] = &["Do", "sh", "BI", "Tj", "TJ", "'", "\"", "f", "F", "f*", "S", "s", "B", "B*", "b", "b*"];

/// The most decoded content read for the structural test; a page with more is judged by its
/// picture.
const MAX_CONTENT: usize = 64 << 20;

/// What to look for.
#[derive(Clone, Debug, PartialEq)]
pub struct BlankOptions {
    /// The largest share of ink pixels (0 to 1) a page may have and still count as blank: 0.002
    /// is 0.2%. 0 accepts only pages with no ink at all.
    pub sensitivity: f32,
    /// Leave pages that have comments or form fields alone, however empty they look (links don't
    /// count: they draw nothing of their own).
    pub ignore_annotated: bool,
    /// The pages to check (0-based); empty checks every page.
    pub pages: Vec<usize>,
}

impl Default for BlankOptions {
    fn default() -> Self {
        BlankOptions { sensitivity: DEFAULT_SENSITIVITY, ignore_annotated: true, pages: Vec::new() }
    }
}

/// A page that looks blank.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlankPage {
    /// 0-based.
    pub page: usize,
    /// The share of ink pixels when drawn at [`DPI`] (0 for a structurally blank page, which is
    /// not drawn).
    pub ink_ratio: f32,
    /// Its content draws nothing, and it has no annotations or XObjects.
    pub structural: bool,
}

/// The luma (Rec. 601, 0–255) of a premultiplied RGBA pixel composed over white paper.
fn luma_over_white(px: &[u8; 4]) -> u32 {
    let gap = 255 - u32::from(px[3]);
    let [r, g, b] = [px[0], px[1], px[2]].map(|c| (u32::from(c) + gap).min(255));
    ((299 * r + 587 * g + 114 * b) / 1000).min(255)
}

/// How many pixels of a premultiplied RGBA8 raster are ink, and how many pixels there are. A
/// trailing partial pixel is ignored.
pub fn ink_pixels(rgba: &[u8]) -> (u64, u64) {
    let pixels = rgba.as_chunks::<4>().0;
    let ink = pixels.iter().filter(|px| luma_over_white(px) < INK_LUMA).count();
    (ink as u64, pixels.len() as u64)
}

/// The share of ink pixels in a premultiplied RGBA8 raster, from 0 to 1; `None` when it has no
/// pixels (nothing can be said about such a page).
pub fn ink_ratio(rgba: &[u8]) -> Option<f32> {
    let (ink, total) = ink_pixels(rgba);
    (total > 0).then(|| (ink as f64 / total as f64) as f32)
}

/// What a page's objects (not its picture) say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Facts {
    /// Draws nothing, no annotations, no XObjects.
    structural: bool,
    /// Has a comment or form field (an annotation other than a link or popup).
    annotated: bool,
}

/// The page's content, concatenated (`/Contents` may be one stream or an array). `None` when a
/// stream can't be decoded or there is more than [`MAX_CONTENT`], so what it draws is unknown.
fn content_bytes(doc: &pdfcraft_cos::Document, contents: Option<&Object>) -> Option<Vec<u8>> {
    let Some(contents) = contents else { return Some(Vec::new()) };
    match &*doc.resolve(contents) {
        Object::Stream(s) => s.decoded_within(MAX_CONTENT).ok(),
        Object::Array(a) => {
            let mut out = Vec::new();
            for c in a {
                match &*doc.resolve(c) {
                    Object::Stream(s) => {
                        let left = MAX_CONTENT.checked_sub(out.len())?;
                        out.extend(s.decoded_within(left).ok()?);
                        out.push(b'\n');
                    }
                    Object::Null => {}
                    _ => return None,
                }
            }
            Some(out)
        }
        Object::Null => Some(Vec::new()),
        _ => None,
    }
}

fn facts(doc: &pdfcraft_cos::Document, page: &pdfcraft_model::Page) -> Facts {
    let d = &page.dict;
    let annots = d.get(b"Annots").map(|a| doc.resolve(a));
    let annots: Vec<Object> = annots.as_deref().and_then(Object::as_array).cloned().unwrap_or_default();
    let annotated = annots.iter().any(|a| {
        let a = doc.resolve(a);
        a.as_dict().is_some_and(|a| !matches!(a.name(b"Subtype"), Some(b"Link" | b"Popup")))
    });
    let xobjects = d
        .get(b"Resources")
        .map(|r| doc.resolve(r))
        .and_then(|r| r.as_dict().and_then(|r| r.get(b"XObject").cloned()))
        .map(|x| doc.resolve(&x))
        .is_some_and(|x| x.as_dict().is_some_and(|x| !x.is_empty()));
    let draws = || match content_bytes(doc, d.get(b"Contents")) {
        // Unreadable content may draw anything: only the picture can tell.
        None => true,
        Some(data) => {
            let parsed = pdfcraft_content::parse(&data);
            parsed.skipped > 0 || parsed.ops.iter().any(|op| PAINTING.iter().any(|p| op.is(p)))
        }
    };
    Facts { structural: annots.is_empty() && !xobjects && !draws(), annotated }
}

/// Everything finding blank pages needs from a document, detached from the session.
pub struct BlankJob {
    cos: pdfcraft_cos::Document,
    bytes: Arc<Vec<u8>>,
    config: RenderConfig,
    /// Each page's displayed size in points.
    sizes: Vec<(f32, f32)>,
    /// The pages to check (0-based, in order, each once).
    pages: Vec<usize>,
    sensitivity: f32,
    ignore_annotated: bool,
}

impl BlankJob {
    /// How many pages are checked.
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// No pages are checked.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Check the pages. `progress(done, total)` is called before each page and once at the end;
    /// returning `false` stops with [`EditError::Cancelled`]. A page that can't be drawn is not a
    /// candidate. A panic is returned as an error, so a worker thread always reports back.
    pub fn run(self, mut progress: impl FnMut(usize, usize) -> bool) -> Result<Vec<BlankPage>, EditError> {
        guard(move || self.work(&mut progress)).map_err(|e| EditError::Invalid(format!("couldn't check the pages: {e}")))?
    }

    fn work(self, progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<Vec<BlankPage>, EditError> {
        let all = pdfcraft_model::pages(&self.cos);
        let mut renderer = PageRenderer::new(self.bytes.clone(), self.config.clone());
        let total = self.pages.len();
        let mut out = Vec::new();
        for (done, &page) in self.pages.iter().enumerate() {
            if !progress(done, total) {
                return Err(EditError::Cancelled);
            }
            // A page the object walk doesn't see is judged by its picture alone.
            let f = all.get(page).map(|p| facts(&self.cos, p)).unwrap_or_default();
            if f.annotated && self.ignore_annotated {
                continue;
            }
            if f.structural {
                out.push(BlankPage { page, ink_ratio: 0.0, structural: true });
                continue;
            }
            let Some(&(w, h)) = self.sizes.get(page) else { continue };
            let scale = pdfcraft_render::effective_scale(w, h, DPI / 72.0);
            let r = renderer.render(RenderRequest { page, scale, tag: PRINT_TAG, ..Default::default() });
            // A page drawn only in part may hide its ink in the part that wasn't drawn.
            if r.error.is_some() || !r.warnings.is_empty() {
                continue;
            }
            if let Some(ink) = ink_ratio(r.rgba.as_ref())
                && ink <= self.sensitivity
            {
                out.push(BlankPage { page, ink_ratio: ink, structural: false });
            }
        }
        progress(total, total);
        Ok(out)
    }
}

impl Session {
    /// Capture what finding the blank pages of document `id` needs (see [`BlankJob::run`]).
    /// Refuses a sensitivity outside 0–1 and pages that don't exist.
    pub fn blank_job(&self, id: DocId, opts: &BlankOptions) -> Result<BlankJob, EditError> {
        if !opts.sensitivity.is_finite() || !(0.0..=1.0).contains(&opts.sensitivity) {
            return Err(EditError::Invalid(format!(
                "the sensitivity must be a share of the page between 0 and 1 (0.002 is 0.2%), not {}",
                opts.sensitivity
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
        // The page as it prints and shows: comments drawn (they are ink).
        config.hide_comments = false;
        Ok(BlankJob {
            cos,
            bytes: src.bytes,
            config,
            sizes: src.sizes,
            pages,
            sensitivity: opts.sensitivity,
            ignore_annotated: opts.ignore_annotated,
        })
    }

    /// Organize ▸ Layout ▸ Remove blank pages: the pages of document `id` that look blank, in
    /// page order. Read-only; delete the ones you want with [`crate::Edit::DeletePages`].
    pub fn find_blank_pages(&self, id: DocId, opts: &BlankOptions) -> Result<Vec<BlankPage>, EditError> {
        self.blank_job(id, opts)?.run(|_, _| true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(pixels: &[[u8; 4]]) -> Vec<u8> {
        pixels.iter().flatten().copied().collect()
    }

    #[test]
    fn ink_is_what_reads_darker_than_the_threshold_over_white() {
        const WHITE: [u8; 4] = [255, 255, 255, 255];
        const CLEAR: [u8; 4] = [0, 0, 0, 0];
        const BLACK: [u8; 4] = [0, 0, 0, 255];
        // Just above and just below the threshold, in grey.
        const LIGHT: [u8; 4] = [230, 230, 230, 255];
        const MID: [u8; 4] = [229, 229, 229, 255];
        // Half-covered black (premultiplied): 128 over white, ink.
        const HALF: [u8; 4] = [0, 0, 0, 127];
        // Pure blue is dark (luma 29), pure yellow light (luma 225, still ink).
        const BLUE: [u8; 4] = [0, 0, 255, 255];
        const YELLOW: [u8; 4] = [255, 255, 0, 255];
        assert_eq!(ink_pixels(&raster(&[WHITE, CLEAR, LIGHT])), (0, 3));
        assert_eq!(ink_pixels(&raster(&[BLACK, MID, HALF, BLUE, YELLOW, WHITE])), (5, 6));
        assert_eq!(ink_ratio(&raster(&[BLACK, WHITE, WHITE, WHITE])), Some(0.25));
        assert_eq!(ink_ratio(&raster(&[WHITE; 8])), Some(0.0));
    }

    #[test]
    fn an_empty_or_ragged_raster_says_nothing_and_never_panics() {
        assert_eq!(ink_pixels(&[]), (0, 0));
        assert_eq!(ink_ratio(&[]), None);
        assert_eq!(ink_ratio(&[0, 0, 0]), None, "a partial pixel is not a pixel");
        assert_eq!(ink_pixels(&[0, 0, 0, 255, 0, 0]), (1, 1));
        // Out-of-range premultiplied values (colour above alpha) are clamped, not overflowed.
        assert_eq!(ink_pixels(&[255, 255, 255, 0]), (0, 1));
    }
}
