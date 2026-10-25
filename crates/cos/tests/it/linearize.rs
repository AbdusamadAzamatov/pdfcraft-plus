//! Linearized saving (Fast Web View, ISO 32000-2 Annex F): layout, detection, encryption and
//! the independent check — qpdf's `--check-linearization` must find nothing wrong.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pdfcraft_cos::{
    Algorithm, CosError, Dict, Document, NewEncryption, ObjRef, Object, PdfString, SaveOptions, Stream, is_linearized, write_full, write_incremental,
    write_linearized,
};
use proptest::prelude::*;

fn dict(entries: &[(&str, Object)]) -> Dict {
    let mut d = Dict::new();
    for (k, v) in entries {
        d.set(k.as_bytes().to_vec(), v.clone());
    }
    d
}

fn name(n: &str) -> Object {
    Object::name(n)
}

fn rect(w: i64, h: i64) -> Object {
    Object::Array(vec![Object::Int(0), Object::Int(0), Object::Int(w), Object::Int(h)])
}

fn image(doc: &mut Document, rgb: [u8; 3]) -> ObjRef {
    let d = dict(&[
        ("Type", name("XObject")),
        ("Subtype", name("Image")),
        ("Width", Object::Int(2)),
        ("Height", Object::Int(2)),
        ("ColorSpace", name("DeviceRGB")),
        ("BitsPerComponent", Object::Int(8)),
    ]);
    doc.add(Object::Stream(Stream::flate(d, &rgb.repeat(4))))
}

fn pages_root(doc: &Document) -> ObjRef {
    doc.get(doc.root().unwrap()).as_dict().unwrap().reference(b"Pages").unwrap()
}

/// `n` pages that exercise every part of the layout: MediaBox and Resources inherited from the
/// page tree, one content stream per page, a font every page uses, an image pages 2 and 3
/// share (shared-objects section), an image pages 1 and 4 share (first-page section), a
/// private image on page 5, a link on page 2 to page 7, and an /Info title. No /ID.
fn fixture(n: usize) -> Document {
    let mut doc = Document::new_empty();
    let pages = pages_root(&doc);
    let font = doc.add(dict(&[("Type", name("Font")), ("Subtype", name("Type1")), ("BaseFont", name("Helvetica"))]));
    let font_res = dict(&[("F1", Object::Ref(font))]);
    let resources = doc.add(dict(&[("Font", Object::Dict(font_res.clone()))]));
    let img23 = image(&mut doc, [200, 30, 30]);
    let img14 = image(&mut doc, [30, 200, 30]);
    let img5 = image(&mut doc, [30, 30, 200]);
    let mut kids = Vec::new();
    for i in 0..n {
        let img = match i {
            1 | 2 => Some(img23),
            0 | 3 => Some(img14),
            4 => Some(img5),
            _ => None,
        };
        let mut ops = String::new();
        if img.is_some() {
            ops.push_str("q 60 0 0 60 20 20 cm /Im1 Do Q\n");
        }
        ops.push_str(&format!("BT /F1 18 Tf 20 120 Td (Page {}) Tj ET", i + 1));
        let content = doc.add(Object::Stream(Stream::flate(Dict::new(), ops.as_bytes())));
        let mut page = dict(&[("Type", name("Page")), ("Parent", Object::Ref(pages)), ("Contents", Object::Ref(content))]);
        if let Some(img) = img {
            let xobjects = dict(&[("Im1", Object::Ref(img))]);
            page.set(b"Resources".to_vec(), Object::Dict(dict(&[("Font", Object::Dict(font_res.clone())), ("XObject", Object::Dict(xobjects))])));
        }
        kids.push(doc.add(page));
    }
    if n >= 7 {
        let link = dict(&[
            ("Type", name("Annot")),
            ("Subtype", name("Link")),
            ("Rect", rect(100, 40)),
            ("Border", Object::Array(vec![Object::Int(0), Object::Int(0), Object::Int(0)])),
            ("Dest", Object::Array(vec![Object::Ref(kids[6]), name("Fit")])),
        ]);
        let link = doc.add(link);
        doc.update_dict(kids[1], |d| d.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(link)]))).unwrap();
    }
    doc.update_dict(pages, |d| {
        d.set(b"Kids".to_vec(), Object::Array(kids.iter().map(|k| Object::Ref(*k)).collect()));
        d.set(b"Count".to_vec(), Object::Int(n as i64));
        d.set(b"MediaBox".to_vec(), rect(200, 160));
        d.set(b"Resources".to_vec(), Object::Ref(resources));
    })
    .unwrap();
    let info = doc.add(dict(&[("Title", Object::String(PdfString::literal(b"Linearized fixture".to_vec())))]));
    doc.trailer_mut().set(b"Info".to_vec(), Object::Ref(info));
    doc
}

