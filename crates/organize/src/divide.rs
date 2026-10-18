//! Divide pages: split each page into a grid of equal parts, each part a page of its own
//! (execution plan PL4). Lossless: every part shares the original's content and resources and
//! only shows its cell, through its crop box (ISO 32000-2 §14.11.2).

use pdfcraft_cos::{Dict, Document, ObjRef, Object};
use pdfcraft_model::{Page, view_matrix_for};

use crate::{INHERITABLE, OrganizeError, check, rebuild, walk};

/// The most columns, and the most rows, a page can be divided into.
pub const MAX_DIVIDE: u32 = 8;

/// The most pages a divided document may have: bounds what one edit adds.
const MAX_RESULT_PAGES: usize = 200_000;

/// Smallest side of a part, in user-space units: a part must stay a usable page.
const MIN_PART: f64 = 1.0;

/// Page entries a part copy must not repeat: its thumbnail shows the whole page, an article
/// bead belongs to one page, and the structure tree's parent tree maps one page's marked content.
const NOT_ON_COPIES: [&[u8]; 4] = [b"Annots", b"Thumb", b"B", b"StructParents"];

/// Divide the pages at `indices` (0-based) into `cols` × `rows` equal parts, in place.
///
/// Each part is a page showing one cell of the page as displayed (after `/Rotate`): rows from
/// top to bottom, and within a row columns from left to right, or right to left when `rtl` (for
/// right-to-left books and manga). The original page object becomes the first part, so bookmarks,
/// links and named destinations that opened the page open its first part; the other parts follow
/// it, sharing its contents and resources. The trim, bleed and art boxes are clipped to each part
/// and dropped where nothing is left of them. Every annotation moves to the one part holding most
/// of its rectangle (a popup goes with its parent): an annotation is never shared between pages.
pub fn divide_pages(doc: &mut Document, indices: &[usize], cols: u32, rows: u32, rtl: bool) -> Result<(), OrganizeError> {
    if !(1..=MAX_DIVIDE).contains(&cols) || !(1..=MAX_DIVIDE).contains(&rows) {
        return Err(OrganizeError::Invalid(format!("columns and rows must each be from 1 to {MAX_DIVIDE} (got {cols} × {rows})")));
    }
    if cols == 1 && rows == 1 {
        return Err(OrganizeError::Invalid("1 column × 1 row leaves the pages as they are: choose more columns or rows".into()));
    }
    if indices.is_empty() {
        return Err(OrganizeError::Invalid("no pages to divide".into()));
    }
    let all = walk(doc)?;
    check(indices, all.len())?;
    let mut selected = indices.to_vec();
    selected.sort_unstable();
    selected.dedup();
    // Bounded by MAX_DIVIDE², so no overflow.
    let parts = (cols * rows) as usize;
    let added = selected.len().checked_mul(parts - 1).ok_or_else(too_many)?;
    if all.len().checked_add(added).is_none_or(|n| n > MAX_RESULT_PAGES) {
        return Err(too_many());
    }

    // Plan every page first, so a refusal changes nothing.
    let mut plans = Vec::with_capacity(selected.len());
    for &i in &selected {
        let Some((obj, inherited)) = all.get(i) else { return Err(OrganizeError::NoSuchPage(i)) };
        let own = doc.get(*obj).as_dict().cloned().unwrap_or_default();
        let mut effective = own.clone();
        for k in INHERITABLE {
            if !effective.contains(k)
                && let Some(v) = inherited.get(k)
            {
                effective.set(k.to_vec(), v.clone());
            }
        }
        let page = Page { obj: *obj, dict: effective.clone() };
        let crop = page.crop(doc);
        let cells = cells(crop, page.rotation(doc), cols, rows, rtl)
            .ok_or_else(|| OrganizeError::Invalid(format!("page {} is too small to divide into {cols} × {rows} parts", i.saturating_add(1))))?;
        let owners = annotation_owners(doc, &own, &cells);
        plans.push((i, *obj, effective, cells, owners));
    }

    let mut order: Vec<(ObjRef, Dict)> = Vec::with_capacity(all.len().saturating_add(added));
    let mut plans = plans.into_iter().peekable();
    for (i, (obj, inherited)) in all.iter().enumerate() {
        order.push((*obj, inherited.clone()));
        let Some((_, page, effective, cells, owners)) = plans.next_if(|p| p.0 == i) else { continue };
        let mut part_refs = vec![page];
        for _ in 1..cells.len() {
            part_refs.push(doc.add(Object::Null));
        }
        // Move each annotation to its part, pointing its /P there.
        let mut lists: Vec<Vec<Object>> = vec![Vec::new(); cells.len()];
        for (annot, part) in owners {
            let target = part_refs.get(part).copied().unwrap_or(page);
            let moved = match annot {
                Object::Ref(r) => {
                    if part != 0 && doc.get(r).as_dict().is_some_and(|d| d.contains(b"P")) {
                        doc.update_dict(r, |d| d.set(b"P".to_vec(), Object::Ref(target)))?;
                    }
                    Object::Ref(r)
                }
                Object::Dict(mut d) => {
                    if part != 0 && d.contains(b"P") {
                        d.set(b"P".to_vec(), Object::Ref(target));
                    }
                    Object::Dict(d)
                }
                other => other,
            };
            if let Some(list) = lists.get_mut(part) {
                list.push(moved);
            }
        }
        for (k, ((cell, list), part)) in cells.iter().zip(lists).zip(&part_refs).enumerate() {
            let mut d = if k == 0 { doc.get(page).as_dict().cloned().unwrap_or_default() } else { effective.clone() };
            if k == 0 {
                d.remove(b"Thumb");
            } else {
                for key in NOT_ON_COPIES {
                    d.remove(key);
                }
            }
            d.set(b"CropBox".to_vec(), rect_object(*cell));
            for key in [b"BleedBox".as_slice(), b"TrimBox", b"ArtBox"] {
                let clipped = rect_of(doc, effective.get(key)).and_then(|r| intersect(r, *cell));
                match clipped {
                    Some(r) => d.set(key.to_vec(), rect_object(r)),
                    None => {
                        d.remove(key);
                    }
                }
            }
            if list.is_empty() {
                d.remove(b"Annots");
            } else {
                d.set(b"Annots".to_vec(), Object::Array(list));
            }
            doc.set(*part, Object::Dict(d));
            if k > 0 {
                // A copy carries its inherited attributes itself.
                order.push((*part, Dict::new()));
            }
        }
    }
    rebuild(doc, &order)
}

