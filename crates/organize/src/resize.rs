//! Change page size (execution plan PL7): give pages a new size, fitting, filling or simply
//! re-centring their content, through [`crate::transform_page`]'s wrapper (the content streams
//! are not rewritten).
//!
//! Sizes are as the page is **shown**: a target of 595 × 842 pt (A4 portrait) on a page with
//! `/Rotate 90` gives it an 842 × 595 user-space box, which displays as A4 portrait. A page's
//! `/UserUnit` (ISO 32000-2 §14.11.2.1) is honoured, so the target is always in points.

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Dict, Document, ObjRef};

use crate::boxes::boxes_of;
use crate::transform::{apply, page_rotate};
use crate::{OrganizeError, check, walk};

/// The largest page side Change page size accepts, in points: 200 in, the page size limit PDF
/// readers commonly enforce (ISO 32000-2 Annex C, at a user unit of 1).
pub const MAX_PAGE_SIDE: f64 = 14_400.0;
/// The smallest page side, in points.
const MIN_PAGE_SIDE: f64 = 1.0;
/// The largest user-space coordinate `transform_page` writes.
const MAX_USER_COORD: f64 = 1.0e7;
/// How far a page may be off the target, on each side, and still count as that size: half a
/// point, like `pdfcraft_print::matching_paper`, so metric sizes rounded to whole points
/// (595 × 842 for A4) aren't "different".
pub const SIZE_TOLERANCE: f64 = 0.5;

/// How [`resize_pages`] places the old content on the new page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResizeMode {
    /// Scale to fit inside the new page, keeping the aspect ratio, centred: nothing is cut off.
    #[default]
    Fit,
    /// Scale to cover the whole new page, keeping the aspect ratio, centred; what sticks out is
    /// cut off.
    Fill,
    /// Keep the content's size and centre it: the canvas grows (margins) or shrinks (crop).
    NoScale,
}

impl ResizeMode {
    pub const ALL: [ResizeMode; 3] = [ResizeMode::Fit, ResizeMode::Fill, ResizeMode::NoScale];

    /// The name used by the automation tool: `fit`, `fill` or `none`.
    pub fn name(self) -> &'static str {
        match self {
            ResizeMode::Fit => "fit",
            ResizeMode::Fill => "fill",
            ResizeMode::NoScale => "none",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        ResizeMode::ALL.into_iter().find(|m| m.name() == s)
    }
}

/// Which way round the target size goes on each page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SizeOrientation {
    /// Every page shown upright: the shorter side across.
    Portrait,
    /// Every page shown sideways: the longer side across.
    Landscape,
    /// Each page keeps the orientation it is shown in (a square page counts as portrait).
    #[default]
    Match,
}

impl SizeOrientation {
    pub const ALL: [SizeOrientation; 3] = [SizeOrientation::Portrait, SizeOrientation::Landscape, SizeOrientation::Match];

    /// The name used by the automation tool: `portrait`, `landscape` or `match`.
    pub fn name(self) -> &'static str {
        match self {
            SizeOrientation::Portrait => "portrait",
            SizeOrientation::Landscape => "landscape",
            SizeOrientation::Match => "match",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        SizeOrientation::ALL.into_iter().find(|o| o.name() == s)
    }
}

fn invalid(msg: impl Into<String>) -> OrganizeError {
    OrganizeError::Invalid(msg.into())
}

fn check_size(width: f64, height: f64) -> Result<(), OrganizeError> {
    let ok = |v: f64| v.is_finite() && (MIN_PAGE_SIDE..=MAX_PAGE_SIDE).contains(&v);
    if ok(width) && ok(height) {
        return Ok(());
    }
    Err(invalid(format!("a page size of {width} × {height} pt can't be used: each side must be from 1 to {MAX_PAGE_SIDE} pt (200 in, 5080 mm)")))
}

/// What one page looks like for a resize: its effective crop box (user space), `/Rotate` and
/// user unit.
struct PageShape {
    crop: [f64; 4],
    quarter_turned: bool,
    unit: f64,
}

impl PageShape {
    fn of(doc: &Document, page: ObjRef, inherited: &Dict) -> Self {
        let crop = boxes_of(doc, page, inherited)[1];
        let quarter_turned = page_rotate(doc, page, inherited).rem_euclid(180) == 90;
        let unit = doc
            .get(page)
            .as_dict()
            .and_then(|d| d.get(b"UserUnit").map(|u| doc.resolve(u)).and_then(|u| u.as_f64()))
            .filter(|u| u.is_finite() && *u > 0.0)
            .unwrap_or(1.0);
        Self { crop, quarter_turned, unit }
    }

    /// The size the page is shown at, in points.
    fn shown(&self) -> (f64, f64) {
        let (w, h) = ((self.crop[2] - self.crop[0]) * self.unit, (self.crop[3] - self.crop[1]) * self.unit);
        if self.quarter_turned { (h, w) } else { (w, h) }
    }

    /// The target size as this page will show it, in points, with `orientation` applied.
    fn target(&self, width: f64, height: f64, orientation: SizeOrientation) -> (f64, f64) {
        let (short, long) = (width.min(height), width.max(height));
        let landscape = match orientation {
            SizeOrientation::Portrait => false,
            SizeOrientation::Landscape => true,
            SizeOrientation::Match => {
                let (w, h) = self.shown();
                w > h
            }
        };
        if landscape { (long, short) } else { (short, long) }
    }

