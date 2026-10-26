//! Stitch pages: a new document with chosen pages placed one after another on a single long page
//! (execution plan PL6). Each page is drawn from a Form XObject copy of it ([`page_as_form`]),
//! placed with `cm`, so the result is vector and lossless. Only what the pages draw comes over:
//! links, comments and form fields are not carried.
//!
//! Viewers cap a page side at 14,400 units. A longer strip keeps its size in points through
//! `/UserUnit` (ISO 32000-2 §14.11.2): the smallest whole unit that brings both sides within the
//! cap, with the drawing scaled down to match.

use std::collections::HashMap;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::import::page_as_form;
use crate::resize::MAX_PAGE_SIDE;
use crate::transform::num;
use crate::{OrganizeError, check, rebuild, walk};

/// The most pages one stitched page may hold.
pub const MAX_STITCH_PAGES: usize = 1_000;

/// The widest gap between pages, in points (one inch).
pub const MAX_STITCH_GAP: f64 = 72.0;

/// The largest `/UserUnit` viewers honour (as `pdfcraft_model::MAX_USER_UNIT`): a strip that
/// would need more is refused.
const MAX_UNIT: f64 = 75_000.0;

/// Which way the pages run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StitchDirection {
    /// Top to bottom, one page under the other.
    #[default]
    Vertical,
    /// Left to right, side by side.
    Horizontal,
}

/// Where a page narrower (vertical) or shorter (horizontal) than the strip sits across it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StitchAlign {
    /// Left edge (vertical) or top edge (horizontal).
    Start,
    /// Centred.
    #[default]
    Centre,
    /// Right edge (vertical) or bottom edge (horizontal).
    End,
}

/// How to stitch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StitchOptions {
    pub direction: StitchDirection,
    /// Space between neighbouring pages, in points: 0 to [`MAX_STITCH_GAP`].
    pub gap: f64,
    /// Fill the page white under the pages and gaps; otherwise leave it unpainted.
    pub white: bool,
    pub align: StitchAlign,
}

impl Default for StitchOptions {
    fn default() -> Self {
        Self { direction: StitchDirection::Vertical, gap: 0.0, white: true, align: StitchAlign::Centre }
    }
}

/// Where everything goes on the stitched page.
#[derive(Clone, Debug, PartialEq)]
pub struct StitchLayout {
    /// The page's size as shown, in points.
    pub size: (f64, f64),
    /// Its `/UserUnit`: 1 unless the strip is longer than 14,400 pt.
    pub user_unit: u32,
    /// Each page's bottom-left corner on the stitched page, in points, in order.
    pub origins: Vec<(f64, f64)>,
}