fn too_many() -> OrganizeError {
    OrganizeError::Invalid(format!("dividing these pages would make more than {MAX_RESULT_PAGES} pages: divide fewer pages or into fewer parts"))
}

/// The cells of `crop` (user space) shown turned by `rotation`, in reading order, in user space;
/// `None` when a cell would be smaller than [`MIN_PART`].
fn cells(crop: [f64; 4], rotation: i64, cols: u32, rows: u32, rtl: bool) -> Option<Vec<[f64; 4]>> {
    let (w, h) = (crop[2] - crop[0], crop[3] - crop[1]);
    let (dw, dh) = if rotation % 180 == 0 { (w, h) } else { (h, w) };
    let (cw, ch) = (dw / f64::from(cols), dh / f64::from(rows));
    if !(cw.is_finite() && ch.is_finite() && cw >= MIN_PART && ch >= MIN_PART) {
        return None;
    }
    // Display space: origin at the bottom-left of the page as shown, y up.
    let m = view_matrix_for(rotation, crop);
    let map = |x: f64, y: f64| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
    let edge = |n: u32, size: f64, of: u32| if n == of { size } else { size * f64::from(n) / f64::from(of) };
    let mut out = Vec::with_capacity((cols * rows) as usize);
    for row in 0..rows {
        // Rows run from the top of the displayed page.
        let (y0, y1) = (dh - edge(row + 1, dh, rows), dh - edge(row, dh, rows));
        for k in 0..cols {
            let col = if rtl { cols - 1 - k } else { k };
            let (x0, x1) = (edge(col, dw, cols), edge(col + 1, dw, cols));
            let (ax, ay) = map(x0, y0);
            let (bx, by) = map(x1, y1);
            out.push([ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)]);
        }
    }
    Some(out)
}

/// Each of the page's annotations (as listed in its /Annots) with the part it goes to: the part
/// whose cell holds most of its /Rect (the first on a tie, or when it has none or lies outside
/// every cell); a popup goes where its parent goes.
fn annotation_owners(doc: &Document, page: &Dict, cells: &[[f64; 4]]) -> Vec<(Object, usize)> {
    let list = page.get(b"Annots").map(|a| doc.resolve(a).as_array().cloned().unwrap_or_default()).unwrap_or_default();
    let mut owners: Vec<(Object, usize)> = list
        .into_iter()
        .map(|a| {
            let rect = doc.resolve(&a).as_dict().and_then(|d| rect_of(doc, d.get(b"Rect")));
            let part = rect.map_or(0, |r| best_cell(r, cells));
            (a, part)
        })
        .collect();
    let refs: Vec<(Option<ObjRef>, usize)> = owners.iter().map(|(a, p)| (a.as_ref(), *p)).collect();
    for (a, part) in &mut owners {
        let resolved = doc.resolve(a);
        let Some(d) = resolved.as_dict() else { continue };
        if d.name(b"Subtype") != Some(b"Popup") {
            continue;
        }
        let Some(parent) = d.reference(b"Parent") else { continue };
        if let Some((_, p)) = refs.iter().find(|(r, _)| *r == Some(parent)) {
            *part = *p;
        }
    }
    owners
}

fn best_cell(rect: [f64; 4], cells: &[[f64; 4]]) -> usize {
    let mut best = (0, 0.0);
    for (k, cell) in cells.iter().enumerate() {
        let area = intersect(rect, *cell).map_or(0.0, |r| (r[2] - r[0]) * (r[3] - r[1]));
        if area > best.1 {
            best = (k, area);
        }
    }
    best.0
}

fn rect_of(doc: &Document, o: Option<&Object>) -> Option<[f64; 4]> {
    let o = doc.resolve(o?);
    let v: Vec<f64> = o.as_array()?.iter().filter_map(|x| doc.resolve(x).as_f64()).collect();
    match v.as_slice() {
        [a, b, c, d] if v.iter().all(|x| x.is_finite()) => Some([a.min(*c), b.min(*d), a.max(*c), b.max(*d)]),
        _ => None,
    }
}

/// The overlap of two rectangles, `None` when it has no area.
fn intersect(a: [f64; 4], b: [f64; 4]) -> Option<[f64; 4]> {
    let r = [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])];
    (r[2] > r[0] && r[3] > r[1]).then_some(r)
}

fn rect_object(r: [f64; 4]) -> Object {
    Object::Array(r.iter().map(|v| Object::Real(*v)).collect())
}
