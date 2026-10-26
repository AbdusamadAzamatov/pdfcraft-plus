//! Collage: chosen pages arranged on sheets of paper, a grid of them per sheet, as a new
//! document; for instance an identity card's front and back on one A4 sheet at their real size,
//! or four photographed receipts on a page. Each page is drawn from a Form XObject copy of it
//! ([`crate::import::page_as_form`]), placed with `cm`, so the result is vector and lossless.
//! Only what the pages draw comes over: links, comments and form fields are not carried.
//!
//! At actual size the pages keep their size in points (shrunk only when one would not fit its
//! share of the sheet) and are packed into a block centred on the sheet: each column as wide as
//! its widest page, each row as tall as its tallest. Fitted, the sheet inside its margins is cut
//! into equal cells and each page is scaled, larger or smaller, to fill its cell.

use std::collections::HashMap;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::stitch::{ShownForm, shown_form};
use crate::transform::num;
use crate::{OrganizeError, check, rebuild, walk};

/// The most pages one collage may hold (on as many sheets as they need).
pub const MAX_COLLAGE_PAGES: usize = 1_000;
/// The most columns and the most rows on a sheet.
pub const MAX_COLLAGE_GRID: usize = 6;
/// The widest margin and the widest gap between pages, in points.
pub const MAX_COLLAGE_MARGIN: f64 = 144.0;
pub const MAX_COLLAGE_GAP: f64 = 72.0;
/// ISO A4 and US Letter, portrait, in points.
pub const A4: (f64, f64) = (595.276, 841.89);
pub const LETTER: (f64, f64) = (612.0, 792.0);

/// How to make a collage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CollageOptions {
    /// Columns and rows of pages on each sheet, 1 to [`MAX_COLLAGE_GRID`].
    pub cols: usize,
    pub rows: usize,
    /// The sheet's width and height in points.
    pub sheet: (f64, f64),
    /// Keep the pages' real size (shrunk only to fit); otherwise scale each to fill its cell.
    pub actual_size: bool,
    /// Space kept clear at the sheet's edges, and between neighbouring pages, in points.
    pub margin: f64,
    pub gap: f64,
}

impl CollageOptions {
    /// An identity card's front and back, one above the other at their real size on A4.
    pub fn id_card() -> Self {
        CollageOptions { cols: 1, rows: 2, sheet: A4, actual_size: true, margin: 36.0, gap: 24.0 }
    }

    /// The pages on each sheet.
    pub fn per_sheet(&self) -> usize {
        self.cols.saturating_mul(self.rows)
    }
}

impl Default for CollageOptions {
    fn default() -> Self {
        Self::id_card()
    }
}

/// One page on a sheet: which of the chosen pages (an index into them), where its bottom-left
/// corner goes (points from the sheet's bottom-left) and how much it is scaled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub page: usize,
    pub origin: (f64, f64),
    pub scale: f64,
}

/// What goes on each sheet, sheet by sheet.
pub type Sheets = Vec<Vec<Placed>>;