fn opts() -> SaveOptions {
    SaveOptions { mod_date: None, ..SaveOptions::default() }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-lin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// The text of every page's content, in page order.
fn page_texts(doc: &Document) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![Object::Ref(pages_root(doc))];
    while let Some(node) = stack.pop() {
        let d = doc.resolve(&node).as_dict().cloned().unwrap();
        if let Some(kids) = d.get(b"Kids") {
            stack.extend(doc.resolve(kids).as_array().unwrap().iter().rev().cloned());
            continue;
        }
        match doc.resolve(d.get(b"Contents").unwrap()).as_ref() {
            Object::Stream(s) => out.push(String::from_utf8_lossy(&s.decoded().unwrap()).into_owned()),
            other => panic!("contents {other:?}"),
        }
    }
    out
}

fn qpdf_check(path: &Path, password: Option<&str>) {
    let qpdf = pdfcraft_oracle::or_skip!(pdfcraft_oracle::qpdf());
    let run = |args: &[&str]| {
        let mut cmd = qpdf.command();
        if let Some(pw) = password {
            cmd.arg(format!("--password={pw}"));
        }
        let out = cmd.args(args).arg(path).output().unwrap();
        (out.status.code(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    };
    let (code, text) = run(&["--check-linearization"]);
    if code != Some(0) || !text.contains("no linearization errors") || text.contains("WARNING") {
        let (_, shown) = run(&["--show-linearization"]);
        panic!("{}: qpdf --check-linearization exit {code:?}\n{text}\n--- show-linearization ---\n{shown}", path.display());
    }
    let (code, text) = run(&["--check"]);
    assert!(code == Some(0) && !text.contains("WARNING"), "{}: qpdf --check exit {code:?}\n{text}", path.display());
}

/// A trailer key's value, read straight from the file's last trailer (the first-page one).
fn lin_dict(bytes: &[u8]) -> Dict {
    let start = bytes.windows(4).position(|w| w == b" obj").unwrap();
    let line = bytes[..start].iter().rposition(|b| *b == b'\n').unwrap() + 1;
    let (_, o) = pdfcraft_cos::parse_indirect(bytes, line, &|_| None).unwrap();
    o.as_dict().cloned().unwrap()
}

fn int(d: &Dict, key: &str) -> i64 {
    d.get(key.as_bytes()).and_then(Object::as_int).unwrap()
}

#[test]
fn qpdf_checks_linearization() {
    for n in [1, 2, 3, 7, 100] {
        let doc = fixture(n);
        let bytes = write_linearized(&doc, &opts()).unwrap();
        let path = scratch(&format!("plain-{n}.pdf"));
        std::fs::write(&path, &bytes).unwrap();
        qpdf_check(&path, None);
    }
}

#[test]
fn qpdf_checks_a_rewritten_file_and_an_overlay_only_document() {
    let mut doc = fixture(7);
    let saved = write_full(&doc, &opts()).unwrap();
    let reopened = Document::open(Arc::new(saved)).unwrap();
    let path = scratch("reopened.pdf");
    std::fs::write(&path, write_linearized(&reopened, &opts()).unwrap()).unwrap();
    qpdf_check(&path, None);
    // Edits in the overlay on top of a parsed file, and a /ModDate stamped on save.
    let mut edited = reopened.clone();
    let first = page_refs(&edited)[0];
    edited.update_dict(first, |d| d.set(b"Rotate".to_vec(), Object::Int(90))).unwrap();
    let path = scratch("edited.pdf");
    let stamped = SaveOptions { mod_date: Some("D:20261018120000Z".into()), ..opts() };
    std::fs::write(&path, write_linearized(&edited, &stamped).unwrap()).unwrap();
    qpdf_check(&path, None);
    doc.require_full_save();
    assert!(write_linearized(&doc, &opts()).is_ok());
}

/// Document-level structures: an open action, viewer preferences, an outline, a form whose
/// field has a widget on page 2 (a kid with /Parent) and a merged field/widget on page 1, and
/// page thumbnails (not part of the page they show).
#[test]
fn qpdf_checks_forms_outlines_and_open_actions() {
    for mode in ["UseNone", "UseOutlines"] {
        let mut doc = fixture(4);
        let pages = page_refs(&doc);
        let root = doc.root().unwrap();
        let field = doc.add(dict(&[("FT", name("Tx")), ("T", Object::String(PdfString::literal(b"name".to_vec())))]));
        let widget = doc.add(dict(&[
            ("Type", name("Annot")),
            ("Subtype", name("Widget")),
            ("Rect", rect(80, 30)),
            ("Parent", Object::Ref(field)),
            ("P", Object::Ref(pages[1])),
        ]));
        doc.update_dict(field, |d| d.set(b"Kids".to_vec(), Object::Array(vec![Object::Ref(widget)]))).unwrap();
        let merged = doc.add(dict(&[
            ("Type", name("Annot")),
            ("Subtype", name("Widget")),
            ("FT", name("Btn")),
            ("T", Object::String(PdfString::literal(b"check".to_vec()))),
            ("Rect", rect(20, 20)),
            ("P", Object::Ref(pages[0])),
        ]));
        doc.update_dict(pages[1], |d| d.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(widget)]))).unwrap();
        doc.update_dict(pages[0], |d| d.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(merged)]))).unwrap();
        let acroform = doc.add(dict(&[("Fields", Object::Array(vec![Object::Ref(field), Object::Ref(merged)]))]));
        let outlines = doc.add(dict(&[("Type", name("Outlines")), ("Count", Object::Int(1))]));
        let item = doc.add(dict(&[
            ("Title", Object::String(PdfString::literal(b"Third".to_vec()))),
            ("Parent", Object::Ref(outlines)),
            ("Dest", Object::Array(vec![Object::Ref(pages[2]), name("Fit")])),
        ]));
        doc.update_dict(outlines, |d| {
            d.set(b"First".to_vec(), Object::Ref(item));
            d.set(b"Last".to_vec(), Object::Ref(item));
        })
        .unwrap();
        for (i, page) in pages.iter().enumerate() {
            let thumb = image(&mut doc, [i as u8 * 40, 90, 90]);
            doc.update_dict(*page, |d| d.set(b"Thumb".to_vec(), Object::Ref(thumb))).unwrap();
        }
        let prefs = doc.add(dict(&[("DisplayDocTitle", Object::Bool(true))]));
        doc.update_dict(root, |d| {
            d.set(b"AcroForm".to_vec(), Object::Ref(acroform));
            d.set(b"Outlines".to_vec(), Object::Ref(outlines));
            d.set(b"PageMode".to_vec(), name(mode));
            d.set(b"ViewerPreferences".to_vec(), Object::Ref(prefs));
            d.set(b"OpenAction".to_vec(), Object::Array(vec![Object::Ref(pages[0]), name("Fit")]));
        })
        .unwrap();
        let path = scratch(&format!("structures-{mode}.pdf"));
        std::fs::write(&path, write_linearized(&doc, &opts()).unwrap()).unwrap();
        qpdf_check(&path, None);
    }
}

