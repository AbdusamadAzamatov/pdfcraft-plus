//! Page content transforms: rotate a page's content by any angle (execution plan PL2), and the
//! reusable helper underneath it, [`transform_page`], which change page size (PL7) and deskew
//! (PL9) build on.
//!
//! A transform never rewrites the page's existing content streams. It wraps them: a new stream
//! `q <m> cm <old crop box> re W n` goes first in `/Contents` and a new stream `Q` goes last.
//! Incremental saves therefore stay small, and signatures over other pages stay valid.

use std::collections::HashSet;

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::boxes::{boxes_of, intersect, own_box};
use crate::{OrganizeError, check, resolved_rotation, rotate_pages, walk};

/// The smallest page side a transform may produce, in points.
const MIN_SIDE: f64 = 1.0;
/// The largest coordinate a transformed page box may have, in points (far beyond any real page;
/// it keeps absurd numbers out of the file).
const MAX_COORD: f64 = 1.0e7;
/// Annotation flags (ISO 32000-2 Table 167) that keep an annotation from being drawn.
const HIDDEN: i64 = 2;
const NO_VIEW: i64 = 32;

/// How [`rotate_content`] fits the turned content on the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RotateFit {
    /// The page grows to the bounding box of the turned page, so nothing is cut off.
    #[default]
    Expand,
    /// The page keeps its size and the turned content is scaled down until it fits.
    KeepSize,
    /// The page keeps its size; the corners of the turned content are cut off.
    Crop,
}

impl RotateFit {
    pub const ALL: [RotateFit; 3] = [RotateFit::Expand, RotateFit::KeepSize, RotateFit::Crop];

    /// The name used by the automation tool: `expand`, `keep_size` or `crop`.
    pub fn name(self) -> &'static str {
        match self {
            RotateFit::Expand => "expand",
            RotateFit::KeepSize => "keep_size",
            RotateFit::Crop => "crop",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        RotateFit::ALL.into_iter().find(|f| f.name() == s)
    }
}

fn invalid(msg: impl Into<String>) -> OrganizeError {
    OrganizeError::Invalid(msg.into())
}

/// `[x0 y0 x1 y1]` with `x0 ≤ x1` and `y0 ≤ y1`.
fn normalised(r: [f64; 4]) -> [f64; 4] {
    [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])]
}

fn big_enough(r: [f64; 4]) -> bool {
    r[2] - r[0] >= MIN_SIDE && r[3] - r[1] >= MIN_SIDE
}

/// A number for the file: rounded to a millionth of a point, which is far below what any device
/// shows and keeps float noise (`99.99999999997`) out of the file.
fn real(v: f64) -> Object {
    let r = (v * 1.0e6).round() / 1.0e6;
    Object::Real(if r.is_finite() { r } else { v })
}

fn rect_object(r: [f64; 4]) -> Object {
    Object::Array(r.iter().map(|v| real(*v)).collect())
}

