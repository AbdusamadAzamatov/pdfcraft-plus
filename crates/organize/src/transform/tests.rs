use std::sync::Arc;

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Document, ObjRef, Object, SaveOptions, write_full};
use proptest::prelude::*;

use super::*;
use crate::{page_boxes, page_rotation, pages};

/// One page (object 3) with content stream 4 and `extra` objects numbered from 5. `page` is
/// spliced into the page dictionary.
fn doc(page: &str, extra: &[&str]) -> Document {
    let mut objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << >> {page} >>").into_bytes(),
        stream(b"1 0 0 rg 40 40 20 20 re f"),
    ];
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

fn page_dict(doc: &Document) -> pdfcraft_cos::Dict {
    doc.get(pages(doc).unwrap()[0].obj).as_dict().cloned().unwrap()
}

fn nums(doc: &Document, o: &Object) -> Vec<f64> {
    doc.resolve(o).as_array().unwrap().iter().map(|x| doc.resolve(x).as_f64().unwrap()).collect()
}

fn rect(doc: &Document, key: &[u8]) -> [f64; 4] {
    let v = nums(doc, page_dict(doc).get(key).unwrap());
    [v[0], v[1], v[2], v[3]]
}

fn annot(doc: &Document, num: u32) -> pdfcraft_cos::Dict {
    doc.get(ObjRef::new(num, 0)).as_dict().cloned().unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

fn stream_text(doc: &Document, o: &Object) -> String {
    let s = doc.resolve(o);
    let Object::Stream(s) = &*s else { panic!("not a stream: {s:?}") };
    String::from_utf8(s.raw.to_vec()).unwrap()
}

#[test]
fn forty_five_degrees_on_a_100_point_square_gives_a_141_point_square() {
    let mut d = doc("/MediaBox [0 0 100 100]", &[]);
    rotate_content(&mut d, &[0], 45.0, RotateFit::Expand).unwrap();
    let media = rect(&d, b"MediaBox");
    let side = 100.0 * 2f64.sqrt();
    assert!(close(media[2] - media[0], side) && close(media[3] - media[1], side), "{media:?}");
    // Centred on the old centre.
    assert!(close(media[0] + media[2], 100.0) && close(media[1] + media[3], 100.0), "{media:?}");
    assert_eq!(rect(&d, b"CropBox"), media);
    assert_eq!(page_rotation(&d, 0).unwrap(), 0, "/Rotate is left alone");
}

#[test]
fn quarter_turns_go_through_rotate_and_leave_the_content_alone() {
    for (degrees, expected) in [(90.0, 90), (-90.0, 270), (180.0, 180), (-180.0, 180), (0.0, 0)] {
        let mut d = doc("/MediaBox [0 0 200 100] /Annots [5 0 R]", &["<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] >>"]);
        let before = page_dict(&d);
        rotate_content(&mut d, &[0], degrees, RotateFit::KeepSize).unwrap();
        let after = page_dict(&d);
        assert_eq!(after.get(b"Contents"), before.get(b"Contents"), "content unchanged at {degrees}°");
        assert_eq!(after.get(b"MediaBox"), before.get(b"MediaBox"));
        assert_eq!(nums(&d, annot(&d, 5).get(b"Rect").unwrap()), [10.0, 10.0, 30.0, 30.0], "annotations stay");
        assert_eq!(page_rotation(&d, 0).unwrap(), expected);
    }
}

#[test]
fn a_page_with_comments_or_fields_is_refused_and_left_alone() {
    for subtype in ["Text", "Widget", "Ink"] {
        let mut d = doc("/MediaBox [0 0 100 100] /Annots [5 0 R]", &[&format!("<< /Type /Annot /Subtype /{subtype} /Rect [10 10 30 30] >>")]);
        let before = page_dict(&d);
        let err = rotate_content(&mut d, &[0], 12.5, RotateFit::Expand).unwrap_err();
        assert_eq!(err, OrganizeError::Invalid("page 1 has comments or form fields; flatten them first or rotate by a multiple of 90°".into()));
        assert_eq!(page_dict(&d), before);
        assert!(page_has_drawn_annotations(&d, 0).unwrap());
    }
}

#[test]
fn links_popups_and_hidden_annotations_turn_with_the_page() {
    let mut d = doc(
        "/MediaBox [0 0 100 100] /Annots [5 0 R 6 0 R 7 0 R 5 0 R]",
        &[
            "<< /Type /Annot /Subtype /Link /Rect [40 40 60 60] /QuadPoints [40 60 60 60 40 40 60 40] >>",
            "<< /Type /Annot /Subtype /Popup /Rect [0 0 10 10] >>",
            "<< /Type /Annot /Subtype /Square /F 2 /Rect [0 0 10 10] >>",
        ],
    );
    assert!(!page_has_drawn_annotations(&d, 0).unwrap());
    let (m, _) = rotation_transform(&d, 0, 30.0, RotateFit::Expand).unwrap();
    rotate_content(&mut d, &[0], 30.0, RotateFit::Expand).unwrap();
    let m = Matrix(m);
    let link = annot(&d, 5);
    // Listed twice, transformed once: the exact quad points of the original.
    let q = nums(&d, link.get(b"QuadPoints").unwrap());
    for (k, (x, y)) in [(40.0, 60.0), (60.0, 60.0), (40.0, 40.0), (60.0, 40.0)].into_iter().enumerate() {
        let (ex, ey) = m.apply(x, y);
        assert!(close(q[2 * k], ex) && close(q[2 * k + 1], ey), "{q:?}");
    }
    let r = nums(&d, link.get(b"Rect").unwrap());
    let b = m.bbox([40.0, 40.0, 60.0, 60.0]);
    assert!(r.iter().zip(b).all(|(a, b)| close(*a, b)), "{r:?} vs {b:?}");
}

#[test]
fn transform_page_wraps_the_content_and_moves_boxes_and_annotations_exactly() {
    let mut d = doc(
        "/MediaBox [0 0 200 300] /CropBox [10 10 190 290] /TrimBox [20 20 180 280] /ArtBox [0 0 1 1] /Annots [5 0 R << /Subtype /Line /Rect [0 0 50 50] /L [0 0 50 50] /CL [1 2 3 4] >>]",
        &["<< /Type /Annot /Subtype /Ink /Rect [0 0 100 100] /InkList [[0 0 10 10] [20 20 30 30]] /Vertices [5 5 6 6] >>"],
    );
    let before = page_dict(&d);
    let old_content = before.get(b"Contents").cloned().unwrap();
    let m = [0.5, 0.0, 0.0, 0.5, 100.0, 50.0];
    transform_page(&mut d, 0, m, [100.0, 50.0, 210.0, 210.0]).unwrap();
    let p = page_dict(&d);
    let contents = p.get(b"Contents").unwrap().as_array().unwrap().clone();
    assert_eq!(contents.len(), 3);
    assert_eq!(contents[1], old_content, "the old stream is kept, not rewritten");
    assert_eq!(stream_text(&d, &contents[0]), "q 0.5 0 0 0.5 100 50 cm 10 10 180 280 re W n\n");
    assert_eq!(stream_text(&d, &contents[2]), "\nQ\n");
    assert_eq!(rect(&d, b"MediaBox"), [100.0, 50.0, 210.0, 210.0]);
    assert_eq!(rect(&d, b"CropBox"), [100.0, 50.0, 210.0, 210.0]);
    assert_eq!(rect(&d, b"TrimBox"), [110.0, 60.0, 190.0, 190.0], "transformed, clipped to the new media box");
    assert!(p.get(b"ArtBox").is_none(), "a box that ends up under a point goes");
    let ink = annot(&d, 5);
    assert_eq!(nums(&d, ink.get(b"Rect").unwrap()), [100.0, 50.0, 150.0, 100.0]);
    let paths = ink.get(b"InkList").unwrap().as_array().unwrap().clone();
    assert_eq!(nums(&d, &paths[0]), [100.0, 50.0, 105.0, 55.0]);
    assert_eq!(nums(&d, &paths[1]), [110.0, 60.0, 115.0, 65.0]);
    assert_eq!(nums(&d, ink.get(b"Vertices").unwrap()), [102.5, 52.5, 103.0, 53.0]);
    let annots = p.get(b"Annots").unwrap().as_array().unwrap().clone();
    let line = annots[1].as_dict().unwrap();
    assert_eq!(nums(&d, line.get(b"L").unwrap()), [100.0, 50.0, 125.0, 75.0]);
    assert_eq!(nums(&d, line.get(b"CL").unwrap()), [100.5, 51.0, 101.5, 52.0]);
    // It still saves and opens.
    let bytes = write_full(&d, &SaveOptions::default()).unwrap();
    let reopened = Document::open(Arc::new(bytes)).unwrap();
    assert_eq!(page_boxes(&reopened).unwrap()[0][0], [100.0, 50.0, 210.0, 210.0]);
}

#[test]
fn transform_page_refuses_unusable_input_and_changes_nothing() {
    let mut d = doc("/MediaBox [0 0 100 100]", &[]);
    let before = page_dict(&d);
    let id = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let media = [0.0, 0.0, 100.0, 100.0];
    assert_eq!(transform_page(&mut d, 1, id, media), Err(OrganizeError::NoSuchPage(1)));
    for bad in [[f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0], [0.0; 6], [1.0, 2.0, 2.0, 4.0, 0.0, 0.0], [1.0, 0.0, 0.0, 1.0, f64::INFINITY, 0.0]] {
        assert!(matches!(transform_page(&mut d, 0, bad, media), Err(OrganizeError::Invalid(_))), "{bad:?}");
    }
    for bad in [[0.0, 0.0, 0.5, 100.0], [0.0, 0.0, f64::NAN, 100.0], [0.0, 0.0, 1e300, 100.0]] {
        assert!(matches!(transform_page(&mut d, 0, id, bad), Err(OrganizeError::Invalid(_))), "{bad:?}");
    }
    assert_eq!(page_dict(&d), before);
    let mut junk = doc("/MediaBox [0 0 100 100]", &[]);
    let page = pages(&junk).unwrap()[0].obj;
    junk.update_dict(page, |p| p.set(b"Contents".to_vec(), Object::Int(7))).unwrap();
    assert!(matches!(transform_page(&mut junk, 0, id, media), Err(OrganizeError::Invalid(m)) if m.contains("damaged")));
}

#[test]
fn transform_page_handles_pages_without_content_and_with_content_arrays() {
    let mut d = doc("/MediaBox [0 0 100 100]", &[]);
    let page = pages(&d).unwrap()[0].obj;
    d.update_dict(page, |p| {
        p.remove(b"Contents");
    })
    .unwrap();
    transform_page(&mut d, 0, [1.0, 0.0, 0.0, 1.0, 5.0, 5.0], [5.0, 5.0, 105.0, 105.0]).unwrap();
    assert_eq!(page_dict(&d).get(b"Contents").unwrap().as_array().unwrap().len(), 2);
    // A shared, indirect content array is read, not changed.
    let mut d = doc("/MediaBox [0 0 100 100]", &["[4 0 R 4 0 R]"]);
    d.update_dict(page, |p| p.set(b"Contents".to_vec(), Object::Ref(ObjRef::new(5, 0)))).unwrap();
    transform_page(&mut d, 0, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 100.0, 100.0]).unwrap();
    assert_eq!(page_dict(&d).get(b"Contents").unwrap().as_array().unwrap().len(), 4);
    assert_eq!(d.get(ObjRef::new(5, 0)).as_array().unwrap().len(), 2);
}