fn page_refs(doc: &Document) -> Vec<ObjRef> {
    let pages = doc.get(pages_root(doc));
    pages.as_dict().unwrap().get(b"Kids").unwrap().as_array().unwrap().iter().map(|k| k.as_ref().unwrap()).collect()
}

const ALL: [Algorithm; 4] = [Algorithm::Rc4_40, Algorithm::Rc4_128, Algorithm::Aes128, Algorithm::Aes256];

fn encryption(alg: Algorithm) -> NewEncryption<'static> {
    NewEncryption { algorithm: alg, user_password: "user", owner_password: "owner", permissions: -1, encrypt_metadata: true, seed: [7; 32] }
}

#[test]
fn qpdf_checks_encrypted_linearization() {
    for alg in ALL {
        let mut doc = fixture(7);
        let expected = page_texts(&doc);
        doc.set_encryption(&encryption(alg)).unwrap();
        let bytes = write_linearized(&doc, &opts()).unwrap();
        assert!(is_linearized(&bytes), "{alg:?}");
        assert!(!String::from_utf8_lossy(&bytes).contains("Page 3"), "{alg:?}: plaintext leaked");
        let path = scratch(&format!("{alg:?}.pdf"));
        std::fs::write(&path, &bytes).unwrap();
        qpdf_check(&path, Some("user"));
        let bytes = Arc::new(bytes);
        assert!(matches!(Document::open(bytes.clone()), Err(CosError::NeedsPassword)), "{alg:?}");
        for pw in ["user", "owner"] {
            let back = Document::open_with_password(bytes.clone(), Some(pw)).unwrap();
            assert_eq!(page_texts(&back), expected, "{alg:?}/{pw}");
            assert!(back.repair_log().is_empty(), "{alg:?}/{pw}: {:?}", back.repair_log());
        }
    }
}

