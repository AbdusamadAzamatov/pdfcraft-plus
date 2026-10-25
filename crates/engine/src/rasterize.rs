//! Compress ▸ Clean up ▸ Rasterize pages (CU2): each chosen page becomes a single picture of
//! itself, for printers that mishandle vector content or to stop its text being copied. The text
//! is no longer searchable afterwards (Recognize text can add it back).
//!
//! The pictures come from [`crate::print_raster::rasterize_page_content`] (page content over white, optionally
//! grey, excluding live widgets), and [`Edit::RasterizePages`] puts them in place in one undo step:
//! [`pdfcraft_organize::replace_pages`] swaps each page's contents and resources for its picture
//! (the page's links, comments, form fields and the bookmarks pointing at it stay), then the page
//! gets its own boxes and `/Rotate` back and the picture is drawn over its visible area, so the
//! page keeps its exact size and its comments stay where they were, rotated pages included.
//!
//! Comments are either left as comments (the pictures are drawn without them) or burned into the
//! pictures, when they are removed from the pages the way Flatten removes the comments it draws
//! (their pop-ups and replies go too; one that can't be drawn stays).

use std::sync::Arc;

use pdfcraft_cos::{Dict, Object, Stream};
use pdfcraft_print::Raster;

use crate::{DocId, Edit, EditError, Session};

/// Lowest resolution offered, in dots per inch.
pub const MIN_DPI: f64 = 72.0;
/// Highest resolution offered, in dots per inch.
pub const MAX_DPI: f64 = 600.0;
/// The dialog's and the tool's default resolution.
pub const DEFAULT_DPI: f64 = 150.0;

/// How the pages are turned into pictures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterizeOptions {
    /// Resolution of the pictures, [`MIN_DPI`]–[`MAX_DPI`].
    pub dpi: f64,
    /// Grey pictures instead of colour ones.
    pub grayscale: bool,
    /// Draw the comments into the pictures and remove them from the pages. Otherwise the pictures
    /// leave them out and they stay as comments.
    pub burn_comments: bool,
}

impl Default for RasterizeOptions {
    fn default() -> Self {
        Self { dpi: DEFAULT_DPI, grayscale: false, burn_comments: false }
    }
}

impl Session {
    /// Rasterize `pages` (0-based; order and repeats don't matter) of document `id` as one undoable
    /// edit ([`Edit::RasterizePages`]). The pictures are drawn from the document as it is shown
    /// now (unsaved edits and hidden layers included). Needs the permission to change the
    /// document's content.
    pub fn rasterize_pages(&mut self, id: DocId, pages: &[usize], options: RasterizeOptions) -> Result<(), EditError> {
        let edit = self.rasterize_edit(id, pages, options)?;
        self.apply(id, edit)
    }

    /// The [`Edit::RasterizePages`] [`Self::rasterize_pages`] applies, with the pictures drawn, for
    /// callers that apply edits themselves. Refuses what applying it would refuse before drawing.
    pub fn rasterize_edit(&self, id: DocId, pages: &[usize], options: RasterizeOptions) -> Result<Edit, EditError> {
        if !options.dpi.is_finite() || !(MIN_DPI..=MAX_DPI).contains(&options.dpi) {
            return Err(EditError::Invalid(format!("the resolution must be between {MIN_DPI} and {MAX_DPI} dpi, not {}", options.dpi)));
        }
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        if doc.editor.is_none() {
            return Err(EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()));
        }
        if !doc.allows_modification() {
            return Err(EditError::NotPermitted("changes to the document"));
        }
        let count = doc.info.pages.len();
        if pages.is_empty() {
            return Err(EditError::Invalid("there are no pages to rasterize: choose at least one page".into()));
        }
        if let Some(&p) = pages.iter().find(|&&p| p >= count) {
            return Err(EditError::Invalid(format!("page {} does not exist; the document has {count} pages", p.saturating_add(1))));
        }
        let mut distinct = pages.to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        let raster = Raster { dpi: options.dpi, grayscale: options.grayscale };
        let bytes = crate::guard(|| crate::print_raster::rasterize_page_content(doc, &distinct, &raster, options.burn_comments))
            .map_err(EditError::Print)?
            .map_err(|e| EditError::Print(format!("the pages could not be drawn as pictures: {e}")))?;
        Ok(Edit::RasterizePages { pages: distinct, bytes: Arc::new(bytes), burn_comments: options.burn_comments })
    }
}