/// Lay out pages of these sizes (as shown, in points), in order.
pub fn stitch_layout(sizes: &[(f64, f64)], opts: &StitchOptions) -> Result<StitchLayout, OrganizeError> {
    if sizes.is_empty() {
        return Err(OrganizeError::Invalid("there are no pages to stitch: choose at least one page".into()));
    }
    if sizes.len() > MAX_STITCH_PAGES {
        return Err(OrganizeError::Invalid(format!("{} pages can't go on one page: stitch at most {MAX_STITCH_PAGES} pages at a time", sizes.len())));
    }
    if !opts.gap.is_finite() || !(0.0..=MAX_STITCH_GAP).contains(&opts.gap) {
        return Err(OrganizeError::Invalid(format!("the gap must be from 0 to {MAX_STITCH_GAP} pt (got {})", opts.gap)));
    }
    for (i, (w, h)) in sizes.iter().enumerate() {
        if !(w.is_finite() && h.is_finite() && *w > 0.0 && *h > 0.0) {
            return Err(OrganizeError::Invalid(format!("page {} has no visible area, so it can't be stitched; leave it out", i.saturating_add(1))));
        }
    }
    let vertical = opts.direction == StitchDirection::Vertical;
    // Along the strip: the pages' lengths plus the gaps; across it: the widest page.
    let along: f64 = sizes.iter().map(|(w, h)| if vertical { *h } else { *w }).sum::<f64>() + opts.gap * (sizes.len() - 1) as f64;
    let across = sizes.iter().map(|(w, h)| if vertical { *w } else { *h }).fold(0.0, f64::max);
    let size = if vertical { (across, along) } else { (along, across) };
    let longest = size.0.max(size.1);
    if !longest.is_finite() || longest / MAX_PAGE_SIDE > MAX_UNIT {
        return Err(OrganizeError::Invalid("the stitched page would be too large for any viewer: stitch fewer or smaller pages".into()));
    }
    // The smallest whole unit bringing both sides within the cap (at least 1, at most MAX_UNIT,
    // checked above, so the cast is exact).
    let user_unit = (longest / MAX_PAGE_SIDE).ceil().max(1.0) as u32;
    let mut origins = Vec::with_capacity(sizes.len());
    let mut at = 0.0;
    for (w, h) in sizes {
        let (len, wide) = if vertical { (*h, *w) } else { (*w, *h) };
        let slack = across - wide;
        let offset = match opts.align {
            StitchAlign::Start => 0.0,
            StitchAlign::Centre => slack / 2.0,
            StitchAlign::End => slack,
        };
        // Vertical runs top to bottom (y measured up); horizontal runs left to right, and its
        // start is the top edge.
        origins.push(if vertical { (offset, along - at - len) } else { (at, across - offset - wide) });
        at += len + opts.gap;
    }
    Ok(StitchLayout { size, user_unit, origins })
}

/// A page of a source document as a form in a new one: the form, its size as shown in points
/// (its own `/UserUnit` included), and that unit.
pub(crate) type ShownForm = (ObjRef, (f64, f64), f64);

/// Page `p` of `src` (whose pages `all` came from [`walk`]) as a [`ShownForm`] in `out`.
pub(crate) fn shown_form(out: &mut Document, src: &Document, all: &[(ObjRef, Dict)], p: usize) -> Result<ShownForm, OrganizeError> {
    let (r, (w, h)) = page_as_form(out, src, p)?;
    let unit = all
        .get(p)
        .and_then(|(obj, _)| src.get(*obj).as_dict().and_then(|d| d.get(b"UserUnit").and_then(|u| src.resolve(u).as_f64())))
        .filter(|u| *u >= 1.0)
        .map_or(1.0, |u| u.min(MAX_UNIT));
    Ok((r, (w * unit, h * unit), unit))
}