#[test]
fn an_encrypted_file_is_linearized_with_the_same_keys() {
    for alg in ALL {
        let mut doc = fixture(3);
        doc.set_encryption(&encryption(alg)).unwrap();
        let protected = Arc::new(write_full(&doc, &opts()).unwrap());
        let opened = Document::open_with_password(protected.clone(), Some("user")).unwrap();
        let bytes = write_linearized(&opened, &opts()).unwrap();
        let path = scratch(&format!("same-keys-{alg:?}.pdf"));
        std::fs::write(&path, &bytes).unwrap();
        qpdf_check(&path, Some("owner"));
        let back = Document::open_with_password(Arc::new(bytes), Some("owner")).unwrap();
        assert_eq!(page_texts(&back), page_texts(&opened), "{alg:?}");
        let encrypt = |d: &Document| d.resolve(d.trailer().get(b"Encrypt").unwrap()).as_dict().cloned().unwrap();
        assert_eq!(encrypt(&back), encrypt(&opened), "{alg:?}: the /Encrypt dictionary (and so the key) is unchanged");
        assert_eq!(back.trailer().get(b"ID"), opened.trailer().get(b"ID"), "{alg:?}: the /ID is part of the key");
    }
}

#[test]
fn the_layout_follows_annex_f() {
    let doc = fixture(7);
    let bytes = write_linearized(&doc, &opts()).unwrap();
    assert!(is_linearized(&bytes));
    let lin = lin_dict(&bytes);
    // Byte offsets must survive: replace every non-ASCII byte by one ASCII character.
    let text = String::from_utf8(bytes.iter().map(|b| if b.is_ascii() { *b } else { b'?' }).collect()).unwrap();
    assert!(text.find("endobj").unwrap() < 1024, "the linearization dictionary sits in the first 1024 bytes");
    assert_eq!(int(&lin, "L"), bytes.len() as i64);
    assert_eq!(int(&lin, "N"), 7);
    assert!(!text.contains("/ObjStm") && !text.contains("/XRef"), "no object streams, no xref streams");
    // The final startxref points at the first-page table; its /Prev at the main table.
    let tail = text.rfind("startxref").unwrap();
    let first_xref: usize = text[tail + 10..].lines().next().unwrap().trim().parse().unwrap();
    assert!(text[first_xref..].starts_with("xref\n"));
    let trailer_at = first_xref + text[first_xref..].find("trailer").unwrap();
    let prev_at = trailer_at + text[trailer_at..].find("/Prev ").unwrap() + 6;
    let prev: usize = text[prev_at..].split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap();
    assert!(text[prev..].starts_with("xref\n0 "), "the main table");
    assert!(!text[prev..].contains("/Prev"), "the main trailer ends the chain");
    // /T is the end-of-line before the main table's first entry.
    let t = int(&lin, "T") as usize;
    assert_eq!(&bytes[t..t + 1], b"\n");
    assert!(text[t + 1..].starts_with("0000000000 65535 f "));
    // /O is the first page's page object, page 2 is object 1, and /E ends the first page.
    let back = Document::open(Arc::new(bytes.clone())).unwrap();
    assert!(back.repair_log().is_empty(), "{:?}", back.repair_log());
    assert_eq!(back.revision_ends().len(), 1);
    let pages = page_refs(&back);
    assert_eq!(pages[0].num as i64, int(&lin, "O"));
    assert_eq!(pages[1].num, 1);
    let e = int(&lin, "E") as usize;
    assert!(text[..e].contains(&format!("\n{} 0 obj\n", pages[0].num)));
    assert!(text[e..].starts_with("1 0 obj\n"), "page 2 starts right after the first page");
    let hint = lin.get(b"H").unwrap().as_array().unwrap();
    let hint_at = hint[0].as_int().unwrap() as usize;
    let hint_len = hint[1].as_int().unwrap() as usize;
    assert!(text[hint_at..hint_at + hint_len].ends_with("endobj\n") && text[hint_at..].contains("/S "));
    assert_eq!(page_texts(&back), page_texts(&doc));
    assert_eq!(
        back.trailer().get(b"Info").map(|i| back.resolve(i).as_dict().unwrap().get(b"Title").cloned()),
        Some(Some(Object::String(PdfString::literal(b"Linearized fixture".to_vec()))))
    );
    assert!(matches!(back.trailer().get(b"ID"), Some(Object::Array(a)) if a.len() == 2), "an /ID is created");
    // Deterministic for unencrypted output.
    assert_eq!(write_linearized(&doc, &opts()).unwrap(), bytes);
}

