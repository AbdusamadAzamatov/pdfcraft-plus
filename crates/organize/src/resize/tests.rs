use std::sync::Arc;

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Document, ObjRef, Object, SaveOptions, write_full};
use proptest::prelude::*;

use super::*;
use crate::{page_boxes, pages};

const LETTER: (f64, f64) = (612.0, 792.0);
const A4: (f64, f64) = (595.28, 841.89);

/// A document whose pages have the given page-dictionary snippets (objects 3, 4, … are the
/// pages, each with its own content stream after them) and `extra` objects numbered after all
/// of those.
fn doc(pages_spec: &[&str], extra: &[&str]) -> Document {
    let n = pages_spec.len();
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
    let mut objs: Vec<Vec<u8>> =
        vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(), format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).into_bytes()];
    for (i, spec) in pages_spec.iter().enumerate() {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << >> {spec} >>", 4 + 2 * i).into_bytes());
        objs.push(stream(b"1 0 0 rg 40 40 20 20 re f"));
    }
    objs.extend(extra.iter().map(|e| e.as_bytes().to_vec()));
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    Document::open(Arc::new(out)).expect("opens")
}

fn stream(body: &[u8]) -> Vec<u8> {
    let mut v = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
    v.extend_from_slice(body);
    v.extend_from_slice(b"\nendstream");
    v
}

fn page_dict(doc: &Document, i: usize) -> pdfcraft_cos::Dict {
    doc.get(pages(doc).unwrap()[i].obj).as_dict().cloned().unwrap()
}

fn nums(doc: &Document, o: &Object) -> Vec<f64> {
    doc.resolve(o).as_array().unwrap().iter().map(|x| doc.resolve(x).as_f64().unwrap()).collect()
}

fn media(doc: &Document, i: usize) -> [f64; 4] {
    page_boxes(doc).unwrap()[i][0]
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-4
}

fn stream_text(doc: &Document, o: &Object) -> String {
    let s = doc.resolve(o);
    let Object::Stream(s) = &*s else { panic!("not a stream: {s:?}") };
    String::from_utf8(s.raw.to_vec()).unwrap()
}

/// The `cm` operands of the page's new first content stream.
fn wrap_matrix(doc: &Document, i: usize) -> [f64; 6] {
    let contents = page_dict(doc, i).get(b"Contents").unwrap().as_array().unwrap().clone();
    let head = stream_text(doc, &contents[0]);
    let v: Vec<f64> = head.split_whitespace().skip(1).take(6).map(|t| t.parse().unwrap()).collect();
    [v[0], v[1], v[2], v[3], v[4], v[5]]
}

#[test]
fn a_letter_page_made_a4_in_fit_mode_scales_by_the_smaller_factor_and_is_centred() {
    let mut d = doc(&["/MediaBox [0 0 612 792]"], &[]);
    let old = page_dict(&d, 0).get(b"Contents").cloned().unwrap();
    resize_pages(&mut d, &[0], A4.0, A4.1, SizeOrientation::Match, ResizeMode::Fit).unwrap();
    let (sx, sy) = (A4.0 / LETTER.0, A4.1 / LETTER.1);
    let s = sx.min(sy);
    assert!(close(s, sx), "Letter → A4 is limited by the width");
    let [a, b, c, dd, e, f] = wrap_matrix(&d, 0);
    assert!(close(a, s) && close(dd, s) && b == 0.0 && c == 0.0, "uniform scale {s}: {a} {dd}");
    // Centred: equal margins left/right (none) and top/bottom.
    assert!(close(e, 0.0), "{e}");
    assert!(close(f, (A4.1 - s * LETTER.1) / 2.0), "{f}");
    let (top, bottom) = (A4.1 - (s * LETTER.1 + f), f);
    assert!(close(top, bottom) && bottom > 10.0, "{top} vs {bottom}");
    assert_eq!(media(&d, 0), [0.0, 0.0, A4.0, A4.1]);
    assert_eq!(page_boxes(&d).unwrap()[0][1], [0.0, 0.0, A4.0, A4.1], "crop box too");
    let contents = page_dict(&d, 0).get(b"Contents").unwrap().as_array().unwrap().clone();
    assert_eq!(contents[1], old, "the content stream is kept, not rewritten");
    assert!(stream_text(&d, &contents[0]).ends_with("cm 0 0 612 792 re W n\n"), "clipped to the old page");
    // It still saves and opens.
    let bytes = write_full(&d, &SaveOptions::default()).unwrap();
    assert_eq!(page_boxes(&Document::open(Arc::new(bytes)).unwrap()).unwrap()[0][0], [0.0, 0.0, A4.0, A4.1]);
}