/// One page's geometry before it is replaced: its effective boxes and rotation.
struct Geometry {
    media: Option<Object>,
    crop: Option<Object>,
    bleed: Option<Object>,
    trim: Option<Object>,
    art: Option<Object>,
    rotate: i64,
}

/// The page attributes a page inherits from its ancestors (ISO 32000-2 §7.7.3.4) that matter here.
fn inherited(doc: &pdfcraft_cos::Document, page: pdfcraft_cos::ObjRef, key: &[u8]) -> Option<Object> {
    let mut node = page;
    // Page trees are shallow; a deeper or cyclic chain is broken and stops here.
    for _ in 0..64 {
        let obj = doc.get(node);
        let d = obj.as_dict()?;
        if let Some(v) = d.get(key) {
            return Some((*doc.resolve(v)).clone());
        }
        node = d.reference(b"Parent")?;
    }
    None
}

fn rect(doc: &pdfcraft_cos::Document, o: Option<&Object>) -> Option<[f64; 4]> {
    let o = o?;
    let a = o.as_array()?;
    let v: Vec<f64> = a.iter().map(|x| doc.resolve(x).as_f64()).collect::<Option<_>>()?;
    let [x0, y0, x1, y1] = <[f64; 4]>::try_from(v).ok()?;
    if ![x0, y0, x1, y1].iter().all(|n| n.is_finite()) {
        return None;
    }
    Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
}

/// The matrix that draws a unit-square picture of the page as displayed over the visible area
/// `[x0, y0, x1, y1]` of a page turned `rotate` degrees clockwise, in the page's own space.
fn placement([x0, y0, x1, y1]: [f64; 4], rotate: i64) -> [f64; 6] {
    let (w, h) = (x1 - x0, y1 - y0);
    match rotate.rem_euclid(360) {
        90 => [0.0, h, -w, 0.0, x1, y0],
        180 => [-w, 0.0, 0.0, -h, x1, y1],
        270 => [0.0, -h, w, 0.0, x0, y1],
        _ => [w, 0.0, 0.0, h, x0, y0],
    }
}

fn num(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.to_string() }
}