/// Lay out pages of these sizes (as shown, in points), in order, filling each sheet's grid left
/// to right, then top to bottom: what goes on each sheet.
pub fn collage_layout(sizes: &[(f64, f64)], opts: &CollageOptions) -> Result<Sheets, OrganizeError> {
    let bad = |m: String| Err(OrganizeError::Invalid(m));
    if sizes.is_empty() {
        return bad("there are no pages for the collage: choose at least one page".into());
    }
    if sizes.len() > MAX_COLLAGE_PAGES {
        return bad(format!("{} pages are too many for one collage: use at most {MAX_COLLAGE_PAGES}", sizes.len()));
    }
    if !(1..=MAX_COLLAGE_GRID).contains(&opts.cols) || !(1..=MAX_COLLAGE_GRID).contains(&opts.rows) {
        return bad(format!("a sheet holds 1 to {MAX_COLLAGE_GRID} columns and 1 to {MAX_COLLAGE_GRID} rows of pages"));
    }
    if !(opts.margin.is_finite() && (0.0..=MAX_COLLAGE_MARGIN).contains(&opts.margin)) {
        return bad(format!("the margin must be from 0 to {MAX_COLLAGE_MARGIN} pt (got {})", opts.margin));
    }
    if !(opts.gap.is_finite() && (0.0..=MAX_COLLAGE_GAP).contains(&opts.gap)) {
        return bad(format!("the gap must be from 0 to {MAX_COLLAGE_GAP} pt (got {})", opts.gap));
    }
    let (sw, sh) = opts.sheet;
    if !(sw.is_finite()
        && sh.is_finite()
        && (72.0..=crate::resize::MAX_PAGE_SIDE).contains(&sw)
        && (72.0..=crate::resize::MAX_PAGE_SIDE).contains(&sh))
    {
        return bad("the sheet must be from 1 to 200 inches on each side".into());
    }
    for (i, (w, h)) in sizes.iter().enumerate() {
        if !(w.is_finite() && h.is_finite() && *w > 0.0 && *h > 0.0) {
            return bad(format!("page {} has no visible area, so it can't go in a collage; leave it out", i.saturating_add(1)));
        }
    }
    let (cols, rows) = (opts.cols as f64, opts.rows as f64);
    // Equal cells inside the margins.
    let cell_w = (sw - 2.0 * opts.margin - opts.gap * (cols - 1.0)) / cols;
    let cell_h = (sh - 2.0 * opts.margin - opts.gap * (rows - 1.0)) / rows;
    if !(cell_w >= 1.0 && cell_h >= 1.0) {
        return bad("the margins and gaps leave no room for the pages: make them smaller or use fewer columns or rows".into());
    }
    let fit = |(w, h): (f64, f64)| (cell_w / w).min(cell_h / h);
    let mut sheets = Vec::with_capacity(sizes.len().div_ceil(opts.per_sheet()));
    for (k, chunk) in sizes.chunks(opts.per_sheet()).enumerate() {
        let first = k * opts.per_sheet();
        let mut placed = Vec::with_capacity(chunk.len());
        if opts.actual_size {
            // Real size, shrunk to the cell only if needed; then a compact block in the middle.
            let scales: Vec<f64> = chunk.iter().map(|&s| fit(s).min(1.0)).collect();
            let used_cols = chunk.len().min(opts.cols);
            let used_rows = chunk.len().div_ceil(opts.cols);
            let mut col_w = vec![0.0f64; used_cols];
            let mut row_h = vec![0.0f64; used_rows];
            for (i, ((w, h), s)) in chunk.iter().zip(&scales).enumerate() {
                if let Some(c) = col_w.get_mut(i % opts.cols) {
                    *c = c.max(w * s);
                }
                if let Some(r) = row_h.get_mut(i / opts.cols) {
                    *r = r.max(h * s);
                }
            }
            let block_w = col_w.iter().sum::<f64>() + opts.gap * (used_cols as f64 - 1.0);
            let block_h = row_h.iter().sum::<f64>() + opts.gap * (used_rows as f64 - 1.0);
            let (left, top) = ((sw - block_w) / 2.0, (sh + block_h) / 2.0);
            for (i, ((w, h), s)) in chunk.iter().zip(&scales).enumerate() {
                let (c, r) = (i % opts.cols, i / opts.cols);
                let x = left + col_w.iter().take(c).map(|v| v + opts.gap).sum::<f64>();
                let y_top = top - row_h.iter().take(r).map(|v| v + opts.gap).sum::<f64>();
                let (cw, rh) = (col_w.get(c).copied().unwrap_or(0.0), row_h.get(r).copied().unwrap_or(0.0));
                // Centred in its column and row.
                let origin = (x + (cw - w * s) / 2.0, y_top - rh + (rh - h * s) / 2.0);
                placed.push(Placed { page: first + i, origin, scale: *s });
            }
        } else {
            for (i, &(w, h)) in chunk.iter().enumerate() {
                let (c, r) = ((i % opts.cols) as f64, (i / opts.cols) as f64);
                let s = fit((w, h));
                let cell_x = opts.margin + c * (cell_w + opts.gap);
                let cell_top = sh - opts.margin - r * (cell_h + opts.gap);
                let origin = (cell_x + (cell_w - w * s) / 2.0, cell_top - cell_h + (cell_h - h * s) / 2.0);
                placed.push(Placed { page: first + i, origin, scale: s });
            }
        }
        sheets.push(placed);
    }
    Ok(sheets)
}