#[test]
fn an_annotation_rect_and_its_points_scale_with_the_page() {
    let mut d = doc(
        &["/MediaBox [0 0 612 792] /Annots [5 0 R]"],
        &["<< /Type /Annot /Subtype /Highlight /Rect [100 100 300 140] /QuadPoints [100 140 300 140 100 100 300 100] >>"],
    );
    let (m, _) = resize_transform(&d, 0, A4.0, A4.1, SizeOrientation::Match, ResizeMode::Fit).unwrap();
    resize_pages(&mut d, &[0], A4.0, A4.1, SizeOrientation::Match, ResizeMode::Fit).unwrap();
    let a = d.get(ObjRef::new(5, 0)).as_dict().cloned().unwrap();
    let m = Matrix(m);
    let expected = m.bbox([100.0, 100.0, 300.0, 140.0]);
    let rect = nums(&d, a.get(b"Rect").unwrap());
    assert!(rect.iter().zip(expected).all(|(a, b)| close(*a, b)), "{rect:?} vs {expected:?}");
    // The width scaled by s = 595.28 / 612.
    assert!(close(rect[2] - rect[0], 200.0 * A4.0 / LETTER.0), "{rect:?}");
    let q = nums(&d, a.get(b"QuadPoints").unwrap());
    let (x, y) = m.apply(100.0, 140.0);
    assert!(close(q[0], x) && close(q[1], y), "{q:?}");
}

#[test]
fn only_pages_of_another_size_are_picked_and_a4_pages_are_left_byte_for_byte() {
    let mut d =
        doc(&["/MediaBox [0 0 612 792]", "/MediaBox [0 0 595 842]", "/MediaBox [0 0 842 595]", "/MediaBox [0 0 595.28 841.89] /Rotate 90"], &[]);
    // Page 3 is A4 landscape: the same size when each page keeps its orientation. Page 4 shows
    // A4 landscape too (portrait box, turned a quarter).
    let differ = pages_of_other_size(&d, &[0, 1, 2, 3], A4.0, A4.1, SizeOrientation::Match).unwrap();
    assert_eq!(differ, vec![0]);
    assert_eq!(pages_of_other_size(&d, &[0, 1, 2, 3], A4.0, A4.1, SizeOrientation::Portrait).unwrap(), vec![0, 2, 3]);
    assert_eq!(pages_of_other_size(&d, &[3, 1], A4.1, A4.0, SizeOrientation::Landscape).unwrap(), vec![1], "order kept, w/h order irrelevant");
    let before: Vec<_> = (1..4).map(|i| page_dict(&d, i)).collect();
    let bytes_before: Vec<_> = (1..4).map(|i| object_bytes(&d, i)).collect();
    resize_pages(&mut d, &differ, A4.0, A4.1, SizeOrientation::Match, ResizeMode::Fit).unwrap();
    assert_eq!(media(&d, 0), [0.0, 0.0, A4.0, A4.1]);
    for i in 1..4 {
        assert_eq!(page_dict(&d, i), before[i - 1], "page {} untouched", i + 1);
        assert_eq!(object_bytes(&d, i), bytes_before[i - 1], "page {} byte for byte", i + 1);
    }
}

/// The page object as a save writes it.
fn object_bytes(d: &Document, i: usize) -> Vec<u8> {
    let page = pages(d).unwrap()[i].obj;
    let mut out = Vec::new();
    pdfcraft_cos::serialize(&d.get(page), &mut out);
    out
}