/// Apply [`Edit::RasterizePages`]: `bytes` is an image-only PDF with one page per entry of
/// `pages` (0-based, distinct), in order, as [`crate::print_raster::rasterize_page_content`] makes it.
pub(crate) fn apply(doc: &mut pdfcraft_cos::Document, pages: &[usize], bytes: &Arc<Vec<u8>>, burn_comments: bool) -> Result<(), EditError> {
    let src = crate::open_source("rasterized pages.pdf", bytes)?;
    let pictures = pdfcraft_organize::page_count(&src)?;
    if pictures != pages.len() || pages.is_empty() {
        return Err(EditError::Invalid(format!("{pictures} pictures can't replace {} pages", pages.len())));
    }
    let refs = pdfcraft_organize::pages(doc)?;
    let mut targets = Vec::with_capacity(pages.len());
    for &p in pages {
        let page = refs.get(p).ok_or(pdfcraft_organize::OrganizeError::NoSuchPage(p))?.obj;
        let direct = doc.get(page).as_dict().cloned().unwrap_or_default();
        let own = |k: &[u8]| direct.get(k).map(|v| (*doc.resolve(v)).clone());
        let geometry = Geometry {
            media: inherited(doc, page, b"MediaBox"),
            crop: inherited(doc, page, b"CropBox"),
            bleed: own(b"BleedBox"),
            trim: own(b"TrimBox"),
            art: own(b"ArtBox"),
            rotate: inherited(doc, page, b"Rotate").and_then(|r| r.as_int()).unwrap_or(0),
        };
        // The visible area: the crop box, clipped to the media box (§14.11.2).
        let media = rect(doc, geometry.media.as_ref()).unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let visible = match rect(doc, geometry.crop.as_ref()) {
            Some(c) => [c[0].max(media[0]), c[1].max(media[1]), c[2].min(media[2]), c[3].min(media[3])],
            None => media,
        };
        if !(visible[2] - visible[0] > 0.0 && visible[3] - visible[1] > 0.0) {
            return Err(EditError::Invalid(format!("page {} has no visible area to rasterize", p.saturating_add(1))));
        }
        targets.push((p, page, geometry, visible));
    }
    if burn_comments {
        // The pictures show the comments; remove the ones drawn, as Flatten does (the page content
        // it writes is replaced just below).
        pdfcraft_edit::flatten(doc, pages, true, false)?;
    }
    let src_pages: Vec<usize> = (0..pictures).collect();
    pdfcraft_organize::replace_pages(doc, pages, &src, &src_pages)?;
    for (p, page, g, visible) in targets {
        let resources = doc.get(page).as_dict().and_then(|d| d.get(b"Resources").cloned()).map(|r| doc.resolve(&r));
        let name = resources
            .as_ref()
            .and_then(|r| r.as_dict())
            .and_then(|r| r.get(b"XObject").map(|x| doc.resolve(x)))
            .and_then(|x| x.as_dict().and_then(|x| x.iter().next().map(|(k, _)| k.clone())))
            .ok_or_else(|| EditError::Invalid(format!("the picture of page {} is missing", p.saturating_add(1))))?;
        let m = placement(visible, g.rotate).map(num);
        let content = format!("q {} {} {} {} {} {} cm /{} Do Q\n", m[0], m[1], m[2], m[3], m[4], m[5], String::from_utf8_lossy(&name));
        let contents = doc.add(Object::Stream(Stream::flate(Dict::new(), content.as_bytes())));
        doc.update_dict(page, |d| {
            d.set(b"Contents".to_vec(), Object::Ref(contents));
            for (k, v) in [(&b"MediaBox"[..], &g.media), (b"CropBox", &g.crop), (b"BleedBox", &g.bleed), (b"TrimBox", &g.trim), (b"ArtBox", &g.art)] {
                match v {
                    Some(v) => d.set(k.to_vec(), v.clone()),
                    None => {
                        d.remove(k);
                    }
                }
            }
            if g.rotate == 0 {
                d.remove(b"Rotate");
            } else {
                d.set(b"Rotate".to_vec(), Object::Int(g.rotate));
            }
        })
        .map_err(|e| EditError::Write(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each corner of the displayed picture lands on the user-space corner a viewer shows there.
    #[test]
    fn the_picture_is_placed_upright_for_every_rotation() {
        let apply = |m: [f64; 6], (s, t): (f64, f64)| (m[0] * s + m[2] * t + m[4], m[1] * s + m[3] * t + m[5]);
        let area = [10.0, 20.0, 110.0, 220.0];
        // (rotation, the user point shown at the picture's top-left, its bottom-right)
        for (rotate, top_left, bottom_right) in [
            (0, (10.0, 220.0), (110.0, 20.0)),
            (90, (10.0, 20.0), (110.0, 220.0)),
            (180, (110.0, 20.0), (10.0, 220.0)),
            (270, (110.0, 220.0), (10.0, 20.0)),
        ] {
            let m = placement(area, rotate);
            assert_eq!(apply(m, (0.0, 1.0)), top_left, "{rotate}");
            assert_eq!(apply(m, (1.0, 0.0)), bottom_right, "{rotate}");
        }
        assert_eq!(placement(area, -90), placement(area, 270));
        assert_eq!(num(-0.0), "0");
        assert_eq!(num(12.5), "12.5");
        assert_eq!(num(100.0), "100");
    }
}