/// A content-stream number: up to six decimals, without trailing zeros.
fn num(v: f64) -> String {
    let r = (v * 1.0e6).round() / 1.0e6;
    if r == 0.0 || !r.is_finite() {
        return "0".into();
    }
    let s = format!("{r:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Transform the content of page `index` (0-based) by the matrix `m` and give it the media box
/// `new_media`.
///
/// - `m` is a PDF matrix `[a b c d e f]` (ISO 32000-2 §8.3.4) mapping the page's current default
///   user space to its new one: a point `(x, y)` goes to `(a·x + c·y + e, b·x + d·y + f)`. It
///   must be finite and invertible.
/// - `new_media` is `[x0 y0 x1 y1]` in the new user space (points); it is normalised and must be
///   finite, at least 1 pt on each side, and within ±10⁷ pt.
///
/// What changes:
/// - **Content.** The existing content streams are kept as they are, byte for byte. A new first
///   stream `q <m> cm <old crop box> re W n` and a new last stream `Q` wrap them, so what used to
///   be visible (the old effective crop box) is what gets transformed: the media area outside the
///   old crop box stays hidden.
/// - **Boxes.** MediaBox and CropBox are both set on the page to `new_media`. The page's own
///   TrimBox, BleedBox and ArtBox become the bounding box of their transformed rectangle,
///   clipped to `new_media`, and are removed when nothing of them is left (they then default
///   to the crop box). `/Rotate` is left alone.
/// - **Annotations.** Each annotation's `/Rect` becomes the axis-aligned bounding box of the
///   transformed rectangle; `/QuadPoints`, `/InkList`, `/Vertices`, `/L` and `/CL` points are
///   transformed exactly. Appearance streams are not touched: a viewer maps them onto the new
///   `/Rect` (Algorithm 8.1), which is exact for a scale plus translation (PL7), but for a turn
///   that is not a multiple of 90° the appearance would be stretched over the bounding box.
///   Callers that rotate should refuse pages for which [`page_has_drawn_annotations`] is true
///   (as [`rotate_content`] does) or flatten them first.
///
/// Errors (and nothing changes): [`OrganizeError::NoSuchPage`], or [`OrganizeError::Invalid`]
/// for a matrix or box that is not usable, an empty crop box, or a damaged `/Contents`.
pub fn transform_page(doc: &mut Document, index: usize, m: [f64; 6], new_media: [f64; 4]) -> Result<(), OrganizeError> {
    let all = walk(doc)?;
    check(&[index], all.len())?;
    let (page, inherited) = all.get(index).ok_or(OrganizeError::NoSuchPage(index))?;
    let crop = boxes_of(doc, *page, inherited)[1];
    apply(doc, index, *page, crop, Matrix(m), new_media)
}

/// True when page `index` (0-based) has an annotation a viewer draws that is not a link or a
/// pop-up: a comment or a form field (or a watermark, 3D or media annotation) that is neither
/// hidden nor marked NoView. Their appearances can follow a scale plus translation of the page,
/// but not a turn by an angle that isn't a multiple of 90°.
pub fn page_has_drawn_annotations(doc: &Document, index: usize) -> Result<bool, OrganizeError> {
    Ok(!pages_with_drawn_annotations(doc, &[index])?.is_empty())
}

/// Of `indices` (0-based), the pages for which [`page_has_drawn_annotations`] is true, in the
/// order given, reading the page tree once.
pub fn pages_with_drawn_annotations(doc: &Document, indices: &[usize]) -> Result<Vec<usize>, OrganizeError> {
    let all = walk(doc)?;
    check(indices, all.len())?;
    Ok(indices.iter().copied().filter(|i| all.get(*i).is_some_and(|(page, _)| drawn_annotations(doc, *page))).collect())
}

fn drawn_annotations(doc: &Document, page: ObjRef) -> bool {
    annotations(doc, page).iter().any(|entry| {
        let obj = doc.resolve(entry);
        let Some(a) = obj.as_dict() else { return false };
        let flags = a.get(b"F").and_then(|f| doc.resolve(f).as_int()).unwrap_or(0);
        match a.name(b"Subtype") {
            None | Some(b"Link" | b"Popup") => false,
            Some(_) => flags & (HIDDEN | NO_VIEW) == 0,
        }
    })
}

fn annotations(doc: &Document, page: ObjRef) -> Vec<Object> {
    let obj = doc.get(page);
    obj.as_dict().and_then(|d| d.get(b"Annots")).map(|a| doc.resolve(a)).and_then(|a| a.as_array().cloned()).unwrap_or_default()
}

/// Rotate the content of `indices` (0-based) by `degrees` clockwise, as the page is shown
/// (its `/Rotate` taken into account), about the centre of the page, fitting it as `fit` says.
///
/// - `degrees` must be finite and within −180…180.
/// - A whole multiple of 90° only changes `/Rotate` ([`rotate_pages`]): lossless, the content
///   and the annotations stay as they are, and `fit` does not apply.
/// - Any other angle goes through [`transform_page`] with the matrix and box of
///   [`rotation_transform`]. A page with comments or form fields
///   ([`page_has_drawn_annotations`]) is refused first, because their appearances can't turn
///   inside an axis-aligned `/Rect`: flatten them, or rotate by a multiple of 90°.
///
/// Every page is checked before any changes, so an error leaves the document as it was.
pub fn rotate_content(doc: &mut Document, indices: &[usize], degrees: f64, fit: RotateFit) -> Result<(), OrganizeError> {
    check_angle(degrees)?;
    let quarters = (degrees / 90.0).round();
    if (degrees - quarters * 90.0).abs() < 1e-9 {
        // |degrees| ≤ 180, so `quarters` is one of −2…2 and the cast is exact.
        return rotate_pages(doc, indices, quarters as i64 * 90);
    }
    let all = walk(doc)?;
    check(indices, all.len())?;
    for &i in indices {
        let (page, inherited) = all.get(i).ok_or(OrganizeError::NoSuchPage(i))?;
        if drawn_annotations(doc, *page) {
            return Err(invalid(format!(
                "page {} has comments or form fields; flatten them first or rotate by a multiple of 90°",
                i.saturating_add(1)
            )));
        }
        let crop = boxes_of(doc, *page, inherited)[1];
        if !big_enough(crop) {
            return Err(invalid(format!("page {} has an empty crop box, so there is nothing to rotate", i.saturating_add(1))));
        }
    }
    for &i in indices {
        let (page, inherited) = all.get(i).ok_or(OrganizeError::NoSuchPage(i))?;
        // Read the boxes again: a page listed twice turns twice, from where the first turn left it.
        let crop = boxes_of(doc, *page, inherited)[1];
        let rotate = page_rotate(doc, *page, inherited);
        let (m, media) = rotation_for(crop, rotate, degrees, fit, i)?;
        apply(doc, i, *page, crop, m, media)?;
    }
    Ok(())
}

/// The matrix and media box [`rotate_content`] would give page `index` (0-based) for a turn of
/// `degrees` clockwise as shown, about the page centre, with `fit`. Useful to preview a turn or
/// to apply it with [`transform_page`] directly (deskew uses [`RotateFit::Crop`]). Unlike
/// [`rotate_content`] it never delegates to `/Rotate`: a multiple of 90° yields the matrix too.
pub fn rotation_transform(doc: &Document, index: usize, degrees: f64, fit: RotateFit) -> Result<([f64; 6], [f64; 4]), OrganizeError> {
    check_angle(degrees)?;
    let all = walk(doc)?;
    let (page, inherited) = all.get(index).ok_or(OrganizeError::NoSuchPage(index))?;
    let crop = boxes_of(doc, *page, inherited)[1];
    let (m, media) = rotation_for(crop, page_rotate(doc, *page, inherited), degrees, fit, index)?;
    Ok((m.0, media))
}

fn check_angle(degrees: f64) -> Result<(), OrganizeError> {
    if !degrees.is_finite() {
        return Err(invalid("the angle must be a number of degrees from -180 to 180"));
    }
    if degrees.abs() > 180.0 {
        return Err(invalid(format!("an angle of {degrees}° is out of range: use -180 to 180 degrees (negative turns counterclockwise)")));
    }
    Ok(())
}

/// The page's effective `/Rotate`: its own, else the inherited one.
pub(crate) fn page_rotate(doc: &Document, page: ObjRef, inherited: &Dict) -> i64 {
    let own = doc.get(page);
    let attrs = own.as_dict().filter(|d| d.get(b"Rotate").is_some()).unwrap_or(inherited);
    resolved_rotation(doc, attrs)
}

/// A clockwise quarter turn (`/Rotate`, §7.7.3.3) as a matrix, written exactly. A `/Rotate` that
/// isn't a multiple of 90 is invalid and counts as 0.
fn quarter_turn(rotate: i64) -> Matrix {
    match rotate.rem_euclid(360) {
        90 => Matrix([0.0, -1.0, 1.0, 0.0, 0.0, 0.0]),
        180 => Matrix([-1.0, 0.0, 0.0, -1.0, 0.0, 0.0]),
        270 => Matrix([0.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
        _ => Matrix::IDENTITY,
    }
}

/// A clockwise turn by `degrees` (y up, as PDF spaces are).
fn turn_cw(degrees: f64) -> Matrix {
    let (s, c) = degrees.to_radians().sin_cos();
    Matrix([c, -s, s, c, 0.0, 0.0])
}

/// `m` applied about the point `(x, y)` instead of the origin.
fn about(m: Matrix, x: f64, y: f64) -> Matrix {
    Matrix::translate(-x, -y).then(&m).then(&Matrix::translate(x, y))
}

/// The rotation in display space, converted to the page's user space: user space goes to display
/// space (the page's `/Rotate`, with the shown page's corner at the origin), turns there about the
/// shown page's centre, and comes back.
fn rotation_for(crop: [f64; 4], rotate: i64, degrees: f64, fit: RotateFit, index: usize) -> Result<(Matrix, [f64; 4]), OrganizeError> {
    let quarter = quarter_turn(rotate);
    let shown = quarter.bbox(crop);
    let to_display = quarter.then(&Matrix::translate(-shown[0], -shown[1]));
    let from_display = to_display.invert().ok_or_else(|| invalid(format!("page {} can't be turned", index.saturating_add(1))))?;
    let (w, h) = (shown[2] - shown[0], shown[3] - shown[1]);
    let mut m = to_display.then(&about(turn_cw(degrees), w / 2.0, h / 2.0)).then(&from_display);
    let turned = m.bbox(crop);
    let media = match fit {
        RotateFit::Expand => turned,
        RotateFit::Crop => crop,
        RotateFit::KeepSize => {
            let (tw, th) = (turned[2] - turned[0], turned[3] - turned[1]);
            if tw <= 0.0 || th <= 0.0 {
                return Err(invalid(format!("page {} has an empty crop box, so there is nothing to rotate", index.saturating_add(1))));
            }
            let s = ((crop[2] - crop[0]) / tw).min((crop[3] - crop[1]) / th).min(1.0);
            let centre = ((crop[0] + crop[2]) / 2.0, (crop[1] + crop[3]) / 2.0);
            m = m.then(&about(Matrix([s, 0.0, 0.0, s, 0.0, 0.0]), centre.0, centre.1));
            crop
        }
    };
    Ok((m, media))
}

/// Where the page's content goes: the page's `/Contents` as a list, without changing any stream.
fn content_list(doc: &Document, page: &Dict, index: usize) -> Result<Vec<Object>, OrganizeError> {
    let damaged = || invalid(format!("page {}'s content is damaged (its /Contents is neither a stream nor an array)", index.saturating_add(1)));
    Ok(match page.get(b"Contents") {
        None | Some(Object::Null) => Vec::new(),
        Some(Object::Ref(r)) => match &*doc.get(*r) {
            Object::Array(items) => items.clone(),
            // A stream, or a reference to nothing (which readers skip): kept as it is.
            Object::Stream(_) | Object::Null => vec![Object::Ref(*r)],
            _ => return Err(damaged()),
        },
        Some(Object::Array(items)) => items.clone(),
        Some(_) => return Err(damaged()),
    })
}

/// The transform itself, after the page was found: validate everything, then write.
pub(crate) fn apply(doc: &mut Document, index: usize, page: ObjRef, crop: [f64; 4], m: Matrix, new_media: [f64; 4]) -> Result<(), OrganizeError> {
    let n = index.saturating_add(1);
    if !m.0.iter().all(|v| v.is_finite()) || m.invert().is_none() {
        return Err(invalid(format!("the transform for page {n} must be finite and invertible")));
    }
    if !new_media.iter().all(|v| v.is_finite() && v.abs() <= MAX_COORD) {
        return Err(invalid(format!("the new page size for page {n} is not a usable rectangle")));
    }
    let media = normalised(new_media);
    if !big_enough(media) {
        return Err(invalid(format!("page {n} would be smaller than a point")));
    }
    if !big_enough(crop) {
        return Err(invalid(format!("page {n} has an empty crop box, so there is nothing to transform")));
    }
    let dict = doc.get(page).as_dict().cloned().ok_or(OrganizeError::NoSuchPage(index))?;
    let existing = content_list(doc, &dict, index)?;

    // Trim, bleed and art boxes follow the content.
    let children: Vec<(&[u8], Option<[f64; 4]>)> = [&b"TrimBox"[..], b"BleedBox", b"ArtBox"]
        .into_iter()
        .filter_map(|k| own_box(doc, page, k).map(|r| (k, Some(intersect(m.bbox(r), media)).filter(|r| big_enough(*r)))))
        .collect();
    transform_annotations(doc, page, &dict, &m)?;

    // Content: wrap, don't rewrite.
    let [a, b, c, d, e, f] = m.0.map(num);
    let [x0, y0, x1, y1] = crop;
    let head = format!("q {a} {b} {c} {d} {e} {f} cm {} {} {} {} re W n\n", num(x0), num(y0), num(x1 - x0), num(y1 - y0));
    let head = doc.add(Object::Stream(Stream::from_raw(Dict::new(), head.into_bytes())));
    let tail = doc.add(Object::Stream(Stream::from_raw(Dict::new(), b"\nQ\n".to_vec())));
    let mut contents = Vec::with_capacity(existing.len().saturating_add(2));
    contents.push(Object::Ref(head));
    contents.extend(existing);
    contents.push(Object::Ref(tail));
    doc.update_dict(page, |p| {
        p.set(b"Contents".to_vec(), Object::Array(contents));
        p.set(b"MediaBox".to_vec(), rect_object(media));
        p.set(b"CropBox".to_vec(), rect_object(media));
        for (k, r) in children {
            match r {
                Some(r) => p.set(k.to_vec(), rect_object(r)),
                None => {
                    p.remove(k);
                }
            }
        }
    })?;
    Ok(())
}

/// Transform every annotation on the page (each object once, however often it is listed).
fn transform_annotations(doc: &mut Document, page: ObjRef, dict: &Dict, m: &Matrix) -> Result<(), OrganizeError> {
    let slot = dict.get(b"Annots").cloned();
    let list = slot.as_ref().map(|a| doc.resolve(a)).and_then(|a| a.as_array().cloned()).unwrap_or_default();
    let mut seen = HashSet::new();
    let mut inline = list.clone();
    let mut inline_changed = false;
    for (entry, out) in list.iter().zip(inline.iter_mut()) {
        match entry {
            Object::Ref(r) => {
                if !seen.insert(*r) {
                    continue;
                }
                let current = doc.get(*r);
                if let Object::Dict(a) = &*current {
                    let moved = transform_annotation(doc, a, m);
                    if &moved != a {
                        doc.update_dict(*r, |x| *x = moved)?;
                    }
                }
            }
            Object::Dict(a) => {
                *out = Object::Dict(transform_annotation(doc, a, m));
                inline_changed = true;
            }
            _ => {}
        }
    }
    if inline_changed {
        match slot {
            Some(Object::Ref(r)) => doc.set(r, Object::Array(inline)),
            _ => doc.update_dict(page, |p| p.set(b"Annots".to_vec(), Object::Array(inline)))?,
        }
    }
    Ok(())
}

/// One annotation, transformed: `/Rect` to its bounding box, the point lists exactly. Entries
/// that aren't usable numbers are left as they are.
fn transform_annotation(doc: &Document, a: &Dict, m: &Matrix) -> Dict {
    let mut out = a.clone();
    if let Some(r) = numbers(doc, a.get(b"Rect")).filter(|r| r.len() == 4) {
        let b = m.bbox(normalised([r[0], r[1], r[2], r[3]]));
        if b.iter().all(|v| v.is_finite()) {
            out.set(b"Rect".to_vec(), rect_object(b));
        }
    }
    for key in [&b"QuadPoints"[..], b"Vertices", b"L", b"CL"] {
        if let Some(points) = a.get(key).and_then(|v| points(doc, v, m)) {
            out.set(key.to_vec(), points);
        }
    }
    if let Some(ink) = a.get(b"InkList").map(|v| doc.resolve(v)).and_then(|v| v.as_array().cloned()) {
        let paths: Option<Vec<Object>> = ink.iter().map(|path| points(doc, path, m)).collect();
        if let Some(paths) = paths {
            out.set(b"InkList".to_vec(), Object::Array(paths));
        }
    }
    out
}

/// The finite numbers of an array (resolving references), or `None` if any element isn't one.
fn numbers(doc: &Document, o: Option<&Object>) -> Option<Vec<f64>> {
    let o = doc.resolve(o?);
    o.as_array()?.iter().map(|x| doc.resolve(x).as_f64().filter(|v| v.is_finite())).collect()
}

/// An array of x y pairs, each pair transformed by `m`.
fn points(doc: &Document, o: &Object, m: &Matrix) -> Option<Object> {
    let v = numbers(doc, Some(o))?;
    if v.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(v.len());
    for &[x, y] in v.as_chunks::<2>().0 {
        let (x, y) = m.apply(x, y);
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        out.push(real(x));
        out.push(real(y));
    }
    Some(Object::Array(out))
}

#[cfg(test)]
mod tests;