#[test]
fn sizes_are_as_the_page_is_shown_so_a_turned_page_gets_a_turned_box() {
    // A Letter page stored landscape and shown portrait (/Rotate 90).
    for rotate in [90, 270] {
        let mut d = doc(&[&format!("/MediaBox [0 0 792 612] /Rotate {rotate}")], &[]);
        resize_pages(&mut d, &[0], A4.0, A4.1, SizeOrientation::Match, ResizeMode::Fit).unwrap();
        assert_eq!(media(&d, 0), [0.0, 0.0, A4.1, A4.0], "/Rotate {rotate}: shown A4 portrait");
        let [a, .., e, f] = wrap_matrix(&d, 0);
        // In user space the long side runs across: 792 → 841.89 and 612 → 595.28, so s = 595.28 / 612.
        assert!(close(a, A4.0 / LETTER.0), "{a}");
        // (`a` was written with six decimals, so allow for its rounding times 792.)
        assert!(close(f, 0.0) && (e - (A4.1 - a * LETTER.1) / 2.0).abs() < 1e-3, "centred: {e} {f}");
        assert_eq!(pages_of_other_size(&d, &[0], A4.0, A4.1, SizeOrientation::Portrait).unwrap(), Vec::<usize>::new());
    }
    // Asked for landscape, the turned page is shown landscape.
    let mut d = doc(&["/MediaBox [0 0 792 612] /Rotate 90"], &[]);
    resize_pages(&mut d, &[0], A4.0, A4.1, SizeOrientation::Landscape, ResizeMode::Fit).unwrap();
    assert_eq!(media(&d, 0), [0.0, 0.0, A4.0, A4.1]);
}

#[test]
fn orientation_portrait_landscape_and_match() {
    let mut d = doc(&["/MediaBox [0 0 612 792]", "/MediaBox [0 0 792 612]"], &[]);
    let mut e = d.clone();
    resize_pages(&mut d, &[0, 1], A4.1, A4.0, SizeOrientation::Match, ResizeMode::Fit).unwrap();
    assert_eq!(media(&d, 0), [0.0, 0.0, A4.0, A4.1], "portrait stays portrait, whatever order the size was given in");
    assert_eq!(media(&d, 1), [0.0, 0.0, A4.1, A4.0], "landscape stays landscape");
    resize_pages(&mut e, &[0, 1], A4.0, A4.1, SizeOrientation::Landscape, ResizeMode::Fit).unwrap();
    assert_eq!(media(&e, 0), [0.0, 0.0, A4.1, A4.0]);
    assert_eq!(media(&e, 1), [0.0, 0.0, A4.1, A4.0]);
}

#[test]
fn fill_covers_and_crops_and_no_scaling_recentres() {
    let mut d = doc(&["/MediaBox [10 20 210 120] /TrimBox [10 20 110 120]"], &[]);
    let mut e = d.clone();
    // 200 × 100 onto 100 × 100: fill scales by 1 (the height), cropping 50 pt each side.
    resize_pages(&mut d, &[0], 100.0, 100.0, SizeOrientation::Match, ResizeMode::Fill).unwrap();
    let [a, _, _, dd, x, y] = wrap_matrix(&d, 0);
    assert!(close(a, 1.0) && close(dd, 1.0));
    assert!(close(x, -50.0 - 10.0) && close(y, -20.0), "{x} {y}");
    assert_eq!(media(&d, 0), [0.0, 0.0, 100.0, 100.0]);
    // The trim box followed the content and was clipped to the new page.
    let trim = nums(&d, page_dict(&d, 0).get(b"TrimBox").unwrap());
    assert_eq!(trim, [0.0, 0.0, 50.0, 100.0]);
    // No scaling onto 300 × 300: the page grows around the content, centred.
    resize_pages(&mut e, &[0], 300.0, 300.0, SizeOrientation::Match, ResizeMode::NoScale).unwrap();
    let [a, _, _, dd, x, y] = wrap_matrix(&e, 0);
    assert!(close(a, 1.0) && close(dd, 1.0));
    assert!(close(x, 50.0 - 10.0) && close(y, 100.0 - 20.0), "{x} {y}");
    assert_eq!(media(&e, 0), [0.0, 0.0, 300.0, 300.0]);
}

#[test]
fn a_user_unit_is_honoured_so_the_target_stays_in_points() {
    let mut d = doc(&["/MediaBox [0 0 306 396] /UserUnit 2"], &[]);
    assert_eq!(pages_of_other_size(&d, &[0], LETTER.0, LETTER.1, SizeOrientation::Match).unwrap(), Vec::<usize>::new(), "shown as Letter");
    resize_pages(&mut d, &[0], A4.0, A4.1, SizeOrientation::Match, ResizeMode::Fit).unwrap();
    let m = media(&d, 0);
    assert!(close(m[2], A4.0 / 2.0) && close(m[3], A4.1 / 2.0), "{m:?}");
}