/// A new document whose single page shows `pages` (0-based, in this order; a page may repeat) of
/// `src`, as they are displayed, one after another. The pages' content is copied; their
/// annotations (links, comments, form fields) are not. A page's own `/UserUnit` is honoured, so it
/// keeps its size in points.
pub fn stitch_pages(src: &Document, pages: &[usize], opts: &StitchOptions) -> Result<(Document, StitchLayout), OrganizeError> {
    if pages.len() > MAX_STITCH_PAGES {
        return Err(OrganizeError::Invalid(format!("{} pages can't go on one page: stitch at most {MAX_STITCH_PAGES} pages at a time", pages.len())));
    }
    let all = walk(src)?;
    check(pages, all.len())?;
    let mut out = Document::new_empty();
    // One form per distinct page, with its size as shown in points.
    let mut forms: HashMap<usize, ShownForm> = HashMap::new();
    let mut placed = Vec::with_capacity(pages.len());
    for &p in pages {
        let form = match forms.get(&p) {
            Some(f) => *f,
            None => {
                let f = shown_form(&mut out, src, &all, p)?;
                forms.insert(p, f);
                f
            }
        };
        placed.push(form);
    }
    let sizes: Vec<(f64, f64)> = placed.iter().map(|f| f.1).collect();
    let layout = stitch_layout(&sizes, opts)?;
    let unit = f64::from(layout.user_unit);
    let mut c = String::from("q\n");
    if layout.user_unit > 1 {
        // Full precision (Display never uses an exponent): six decimals would be off by 2.5% at
        // a unit of 75,000.
        let s = 1.0 / unit;
        c.push_str(&format!("{s} 0 0 {s} 0 0 cm\n"));
    }
    if opts.white {
        c.push_str(&format!("1 g 0 0 {} {} re f\n", num(layout.size.0), num(layout.size.1)));
    }
    let mut xo = Dict::new();
    let mut names: HashMap<ObjRef, String> = HashMap::new();
    for ((form, _, page_unit), (x, y)) in placed.iter().zip(&layout.origins) {
        let next = names.len();
        let name = names.entry(*form).or_insert_with(|| format!("P{next}")).clone();
        xo.set(name.clone().into_bytes(), Object::Ref(*form));
        let s = num(*page_unit);
        c.push_str(&format!("q {s} 0 0 {s} {} {} cm /{name} Do Q\n", num(*x), num(*y)));
    }
    c.push_str("Q\n");
    let contents = out.add(Object::Stream(Stream::flate(Dict::new(), c.as_bytes())));
    let mut res = Dict::new();
    res.set(b"XObject".to_vec(), Object::Dict(xo));
    let mut page = Dict::new();
    page.set(b"Type".to_vec(), Object::name("Page"));
    let media = [0.0, 0.0, layout.size.0 / unit, layout.size.1 / unit];
    page.set(b"MediaBox".to_vec(), Object::Array(media.iter().map(|v| Object::Real((v * 1.0e6).round() / 1.0e6)).collect()));
    if layout.user_unit > 1 {
        page.set(b"UserUnit".to_vec(), Object::Int(i64::from(layout.user_unit)));
    }
    page.set(b"Resources".to_vec(), Object::Dict(res));
    page.set(b"Contents".to_vec(), Object::Ref(contents));
    let page = out.add(Object::Dict(page));
    rebuild(&mut out, &[(page, Dict::new())])?;
    // Pages sharing fonts or images were copied once each: merge the copies.
    let objects: Vec<ObjRef> = out.object_numbers().into_iter().map(|n| ObjRef::new(n, out.generation(n))).collect();
    crate::dedupe::dedupe_resources(&mut out, &objects, false);
    Ok((out, layout))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(direction: StitchDirection, gap: f64, align: StitchAlign) -> StitchOptions {
        StitchOptions { direction, gap, white: true, align }
    }

    #[test]
    fn three_pages_with_a_gap_make_one_long_page() {
        let l = stitch_layout(&[(100.0, 200.0); 3], &opts(StitchDirection::Vertical, 10.0, StitchAlign::Centre)).unwrap();
        assert_eq!(l.size, (100.0, 620.0));
        assert_eq!(l.user_unit, 1);
        assert_eq!(l.origins, [(0.0, 420.0), (0.0, 210.0), (0.0, 0.0)], "first page on top");
        let l = stitch_layout(&[(100.0, 200.0); 3], &opts(StitchDirection::Horizontal, 10.0, StitchAlign::Centre)).unwrap();
        assert_eq!(l.size, (320.0, 200.0));
        assert_eq!(l.origins, [(0.0, 0.0), (110.0, 0.0), (220.0, 0.0)], "first page on the left");
    }

    #[test]
    fn narrower_pages_align_across_the_strip() {
        let sizes = [(100.0, 50.0), (60.0, 30.0)];
        let x = |align| stitch_layout(&sizes, &opts(StitchDirection::Vertical, 0.0, align)).unwrap().origins[1];
        assert_eq!(x(StitchAlign::Start), (0.0, 0.0));
        assert_eq!(x(StitchAlign::Centre), (20.0, 0.0));
        assert_eq!(x(StitchAlign::End), (40.0, 0.0));
        let y = |align| stitch_layout(&sizes, &opts(StitchDirection::Horizontal, 0.0, align)).unwrap().origins[1];
        assert_eq!(y(StitchAlign::Start), (100.0, 20.0), "start is the top edge");
        assert_eq!(y(StitchAlign::Centre), (100.0, 10.0));
        assert_eq!(y(StitchAlign::End), (100.0, 0.0));
    }

    #[test]
    fn a_strip_past_the_viewer_cap_gets_the_smallest_user_unit() {
        let l = stitch_layout(&[(100.0, 200.0); 200], &opts(StitchDirection::Vertical, 10.0, StitchAlign::Centre)).unwrap();
        assert_eq!(l.size, (100.0, 200.0 * 200.0 + 199.0 * 10.0));
        assert_eq!(l.user_unit, 3, "41,990 pt needs a unit of 3 to fit 14,400");
        // Exactly at the cap still fits in one unit.
        let l = stitch_layout(&[(100.0, 7200.0); 2], &opts(StitchDirection::Vertical, 0.0, StitchAlign::Centre)).unwrap();
        assert_eq!(l.user_unit, 1);
    }

    #[test]
    fn bad_input_is_refused_with_what_to_change() {
        let o = StitchOptions::default();
        let err = |sizes: &[(f64, f64)], o: &StitchOptions| match stitch_layout(sizes, o) {
            Err(OrganizeError::Invalid(m)) => m,
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert!(err(&[], &o).contains("choose at least one page"));
        assert!(err(&vec![(10.0, 10.0); MAX_STITCH_PAGES + 1], &o).contains("at most 1000 pages"));
        for gap in [-1.0, 72.5, f64::NAN, f64::INFINITY] {
            assert!(err(&[(10.0, 10.0)], &StitchOptions { gap, ..o }).contains("gap"), "{gap}");
        }
        assert!(err(&[(10.0, 10.0), (0.0, 10.0)], &o).contains("page 2"));
        assert!(err(&[(10.0, f64::NAN)], &o).contains("page 1"));
        assert!(err(&[(10.0, 1.0e300), (10.0, 1.0e300)], &o).contains("too large"));
        assert!(err(&[(10.0, f64::MAX), (10.0, f64::MAX)], &o).contains("too large"), "an overflowing sum");
    }

    /// `n` pages of 100 × 200 pt, each painted with its own grey.
    fn source(n: usize) -> Document {
        let mut doc = Document::new_empty();
        for i in 0..n {
            let page = crate::insert_blank_page(&mut doc, i, 100.0, 200.0).unwrap();
            let grey = (i % 10) as f64 / 10.0;
            let c = doc.add(Object::Stream(Stream::flate(Dict::new(), format!("{grey} g 0 0 100 200 re f").as_bytes())));
            doc.update_dict(page, |d| d.set(b"Contents".to_vec(), Object::Ref(c))).unwrap();
        }
        doc
    }

    fn reopen(doc: &Document) -> Document {
        let bytes = pdfcraft_cos::write_full(doc, &pdfcraft_cos::SaveOptions::default()).unwrap();
        Document::open(std::sync::Arc::new(bytes)).unwrap()
    }

    fn media(doc: &Document) -> Vec<f64> {
        let page = pdfcraft_model::pages(doc).remove(0);
        page.dict.get(b"MediaBox").unwrap().as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect()
    }

    #[test]
    fn stitching_three_pages_with_a_gap_makes_one_100_by_620_page() {
        let src = source(3);
        let (out, layout) = stitch_pages(&src, &[0, 1, 2], &opts(StitchDirection::Vertical, 10.0, StitchAlign::Centre)).unwrap();
        let out = reopen(&out);
        let pages = pdfcraft_model::pages(&out);
        assert_eq!(pages.len(), 1);
        assert_eq!(media(&out), [0.0, 0.0, 100.0, 620.0]);
        assert_eq!(pages[0].display_size(&out), (100.0, 620.0));
        assert_eq!(layout.user_unit, 1);
        assert!(!pages[0].dict.contains(b"UserUnit"));
        let Object::Stream(s) = &*out.resolve(pages[0].dict.get(b"Contents").unwrap()) else { panic!("contents") };
        let content = String::from_utf8(s.decoded().unwrap()).unwrap();
        assert!(content.contains("1 g 0 0 100 620 re f"), "white background: {content}");
        assert!(content.contains("1 0 0 1 0 420 cm /P0 Do") && content.contains("1 0 0 1 0 0 cm /P2 Do"), "{content}");
        // No background, and a page used twice draws one form twice.
        let (out, _) = stitch_pages(&src, &[1, 1], &StitchOptions { white: false, ..StitchOptions::default() }).unwrap();
        let out = reopen(&out);
        let page = pdfcraft_model::pages(&out).remove(0);
        let Object::Stream(s) = &*out.resolve(page.dict.get(b"Contents").unwrap()) else { panic!("contents") };
        let content = String::from_utf8(s.decoded().unwrap()).unwrap();
        assert!(!content.contains(" re f"), "{content}");
        assert_eq!(content.matches("/P0 Do").count(), 2, "{content}");
        assert_eq!(media(&out), [0.0, 0.0, 100.0, 400.0]);
        assert_eq!(stitch_pages(&src, &[0, 3], &StitchOptions::default()).map(|_| ()), Err(OrganizeError::NoSuchPage(3)));
    }

    #[test]
    fn two_hundred_pages_get_a_user_unit_and_keep_their_size_in_points() {
        let src = source(200);
        let all: Vec<usize> = (0..200).collect();
        let (out, layout) = stitch_pages(&src, &all, &opts(StitchDirection::Vertical, 10.0, StitchAlign::Centre)).unwrap();
        assert_eq!(layout.user_unit, 3);
        let out = reopen(&out);
        let page = pdfcraft_model::pages(&out).remove(0);
        assert_eq!(page.dict.get(b"UserUnit").and_then(Object::as_int), Some(3));
        let m = media(&out);
        assert!(m[3] <= MAX_PAGE_SIDE && m[2] <= MAX_PAGE_SIDE, "{m:?}");
        let (w, h) = page.display_size(&out);
        assert!((w - 100.0).abs() < 0.01 && (h - 41_990.0).abs() < 0.01, "{w} × {h}");
        assert_eq!(
            stitch_pages(&src, &vec![0; MAX_STITCH_PAGES + 1], &StitchOptions::default()).map(|_| ()).unwrap_err().to_string(),
            "1001 pages can't go on one page: stitch at most 1000 pages at a time"
        );
    }

    #[test]
    fn a_turned_page_with_its_own_user_unit_keeps_its_shown_size() {
        let mut src = source(2);
        let first = crate::pages(&src).unwrap()[0].obj;
        src.update_dict(first, |d| {
            d.set(b"Rotate".to_vec(), Object::Int(90));
            d.set(b"UserUnit".to_vec(), Object::Int(2));
        })
        .unwrap();
        let (out, layout) = stitch_pages(&src, &[0, 1], &opts(StitchDirection::Horizontal, 0.0, StitchAlign::End)).unwrap();
        // Page 1 shows 400 × 200 pt; page 2 is 100 × 200 at the bottom.
        assert_eq!(layout.size, (500.0, 200.0));
        assert_eq!(layout.origins, [(0.0, 0.0), (400.0, 0.0)]);
        let out = reopen(&out);
        let page = pdfcraft_model::pages(&out).remove(0);
        let Object::Stream(s) = &*out.resolve(page.dict.get(b"Contents").unwrap()) else { panic!("contents") };
        assert!(String::from_utf8(s.decoded().unwrap()).unwrap().contains("q 2 0 0 2 0 0 cm /P0 Do Q"));
    }
}