/// A new document of sheets showing `pages` (0-based, in this order; a page may repeat) of
/// `src` as they are displayed, laid out by [`collage_layout`]. The pages' content is copied;
/// their annotations (links, comments, form fields) are not. Returns it with the layout.
pub fn collage_pages(src: &Document, pages: &[usize], opts: &CollageOptions) -> Result<(Document, Sheets), OrganizeError> {
    if pages.len() > MAX_COLLAGE_PAGES {
        return Err(OrganizeError::Invalid(format!("{} pages are too many for one collage: use at most {MAX_COLLAGE_PAGES}", pages.len())));
    }
    let all = walk(src)?;
    check(pages, all.len())?;
    let mut out = Document::new_empty();
    let mut forms: HashMap<usize, ShownForm> = HashMap::new();
    let mut chosen = Vec::with_capacity(pages.len());
    for &p in pages {
        let form = match forms.get(&p) {
            Some(f) => *f,
            None => {
                let f = shown_form(&mut out, src, &all, p)?;
                forms.insert(p, f);
                f
            }
        };
        chosen.push(form);
    }
    let sizes: Vec<(f64, f64)> = chosen.iter().map(|f| f.1).collect();
    let sheets = collage_layout(&sizes, opts)?;
    let mut order = Vec::with_capacity(sheets.len());
    for sheet in &sheets {
        let mut c = String::new();
        let mut xo = Dict::new();
        let mut names: HashMap<ObjRef, String> = HashMap::new();
        for p in sheet {
            let Some((form, _, unit)) = chosen.get(p.page) else { continue };
            let next = names.len();
            let name = names.entry(*form).or_insert_with(|| format!("P{next}")).clone();
            xo.set(name.clone().into_bytes(), Object::Ref(*form));
            let s = num(p.scale * unit);
            c.push_str(&format!("q {s} 0 0 {s} {} {} cm /{name} Do Q\n", num(p.origin.0), num(p.origin.1)));
        }
        let contents = out.add(Object::Stream(Stream::flate(Dict::new(), c.as_bytes())));
        let mut res = Dict::new();
        res.set(b"XObject".to_vec(), Object::Dict(xo));
        let mut page = Dict::new();
        page.set(b"Type".to_vec(), Object::name("Page"));
        let media = [0.0, 0.0, opts.sheet.0, opts.sheet.1];
        page.set(b"MediaBox".to_vec(), Object::Array(media.iter().map(|v| Object::Real((v * 1000.0).round() / 1000.0)).collect()));
        page.set(b"Resources".to_vec(), Object::Dict(res));
        page.set(b"Contents".to_vec(), Object::Ref(contents));
        order.push((out.add(Object::Dict(page)), Dict::new()));
    }
    rebuild(&mut out, &order)?;
    // Pages sharing fonts or images were copied once each: merge the copies.
    let objects: Vec<ObjRef> = out.object_numbers().into_iter().map(|n| ObjRef::new(n, out.generation(n))).collect();
    crate::dedupe::dedupe_resources(&mut out, &objects, false);
    Ok((out, sheets))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A credit-card-sized card (85.6 × 54 mm) in points.
    const CARD: (f64, f64) = (242.65, 153.07);

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01
    }

    #[test]
    fn an_id_card_front_and_back_sit_together_in_the_middle_of_a4_at_real_size() {
        let sheets = collage_layout(&[CARD, CARD], &CollageOptions::id_card()).unwrap();
        assert_eq!(sheets.len(), 1);
        let s = &sheets[0];
        assert!(s.iter().all(|p| p.scale == 1.0), "real size");
        // A block 242.65 wide and 2 × 153.07 + 24 tall, centred on the sheet.
        let x = (A4.0 - CARD.0) / 2.0;
        let top = (A4.1 + 2.0 * CARD.1 + 24.0) / 2.0;
        assert!(close(s[0].origin, (x, top - CARD.1)), "front on top: {:?}", s[0].origin);
        assert!(close(s[1].origin, (x, top - 2.0 * CARD.1 - 24.0)), "back below: {:?}", s[1].origin);
        // Three cards: a second sheet with the third, again in the middle.
        let sheets = collage_layout(&[CARD; 3], &CollageOptions::id_card()).unwrap();
        assert_eq!(sheets.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(sheets[1][0].page, 2);
        assert!(close(sheets[1][0].origin, ((A4.0 - CARD.0) / 2.0, (A4.1 - CARD.1) / 2.0)));
    }

    #[test]
    fn a_page_too_large_for_its_share_is_shrunk_at_actual_size() {
        let o = CollageOptions { cols: 2, rows: 1, ..CollageOptions::id_card() };
        let sheets = collage_layout(&[A4, CARD], &o).unwrap();
        // Each cell is (595.276 - 72 - 24) / 2 wide: a whole A4 page has to shrink, the card doesn't.
        let cell = (A4.0 - 72.0 - 24.0) / 2.0;
        assert!((sheets[0][0].scale - cell / A4.0).abs() < 1e-9);
        assert_eq!(sheets[0][1].scale, 1.0);
    }

    #[test]
    fn fitted_pages_fill_equal_cells_in_reading_order() {
        let o = CollageOptions { cols: 2, rows: 2, sheet: (600.0, 800.0), actual_size: false, margin: 50.0, gap: 20.0 };
        // Cells are 240 × 340. A 120 × 170 page doubles; a 480 × 340 one halves.
        let sheets = collage_layout(&[(120.0, 170.0), (480.0, 340.0), (240.0, 340.0), (240.0, 340.0), (100.0, 100.0)], &o).unwrap();
        assert_eq!(sheets.iter().map(Vec::len).collect::<Vec<_>>(), [4, 1]);
        let s = &sheets[0];
        assert_eq!((s[0].scale, s[1].scale), (2.0, 0.5));
        assert!(close(s[0].origin, (50.0, 410.0)), "top left: {:?}", s[0].origin);
        // Top right, halved to 240 × 170, centred down its cell.
        assert!(close(s[1].origin, (310.0, 410.0 + 85.0)), "{:?}", s[1].origin);
        assert!(close(s[2].origin, (50.0, 50.0)) && close(s[3].origin, (310.0, 50.0)), "the second row");
        // The fifth page starts the next sheet, top left, scaled up to its cell's width.
        assert_eq!((sheets[1][0].page, sheets[1][0].scale), (4, 2.4));
    }

    #[test]
    fn bad_input_is_refused_with_what_to_change() {
        let o = CollageOptions::id_card();
        let err = |sizes: &[(f64, f64)], o: &CollageOptions| match collage_layout(sizes, o) {
            Err(OrganizeError::Invalid(m)) => m,
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert!(err(&[], &o).contains("at least one page"));
        assert!(err(&vec![CARD; MAX_COLLAGE_PAGES + 1], &o).contains("at most 1000"));
        for (cols, rows) in [(0, 1), (1, 0), (7, 1), (1, 7)] {
            assert!(err(&[CARD], &CollageOptions { cols, rows, ..o }).contains("columns"), "{cols} × {rows}");
        }
        for margin in [-1.0, 145.0, f64::NAN] {
            assert!(err(&[CARD], &CollageOptions { margin, ..o }).contains("margin"), "{margin}");
        }
        for gap in [-1.0, 73.0, f64::INFINITY] {
            assert!(err(&[CARD], &CollageOptions { gap, ..o }).contains("gap"), "{gap}");
        }
        for sheet in [(10.0, 800.0), (600.0, f64::NAN), (20_000.0, 800.0)] {
            assert!(err(&[CARD], &CollageOptions { sheet, ..o }).contains("sheet"), "{sheet:?}");
        }
        assert!(err(&[CARD, (0.0, 5.0)], &o).contains("page 2"));
        let cramped = CollageOptions { cols: 6, rows: 6, sheet: (100.0, 100.0), margin: 40.0, gap: 10.0, actual_size: false };
        assert!(err(&[CARD], &cramped).contains("no room"));
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

    #[test]
    fn five_pages_two_by_two_make_two_sheets_drawing_each_page_once() {
        let src = source(3);
        let o = CollageOptions { cols: 2, rows: 2, sheet: LETTER, actual_size: true, margin: 36.0, gap: 12.0 };
        let (out, sheets) = collage_pages(&src, &[0, 1, 2, 0, 1], &o).unwrap();
        assert_eq!(sheets.len(), 2);
        let bytes = pdfcraft_cos::write_full(&out, &pdfcraft_cos::SaveOptions::default()).unwrap();
        let out = Document::open(std::sync::Arc::new(bytes)).unwrap();
        let pages = pdfcraft_model::pages(&out);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].display_size(&out), LETTER);
        let content = |i: usize| {
            let Object::Stream(s) = &*out.resolve(pages[i].dict.get(b"Contents").unwrap()) else { panic!("contents") };
            String::from_utf8(s.decoded().unwrap()).unwrap()
        };
        // The first sheet: pages 1, 2, 3, then page 1 again, from one form per source page.
        let first = content(0);
        assert_eq!(first.matches(" Do Q").count(), 4, "{first}");
        assert_eq!(first.matches("/P0 Do").count(), 2, "page 1 twice from one form: {first}");
        assert_eq!(content(1).matches(" Do Q").count(), 1);
        assert_eq!(collage_pages(&src, &[0, 3], &o).map(|_| ()), Err(OrganizeError::NoSuchPage(3)));
        assert!(collage_pages(&src, &vec![0; MAX_COLLAGE_PAGES + 1], &o).is_err());
    }
}