    fn differs(&self, width: f64, height: f64, orientation: SizeOrientation) -> bool {
        let (w, h) = self.shown();
        let (tw, th) = self.target(width, height, orientation);
        (w - tw).abs() > SIZE_TOLERANCE || (h - th).abs() > SIZE_TOLERANCE
    }

    /// The matrix and media box (user space) that give the page the target size.
    fn transform(
        &self,
        width: f64,
        height: f64,
        orientation: SizeOrientation,
        mode: ResizeMode,
        index: usize,
    ) -> Result<(Matrix, [f64; 4]), OrganizeError> {
        let n = index.saturating_add(1);
        let (sw, sh) = self.target(width, height, orientation);
        // Shown size → user space: undo the quarter turn and the user unit.
        let (tw, th) = if self.quarter_turned { (sh, sw) } else { (sw, sh) };
        let (tw, th) = (tw / self.unit, th / self.unit);
        let [x0, y0, x1, y1] = self.crop;
        let (cw, ch) = (x1 - x0, y1 - y0);
        if !(cw >= MIN_PAGE_SIDE && ch >= MIN_PAGE_SIDE) {
            return Err(invalid(format!("page {n} has an empty crop box, so there is nothing to resize")));
        }
        let (sx, sy) = (tw / cw, th / ch);
        let s = match mode {
            ResizeMode::Fit => sx.min(sy),
            ResizeMode::Fill => sx.max(sy),
            ResizeMode::NoScale => 1.0,
        };
        if !(s.is_finite() && s > 0.0 && tw.is_finite() && th.is_finite()) {
            return Err(invalid(format!("page {n} can't be resized to {width} × {height} pt")));
        }
        // Scale about the crop box's corner, then centre it on the new page.
        let m = Matrix([s, 0.0, 0.0, s, (tw - s * cw) / 2.0 - s * x0, (th - s * ch) / 2.0 - s * y0]);
        Ok((m, [0.0, 0.0, tw, th]))
    }
}

/// Give the pages at `indices` (0-based) the size `width` × `height` (points, as the page is
/// shown, turned as `orientation` says), placing their content as `mode` says.
///
/// Each page goes through [`crate::transform_page`]'s wrapper: the content streams are kept
/// byte for byte and wrapped in a scale-and-move, MediaBox and CropBox become the new page
/// (lower-left corner at the origin), Trim/Bleed/Art boxes follow the content, and every
/// annotation's `/Rect` and points scale with the page exactly (their appearances follow the
/// `/Rect`), so no flattening is needed. `/Rotate` is left alone.
///
/// A page listed twice is resized once. Every page is checked before any changes, so an error
/// leaves the document as it was.
pub fn resize_pages(
    doc: &mut Document,
    indices: &[usize],
    width: f64,
    height: f64,
    orientation: SizeOrientation,
    mode: ResizeMode,
) -> Result<(), OrganizeError> {
    check_size(width, height)?;
    if indices.is_empty() {
        return Err(invalid("no pages to resize"));
    }
    let all = walk(doc)?;
    check(indices, all.len())?;
    let mut selected = indices.to_vec();
    selected.sort_unstable();
    selected.dedup();
    let mut plans = Vec::with_capacity(selected.len());
    for &i in &selected {
        let (page, inherited) = all.get(i).ok_or(OrganizeError::NoSuchPage(i))?;
        let shape = PageShape::of(doc, *page, inherited);
        let (m, media) = shape.transform(width, height, orientation, mode, i)?;
        // What `apply` refuses, refused before anything changes (only an odd /UserUnit gets here).
        if !media.iter().all(|v| v.is_finite()) || media[2].min(media[3]) < MIN_PAGE_SIDE || media[2].max(media[3]) > MAX_USER_COORD {
            return Err(invalid(format!("page {} can't be given that size: its /UserUnit is unusual", i.saturating_add(1))));
        }
        plans.push((i, *page, shape.crop, m, media));
    }
    for (i, page, crop, m, media) in plans {
        apply(doc, i, page, crop, m, media)?;
    }
    Ok(())
}

/// The matrix and media box [`resize_pages`] would give page `index` (0-based): useful to
/// preview a resize, or to check where a point of the old page lands.
pub fn resize_transform(
    doc: &Document,
    index: usize,
    width: f64,
    height: f64,
    orientation: SizeOrientation,
    mode: ResizeMode,
) -> Result<([f64; 6], [f64; 4]), OrganizeError> {
    check_size(width, height)?;
    let all = walk(doc)?;
    check(&[index], all.len())?;
    let (page, inherited) = all.get(index).ok_or(OrganizeError::NoSuchPage(index))?;
    let (m, media) = PageShape::of(doc, *page, inherited).transform(width, height, orientation, mode, index)?;
    Ok((m.0, media))
}

/// Of `indices` (0-based), in the order given, the pages whose shown size differs from
/// `width` × `height` turned as `orientation` says, by more than [`SIZE_TOLERANCE`] on a side:
/// the pages "only pages that differ" resizes.
pub fn pages_of_other_size(
    doc: &Document,
    indices: &[usize],
    width: f64,
    height: f64,
    orientation: SizeOrientation,
) -> Result<Vec<usize>, OrganizeError> {
    check_size(width, height)?;
    let all = walk(doc)?;
    check(indices, all.len())?;
    Ok(indices
        .iter()
        .copied()
        .filter(|i| all.get(*i).is_some_and(|(page, inherited)| PageShape::of(doc, *page, inherited).differs(width, height, orientation)))
        .collect())
}

#[cfg(test)]
mod tests;