#[test]
fn angles_that_are_not_numbers_or_out_of_range_are_refused() {
    let mut d = doc("/MediaBox [0 0 100 100]", &[]);
    let before = page_dict(&d);
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 180.1, -181.0] {
        let err = rotate_content(&mut d, &[0], bad, RotateFit::Expand).unwrap_err();
        assert!(matches!(&err, OrganizeError::Invalid(m) if m.contains("-180") && m.contains("180")), "{bad}: {err}");
    }
    assert_eq!(rotate_content(&mut d, &[3], 10.0, RotateFit::Expand), Err(OrganizeError::NoSuchPage(3)));
    assert_eq!(page_dict(&d), before);
    assert_eq!(RotateFit::from_name("keep_size"), Some(RotateFit::KeepSize));
    assert!(RotateFit::ALL.iter().all(|f| RotateFit::from_name(f.name()) == Some(*f)));
}

#[test]
fn the_turn_is_clockwise_as_the_page_is_shown_whatever_its_rotate() {
    // Shown upright, the point at the middle of the top edge moves right when the content turns
    // clockwise. With /Rotate 90 the shown top edge is the user-space left edge (x = 0).
    for (rotate, top_middle, right) in
        [(0, (50.0, 100.0), (1.0, 0.0)), (90, (0.0, 50.0), (0.0, 1.0)), (180, (50.0, 0.0), (-1.0, 0.0)), (270, (100.0, 50.0), (0.0, -1.0))]
    {
        let d = doc(&format!("/MediaBox [0 0 100 100] /Rotate {rotate}"), &[]);
        let (m, _) = rotation_transform(&d, 0, 10.0, RotateFit::Expand).unwrap();
        let (x, y) = Matrix(m).apply(top_middle.0, top_middle.1);
        let moved = (x - top_middle.0) * right.0 + (y - top_middle.1) * right.1;
        assert!(moved > 5.0, "/Rotate {rotate}: moved {moved}");
        let (cx, cy) = Matrix(m).apply(50.0, 50.0);
        assert!(close(cx, 50.0) && close(cy, 50.0), "turns about the centre");
    }
}