#[test]
fn an_incremental_save_after_linearizing_is_not_linearized() {
    let bytes = write_linearized(&fixture(3), &opts()).unwrap();
    let mut doc = Document::open(Arc::new(bytes)).unwrap();
    let first = page_refs(&doc)[0];
    doc.update_dict(first, |d| d.set(b"Rotate".to_vec(), Object::Int(90))).unwrap();
    let updated = write_incremental(&doc, &opts()).unwrap();
    assert!(!is_linearized(&updated), "an appended revision makes the file no longer linearized");
    let back = Document::open(Arc::new(updated.clone())).unwrap();
    assert!(back.repair_log().is_empty(), "{:?}", back.repair_log());
    assert_eq!(back.get(page_refs(&back)[0]).as_dict().unwrap().int(b"Rotate"), Some(90));
    assert_eq!(page_texts(&back).len(), 3);
    let path = scratch("updated.pdf");
    std::fs::write(&path, &updated).unwrap();
    let qpdf = pdfcraft_oracle::or_skip!(pdfcraft_oracle::qpdf());
    let check = qpdf.command().arg("--check").arg(&path).output().unwrap();
    assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stdout));
}

#[test]
fn documents_that_cannot_be_linearized_are_refused_with_a_reason() {
    let empty = Document::new_empty();
    assert!(matches!(write_linearized(&empty, &opts()), Err(CosError::Linearize(m)) if m.contains("no pages")));
    let mut no_root = fixture(1);
    no_root.trailer_mut().remove(b"Root");
    assert!(matches!(write_linearized(&no_root, &opts()), Err(CosError::Linearize(_))));
    let bytes = Arc::new(write_full(&fixture(2), &opts()).unwrap());
    let limited = Document::open_with_stream_limit(bytes, None, 1024).unwrap();
    assert_eq!(write_linearized(&limited, &opts()), Err(CosError::ReadOnlyLimit));
}