#[test]
fn unusable_sizes_and_pages_are_refused_and_nothing_changes() {
    let mut d = doc(&["/MediaBox [0 0 612 792]"], &[]);
    let before = page_dict(&d, 0);
    for (w, h) in [(f64::NAN, 100.0), (100.0, f64::INFINITY), (0.0, 100.0), (-5.0, 100.0), (0.5, 100.0), (14_401.0, 100.0)] {
        let err = resize_pages(&mut d, &[0], w, h, SizeOrientation::Match, ResizeMode::Fit).unwrap_err();
        assert!(matches!(&err, OrganizeError::Invalid(m) if m.contains("from 1 to 14400 pt")), "{w}×{h}: {err}");
        assert!(pages_of_other_size(&d, &[0], w, h, SizeOrientation::Match).is_err());
        assert!(resize_transform(&d, 0, w, h, SizeOrientation::Match, ResizeMode::Fit).is_err());
    }
    assert_eq!(resize_pages(&mut d, &[1], 100.0, 100.0, SizeOrientation::Match, ResizeMode::Fit), Err(OrganizeError::NoSuchPage(1)));
    assert!(matches!(resize_pages(&mut d, &[], 100.0, 100.0, SizeOrientation::Match, ResizeMode::Fit), Err(OrganizeError::Invalid(_))));
    assert_eq!(page_dict(&d, 0), before);
    // A page whose crop box is under a point can't be resized: refused before anything changes.
    let mut d = doc(&["/MediaBox [0 0 612 792]", "/MediaBox [0 0 612 0.5]"], &[]);
    let first = page_dict(&d, 0);
    let err = resize_pages(&mut d, &[0, 1], 100.0, 100.0, SizeOrientation::Match, ResizeMode::Fit).unwrap_err();
    assert!(err.to_string().contains("page 2 has an empty crop box"), "{err}");
    assert_eq!(page_dict(&d, 0), first, "page 1 was not changed either");
    // A page listed twice is resized once.
    let mut d = doc(&["/MediaBox [0 0 612 792]"], &[]);
    resize_pages(&mut d, &[0, 0], A4.0, A4.1, SizeOrientation::Match, ResizeMode::Fit).unwrap();
    assert_eq!(page_dict(&d, 0).get(b"Contents").unwrap().as_array().unwrap().len(), 3);
    assert_eq!(ResizeMode::from_name("none"), Some(ResizeMode::NoScale));
    assert!(ResizeMode::ALL.iter().all(|m| ResizeMode::from_name(m.name()) == Some(*m)));
    assert!(SizeOrientation::ALL.iter().all(|o| SizeOrientation::from_name(o.name()) == Some(*o)));
}

proptest! {
    /// Whatever the page, mode and orientation: the new page has the target size as shown, Fit
    /// keeps the whole old page inside it, Fill covers it, and the content is centred.
    #[test]
    fn every_mode_keeps_its_promise(
        w in 10.0f64..3000.0,
        h in 10.0f64..3000.0,
        x0 in -500.0f64..500.0,
        tw in 1.0f64..14_400.0,
        th in 1.0f64..14_400.0,
        rotate in 0i64..4,
    ) {
        let crop = [x0, 7.0, x0 + w, 7.0 + h];
        for mode in ResizeMode::ALL {
            let d = doc(&[&format!("/MediaBox [{} {} {} {}] /Rotate {}", crop[0], crop[1], crop[2], crop[3], rotate * 90)], &[]);
            let (m, media) = resize_transform(&d, 0, tw, th, SizeOrientation::Portrait, mode).unwrap();
            let shown = if rotate % 2 == 1 { (media[3], media[2]) } else { (media[2], media[3]) };
            prop_assert!(close(shown.0, tw.min(th)) && close(shown.1, tw.max(th)), "{:?}", shown);
            let b = Matrix(m).bbox(crop);
            let centre = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
            prop_assert!((centre.0 - media[2] / 2.0).abs() < 1e-6 * media[2].max(1.0) && (centre.1 - media[3] / 2.0).abs() < 1e-6 * media[3].max(1.0));
            let eps = 1e-6 * media[2].max(media[3]);
            match mode {
                ResizeMode::Fit => prop_assert!(b[0] >= -eps && b[1] >= -eps && b[2] <= media[2] + eps && b[3] <= media[3] + eps, "{:?} in {:?}", b, media),
                ResizeMode::Fill => prop_assert!(b[0] <= eps && b[1] <= eps && b[2] >= media[2] - eps && b[3] >= media[3] - eps, "{:?} covers {:?}", b, media),
                ResizeMode::NoScale => prop_assert!(close(m[0], 1.0) && close(m[3], 1.0)),
            }
        }
    }
}