proptest! {
    /// Expand: the new media box holds every corner of the turned crop box; keep size: the turned
    /// corners fit the old crop box; crop: the box stays.
    #[test]
    fn every_fit_keeps_its_promise(
        degrees in -180.0f64..180.0,
        w in 10.0f64..2000.0,
        h in 10.0f64..2000.0,
        x0 in -500.0f64..500.0,
        rotate in 0i64..4,
    ) {
        let crop = [x0, 7.0, x0 + w, 7.0 + h];
        for fit in RotateFit::ALL {
            let mut d = doc(&format!("/MediaBox [{} {} {} {}] /Rotate {}", crop[0], crop[1], crop[2], crop[3], rotate * 90), &[]);
            let (m, _) = rotation_transform(&d, 0, degrees, fit).unwrap();
            rotate_content(&mut d, &[0], degrees, fit).unwrap();
            let media = page_boxes(&d).unwrap()[0][0];
            let corners = [(crop[0], crop[1]), (crop[2], crop[1]), (crop[0], crop[3]), (crop[2], crop[3])];
            let quarter = (degrees / 90.0).round() * 90.0 == degrees;
            if quarter {
                prop_assert_eq!(media, crop);
                continue;
            }
            let inside = |(x, y): (f64, f64), b: [f64; 4]| x >= b[0] - 1e-3 && x <= b[2] + 1e-3 && y >= b[1] - 1e-3 && y <= b[3] + 1e-3;
            for c in corners {
                let p = Matrix(m).apply(c.0, c.1);
                match fit {
                    RotateFit::Expand => prop_assert!(inside(p, media), "{:?} outside {:?}", p, media),
                    RotateFit::KeepSize => prop_assert!(inside(p, crop), "{:?} outside {:?}", p, crop),
                    RotateFit::Crop => {}
                }
            }
            if fit != RotateFit::Expand {
                prop_assert!(media.iter().zip(crop).all(|(a, b)| close(*a, b)), "{:?} vs {:?}", media, crop);
            }
        }
    }
}