#[test]
fn malformed_page_trees_still_produce_valid_files() {
    // A cycle (the page tree lists itself), a page listed twice, a dangling kid, a page with no
    // /Contents, and one whose /Contents is an indirect array of two streams.
    let mut doc = fixture(3);
    let pages = pages_root(&doc);
    let mut kids = page_refs(&doc);
    let bare = doc.add(dict(&[("Type", name("Page")), ("Parent", Object::Ref(pages))]));
    let a = doc.add(Object::Stream(Stream::flate(Dict::new(), b"BT /F1 9 Tf 10 10 Td (A) Tj ET")));
    let b = doc.add(Object::Stream(Stream::flate(Dict::new(), b"BT /F1 9 Tf 10 30 Td (B) Tj ET")));
    let array = doc.add(Object::Array(vec![Object::Ref(a), Object::Ref(b)]));
    let split = doc.add(dict(&[("Type", name("Page")), ("Parent", Object::Ref(pages)), ("Contents", Object::Ref(array))]));
    kids.extend([kids[0], ObjRef::new(9999, 0), pages, bare, split]);
    doc.update_dict(pages, |d| {
        d.set(b"Kids".to_vec(), Object::Array(kids.iter().map(|k| Object::Ref(*k)).collect()));
        d.set(b"Count".to_vec(), Object::Int(5));
    })
    .unwrap();
    let bytes = write_linearized(&doc, &opts()).unwrap();
    assert_eq!(int(&lin_dict(&bytes), "N"), 5);
    let back = Document::open(Arc::new(bytes.clone())).unwrap();
    assert!(back.repair_log().is_empty(), "{:?}", back.repair_log());
    assert!(is_linearized(&bytes));
    // The tree itself is kept as it was (a full save does not restructure it), so qpdf still
    // reports the loop; what matters here is that writing terminates and the file reopens.
    let kids = back.get(pages_root(&back)).as_dict().unwrap().get(b"Kids").unwrap().as_array().unwrap().clone();
    assert_eq!(kids.len(), 8);
    assert_eq!(kids[4], Object::Null, "the dangling kid");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]
    /// Random sharing patterns between 1–8 pages reopen with the same content, and qpdf (when
    /// installed) agrees with the hint tables.
    #[test]
    fn random_sharing_round_trips(n in 1usize..=8, uses in proptest::collection::vec(proptest::collection::vec(0usize..4, 0..4), 8)) {
        let mut doc = Document::new_empty();
        let pages = pages_root(&doc);
        let fonts: Vec<ObjRef> = (0..4).map(|i| doc.add(dict(&[("Type", name("Font")), ("Subtype", name("Type1")), ("BaseFont", name(["Helvetica", "Courier", "Times-Roman", "Symbol"][i]))]))).collect();
        let mut kids = Vec::new();
        for (i, used) in uses.iter().take(n).enumerate() {
            let mut font_dict = Dict::new();
            let mut ops = String::new();
            for f in used {
                font_dict.set(format!("F{f}").into_bytes(), Object::Ref(fonts[*f]));
                ops.push_str(&format!("BT /F{f} 9 Tf 10 {} Td (p{i} f{f}) Tj ET\n", 10 + 12 * f));
            }
            let content = doc.add(Object::Stream(Stream::flate(Dict::new(), ops.as_bytes())));
            kids.push(doc.add(dict(&[
                ("Type", name("Page")),
                ("Parent", Object::Ref(pages)),
                ("MediaBox", rect(100, 100)),
                ("Resources", Object::Dict(dict(&[("Font", Object::Dict(font_dict))]))),
                ("Contents", Object::Ref(content)),
            ])));
        }
        doc.update_dict(pages, |d| {
            d.set(b"Kids".to_vec(), Object::Array(kids.iter().map(|k| Object::Ref(*k)).collect()));
            d.set(b"Count".to_vec(), Object::Int(n as i64));
        }).unwrap();
        let bytes = write_linearized(&doc, &opts()).unwrap();
        prop_assert!(is_linearized(&bytes));
        let back = Document::open(Arc::new(bytes.clone())).unwrap();
        prop_assert!(back.repair_log().is_empty());
        prop_assert_eq!(page_texts(&back), page_texts(&doc));
        if let Ok(qpdf) = pdfcraft_oracle::qpdf() {
            let path = scratch(&format!("prop-{n}-{}.pdf", uses.iter().flatten().fold(0usize, |h, u| h * 5 + u + 1) % 1_000_007));
            std::fs::write(&path, &bytes).unwrap();
            let out = qpdf.command().arg("--check-linearization").arg(&path).output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            prop_assert!(out.status.success() && !text.contains("WARNING"), "{}", text);
        }
    }
}
