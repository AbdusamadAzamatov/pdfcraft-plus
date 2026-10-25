//! Convert to grayscale (CU3) on synthetic documents.

use std::sync::Arc;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, SaveOptions, Stream, write_full};
use pdfcraft_optimize::color::{Kept, KeptKind, to_gray};

fn add_page(doc: &mut Document, resources: Dict, content: &[u8]) -> ObjRef {
    let pages = doc.root().and_then(|r| doc.get(r).as_dict().and_then(|d| d.reference(b"Pages"))).unwrap();
    let c = doc.add(Object::Stream(Stream::from_raw(Dict::new(), content.to_vec())));
    let mut p = Dict::new();
    p.set(b"Type".to_vec(), Object::name("Page"));
    p.set(b"Parent".to_vec(), Object::Ref(pages));
    p.set(b"MediaBox".to_vec(), Object::Array(vec![0.into(), 0.into(), 200.into(), 200.into()]));
    p.set(b"Resources".to_vec(), Object::Dict(resources));
    p.set(b"Contents".to_vec(), Object::Ref(c));
    let r = doc.add(Object::Dict(p));
    doc.update_dict(pages, |d| {
        let mut kids = d.get(b"Kids").and_then(|k| k.as_array().cloned()).unwrap_or_default();
        kids.push(Object::Ref(r));
        d.set(b"Count".to_vec(), Object::Int(kids.len() as i64));
        d.set(b"Kids".to_vec(), Object::Array(kids));
    })
    .unwrap();
    r
}

fn dict(pairs: Vec<(&str, Object)>) -> Dict {
    let mut d = Dict::new();
    for (k, v) in pairs {
        d.set(k.as_bytes().to_vec(), v);
    }
    d
}

fn stream(doc: &Document, r: ObjRef) -> Stream {
    match &*doc.get(r) {
        Object::Stream(s) => s.clone(),
        _ => panic!("not a stream"),
    }
}

fn page_content(doc: &Document, page: ObjRef) -> String {
    let c = doc.get(page).as_dict().unwrap().reference(b"Contents").unwrap();
    String::from_utf8(stream(doc, c).decoded().unwrap()).unwrap()
}

fn ops(content: &str) -> Vec<String> {
    pdfcraft_content::parse(content.as_bytes())
        .ops
        .iter()
        .map(|o| String::from_utf8_lossy(&pdfcraft_content::serialize_ops(std::slice::from_ref(o))).trim().to_string())
        .collect()
}

/// Samples of a `w` × `h` colour photo-like picture (`n` = 3 RGB, 4 CMYK).
fn photo(w: u32, h: u32, n: usize) -> Vec<u8> {
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let t = ((x ^ y) & 7) as u8;
            px.push(((x * 255 / w) as u8).wrapping_add(t));
            px.push((y * 255 / h) as u8);
            px.push((((x + y) * 127 / (w + h)) as u8).wrapping_add(t));
            if n == 4 {
                px.push(20);
            }
        }
    }
    px
}

fn image(doc: &mut Document, w: u32, h: u32, cs: Object, data: Stream) -> ObjRef {
    let mut d = data.dict.clone();
    for (k, v) in [
        ("Type", Object::name("XObject")),
        ("Subtype", Object::name("Image")),
        ("Width", Object::Int(w as i64)),
        ("Height", Object::Int(h as i64)),
        ("BitsPerComponent", Object::Int(8)),
        ("ColorSpace", cs),
    ] {
        d.set(k.as_bytes().to_vec(), v);
    }
    doc.add(Object::Stream(Stream::from_raw(d, data.raw.to_vec())))
}

fn flate(px: &[u8]) -> Stream {
    Stream::flate(Dict::new(), px)
}

fn jpeg(px: &[u8], w: u32, h: u32) -> Stream {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95).encode(px, w, h, image::ExtendedColorType::Rgb8).unwrap();
    Stream::from_raw(dict(vec![("Filter", Object::name("DCTDecode"))]), out)
}

fn xobjects(pairs: &[(&str, ObjRef)]) -> Object {
    Object::Dict(dict(pairs.iter().map(|(n, r)| (*n, Object::Ref(*r))).collect()))
}

fn icc(doc: &mut Document, n: i64) -> Object {
    let s = doc.add(Object::Stream(Stream::from_raw(dict(vec![("N", Object::Int(n))]), vec![0; 16])));
    Object::Array(vec![Object::name("ICCBased"), Object::Ref(s)])
}

fn separation() -> Object {
    let f = dict(vec![
        ("FunctionType", Object::Int(2)),
        ("Domain", Object::Array(vec![0.into(), 1.into()])),
        ("C0", Object::Array(vec![0.into(), 0.into(), 0.into(), 0.into()])),
        ("C1", Object::Array(vec![1.into(), 0.into(), 0.into(), 0.into()])),
        ("N", Object::Int(1)),
    ]);
    Object::Array(vec![Object::name("Separation"), Object::name("Spot"), Object::name("DeviceCMYK"), Object::Dict(f)])
}

#[test]
fn colour_operators_become_gray_and_spot_colours_and_shadings_are_reported() {
    let mut doc = Document::new_empty();
    let cmyk_icc = icc(&mut doc, 4);
    let rgb_icc = icc(&mut doc, 3);
    let shading = dict(vec![
        ("ShadingType", Object::Int(2)),
        ("ColorSpace", Object::name("DeviceRGB")),
        ("Coords", Object::Array(vec![0.into(), 0.into(), 100.into(), 0.into()])),
    ]);
    let res = dict(vec![
        ("ColorSpace", Object::Dict(dict(vec![("C4", cmyk_icc), ("C3", rgb_icc), ("Sep", separation())]))),
        ("Shading", Object::Dict(dict(vec![("Sh0", Object::Dict(shading))]))),
    ]);
    let content = b"1 0 0 rg 0 0 1 RG 0 0 0 1 k 1 0 0 0 K\n\
        /DeviceRGB cs 0 1 0 sc /C4 CS 0 0 0 0 SCN /C3 cs 1 1 1 scn\n\
        q /Sep cs 0.5 scn 0 0 10 10 re f Q 0.5 g 0.25 G\n\
        /DeviceCMYK CS 0 0 0 0.5 SC /Sh0 sh (a) Tj BX unknownop EX";
    let page = add_page(&mut doc, res, content);
    let report = to_gray(&mut doc).unwrap();
    let got = ops(&page_content(&doc, page));
    assert_eq!(
        got,
        [
            "0.299 g",
            "0.114 G",
            "0 g",
            "0.7 G",
            "/DeviceGray cs",
            "0.587 sc",
            "/DeviceGray CS",
            "1 SCN",
            "/DeviceGray cs",
            "1 scn",
            "q",
            "/Sep cs",
            "0.5 scn",
            "0 0 10 10 re",
            "f",
            "Q",
            "0.5 g",
            "0.25 G",
            "/DeviceGray CS",
            "0.5 SC",
            "/Sh0 sh",
            "(a) Tj",
            "BX",
            "unknownop",
            "EX",
        ]
    );
    assert_eq!(report.colors, 8, "{report:?}");
    assert_eq!(report.streams, 1);
    assert_eq!(report.kept, [Kept { kind: KeptKind::Shading, page: Some(1), count: 1 }, Kept { kind: KeptKind::SpotColor, page: Some(1), count: 1 }]);
    assert_eq!(report.kept_lines(), ["1 shading on page 1 kept its colour", "1 spot colour on page 1 kept its colour"]);
}

#[test]
fn an_rgb_image_becomes_one_gray_component_of_the_same_size() {
    let mut doc = Document::new_empty();
    let (w, h) = (40, 30);
    let px = photo(w, h, 3);
    let smask = image(&mut doc, w, h, Object::name("DeviceGray"), flate(&vec![200; (w * h) as usize]));
    let rgb = image(&mut doc, w, h, Object::name("DeviceRGB"), flate(&px));
    doc.update_dict(rgb, |d| d.set(b"SMask".to_vec(), Object::Ref(smask))).ok();
    let photo_jpeg = image(&mut doc, w, h, Object::name("DeviceRGB"), jpeg(&px, w, h));
    let cmyk = image(&mut doc, w, h, Object::name("DeviceCMYK"), flate(&photo(w, h, 4)));
    add_page(&mut doc, dict(vec![("XObject", xobjects(&[("A", rgb), ("B", photo_jpeg), ("C", cmyk)]))]), b"q 40 0 0 30 0 0 cm /A Do /B Do /C Do Q");
    let report = to_gray(&mut doc).unwrap();
    assert_eq!(report.images, 3, "{report:?}");
    assert!(report.kept.is_empty(), "{report:?}");
    let a = stream(&doc, rgb);
    assert_eq!((a.dict.int(b"Width"), a.dict.int(b"Height")), (Some(w as i64), Some(h as i64)));
    assert_eq!(a.dict.name(b"ColorSpace"), Some(&b"DeviceGray"[..]));
    assert_eq!(a.dict.reference(b"SMask"), Some(smask), "the soft mask stays");
    let samples = a.decoded().unwrap();
    assert_eq!(samples.len(), (w * h) as usize, "one component");
    let [r, g, b] = [px[0], px[1], px[2]].map(f64::from);
    assert!((f64::from(samples[0]) - (0.299 * r + 0.587 * g + 0.114 * b)).abs() <= 1.0);
    let j = stream(&doc, photo_jpeg);
    assert_eq!(j.dict.name(b"Filter"), Some(&b"DCTDecode"[..]), "a JPEG stays a JPEG");
    let decoded = image::load_from_memory_with_format(&j.raw, image::ImageFormat::Jpeg).unwrap();
    assert_eq!(decoded.color(), image::ColorType::L8);
    assert_eq!((decoded.width(), decoded.height()), (w, h));
    let c = stream(&doc, cmyk);
    assert_eq!(c.dict.name(b"ColorSpace"), Some(&b"DeviceGray"[..]));
    assert_eq!(c.decoded().unwrap().len(), (w * h) as usize);
}

#[test]
fn an_indexed_image_gets_a_gray_palette_and_keeps_its_indices() {
    let mut doc = Document::new_empty();
    let palette = Object::Array(vec![
        Object::name("Indexed"),
        Object::name("DeviceRGB"),
        Object::Int(2),
        Object::String(pdfcraft_cos::PdfString::literal(vec![255, 0, 0, 0, 255, 0, 0, 0, 255])),
    ]);
    let indices = vec![0u8, 1, 2, 1];
    let im = image(&mut doc, 2, 2, palette, flate(&indices));
    let raw_before = stream(&doc, im).raw.to_vec();
    add_page(&mut doc, dict(vec![("XObject", xobjects(&[("I", im)]))]), b"/I Do");
    let report = to_gray(&mut doc).unwrap();
    assert_eq!((report.palettes, report.images), (1, 0), "{report:?}");
    let s = stream(&doc, im);
    assert_eq!(s.raw.to_vec(), raw_before, "the indices are untouched");
    let cs = s.dict.get(b"ColorSpace").unwrap().as_array().unwrap().clone();
    assert_eq!(cs[1].as_name(), Some(&b"DeviceGray"[..]));
    assert_eq!(cs[2].as_int(), Some(2));
    assert_eq!(cs[3].as_string().unwrap().bytes, [76, 150, 29]);
}

#[test]
fn a_separation_image_and_colour_are_reported_not_touched() {
    let mut doc = Document::new_empty();
    let sep = image(&mut doc, 8, 8, separation(), flate(&[128; 64]));
    let before = stream(&doc, sep);
    let res = dict(vec![("XObject", xobjects(&[("S", sep)])), ("ColorSpace", Object::Dict(dict(vec![("Sp", separation())])))]);
    add_page(&mut doc, Dict::new(), b"0 0 1 rg");
    let page = add_page(&mut doc, res, b"/Sp cs 1 scn 0 0 5 5 re f /S Do");
    let report = to_gray(&mut doc).unwrap();
    assert_eq!(stream(&doc, sep), before);
    assert_eq!(ops(&page_content(&doc, page))[..2], ["/Sp cs", "1 scn"]);
    assert_eq!(report.kept, [Kept { kind: KeptKind::SpotColor, page: Some(2), count: 1 }, Kept { kind: KeptKind::Image, page: Some(2), count: 1 }]);
    assert_eq!(report.kept_lines()[1], "1 image on page 2 kept its colour");
}

#[test]
fn forms_appearances_and_type3_glyphs_are_converted_once() {
    let mut doc = Document::new_empty();
    // A form drawn twice and nested in another one.
    let inner = doc.add(Object::Stream(Stream::from_raw(
        dict(vec![
            ("Type", Object::name("XObject")),
            ("Subtype", Object::name("Form")),
            ("BBox", Object::Array(vec![0.into(), 0.into(), 10.into(), 10.into()])),
        ]),
        b"1 0 0 rg 0 0 10 10 re f".to_vec(),
    )));
    let outer = doc.add(Object::Stream(Stream::from_raw(
        dict(vec![
            ("Subtype", Object::name("Form")),
            ("BBox", Object::Array(vec![0.into(), 0.into(), 10.into(), 10.into()])),
            ("Resources", Object::Dict(dict(vec![("XObject", xobjects(&[("In", inner)]))]))),
        ]),
        b"/In Do 0 1 0 sc".to_vec(),
    )));
    // Type 3: a d0 glyph takes colours, a d1 glyph must not set any and is left alone.
    let d0 = doc.add(Object::Stream(Stream::from_raw(Dict::new(), b"1000 0 d0 0 0 1 rg 0 0 500 500 re f".to_vec())));
    let d1 = doc.add(Object::Stream(Stream::from_raw(Dict::new(), b"1000 0 0 0 750 750 d1 1 0 0 rg 0 0 500 500 re f".to_vec())));
    let font = doc.add(Object::Dict(dict(vec![
        ("Type", Object::name("Font")),
        ("Subtype", Object::name("Type3")),
        ("CharProcs", Object::Dict(dict(vec![("a", Object::Ref(d0)), ("b", Object::Ref(d1))]))),
    ])));
    // An annotation with an appearance and a colour.
    let ap = doc.add(Object::Stream(Stream::from_raw(
        dict(vec![("Subtype", Object::name("Form")), ("BBox", Object::Array(vec![0.into(), 0.into(), 10.into(), 10.into()]))]),
        b"0 0 1 RG 0 0 m 10 10 l S".to_vec(),
    )));
    let annot = doc.add(Object::Dict(dict(vec![
        ("Subtype", Object::name("Square")),
        ("Rect", Object::Array(vec![0.into(), 0.into(), 10.into(), 10.into()])),
        ("C", Object::Array(vec![0.into(), 0.into(), 1.into()])),
        ("AP", Object::Dict(dict(vec![("N", Object::Ref(ap))]))),
    ])));
    let res = dict(vec![("XObject", xobjects(&[("Out", outer), ("In", inner)])), ("Font", Object::Dict(dict(vec![("T3", Object::Ref(font))])))]);
    let page = add_page(&mut doc, res, b"/Out Do /In Do BT /T3 12 Tf (ab) Tj ET");
    doc.update_dict(page, |d| d.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(annot)]))).unwrap();
    let report = to_gray(&mut doc).unwrap();
    let text = |r| String::from_utf8(stream(&doc, r).decoded().unwrap()).unwrap();
    assert_eq!(ops(&text(inner)), ["0.299 g", "0 0 10 10 re", "f"]);
    assert_eq!(ops(&text(outer)), ["/In Do", "0.587 sc"], "a colour in the caller's space is read by its count");
    assert_eq!(ops(&text(d0))[1], "0.114 g");
    assert_eq!(text(d1), "1000 0 0 0 750 750 d1 1 0 0 rg 0 0 500 500 re f", "d1 glyphs are untouched");
    assert_eq!(ops(&text(ap))[0], "0.114 G");
    assert_eq!(doc.get(annot).as_dict().unwrap().get(b"C"), Some(&Object::Array(vec![Object::Real(0.114)])));
    // inner, outer, d0, the appearance; the page content had nothing to change.
    assert_eq!(report.streams, 4, "{report:?}");
    assert_eq!(report.colors, 5, "{report:?}");
}

#[test]
fn inline_images_and_colour_key_masks_turn_gray() {
    let mut doc = Document::new_empty();
    let mut content = b"q 20 0 0 20 0 0 cm BI /W 2 /H 1 /CS /RGB /BPC 8 ID ".to_vec();
    content.extend_from_slice(&[255, 0, 0, 0, 0, 255]);
    content.extend_from_slice(b" EI Q");
    // An RGB image whose pure-red pixels are transparent.
    let (w, h) = (4, 1);
    let keyed = image(&mut doc, w, h, Object::name("DeviceRGB"), flate(&[255, 0, 0, 0, 255, 0, 255, 0, 0, 0, 0, 0]));
    doc.update_dict(keyed, |d| d.set(b"Mask".to_vec(), Object::Array(vec![250.into(), 255.into(), 0.into(), 5.into(), 0.into(), 5.into()]))).unwrap();
    content.extend_from_slice(b" /K Do");
    let page = add_page(&mut doc, dict(vec![("XObject", xobjects(&[("K", keyed)]))]), &content);
    let report = to_gray(&mut doc).unwrap();
    assert_eq!(report.images, 2, "{report:?}");
    let c = doc.get(page).as_dict().unwrap().reference(b"Contents").unwrap();
    let parsed = pdfcraft_content::parse(&stream(&doc, c).decoded().unwrap());
    let bi = parsed.ops.iter().find(|o| o.is("BI")).unwrap();
    let (d, data) = bi.inline.as_ref().unwrap();
    assert_eq!(d.get(b"CS").and_then(Object::as_name), Some(&b"G"[..]));
    assert_eq!(data, &[76, 29]);
    let k = stream(&doc, keyed);
    assert!(!k.dict.contains(b"Mask"), "the colour key named RGB colours");
    let m = stream(&doc, k.dict.reference(b"SMask").unwrap());
    assert_eq!(m.decoded().unwrap(), [0, 255, 0, 255]);
}

#[test]
fn malformed_and_cyclic_content_does_not_crash() {
    let mut doc = Document::new_empty();
    // A form drawing itself, forms nested past the limit, operands of the wrong kind.
    let selfie = doc.add(Object::Null);
    let res = dict(vec![("XObject", xobjects(&[("Me", selfie)]))]);
    doc.set(
        selfie,
        Object::Stream(Stream::from_raw(
            dict(vec![("Subtype", Object::name("Form")), ("Resources", Object::Dict(res))]),
            b"/Me Do 1 0 0 rg".to_vec(),
        )),
    );
    let mut deepest = doc.add(Object::Stream(Stream::from_raw(dict(vec![("Subtype", Object::name("Form"))]), b"1 0 0 rg".to_vec())));
    let mut chain = vec![deepest];
    for _ in 0..30 {
        let res = dict(vec![("XObject", xobjects(&[("N", deepest)]))]);
        deepest = doc.add(Object::Stream(Stream::from_raw(
            dict(vec![("Subtype", Object::name("Form")), ("Resources", Object::Dict(res))]),
            b"/N Do".to_vec(),
        )));
        chain.push(deepest);
    }
    let res = dict(vec![("XObject", xobjects(&[("S", selfie), ("D", deepest)]))]);
    add_page(&mut doc, res, b"/S Do /D Do (x) 1 rg /Nope cs 1 2 3 4 5 sc 1e308 0 0 rg 0 0 0 k K /Pattern cs /P0 scn q q q Q Q Q Q Q");
    let report = to_gray(&mut doc).unwrap();
    assert!(report.colors >= 2, "{report:?}");
    assert_eq!(String::from_utf8(stream(&doc, chain[0]).decoded().unwrap()).unwrap(), "1 0 0 rg", "past the depth limit");
    assert!(report.kept.iter().any(|k| k.kind == KeptKind::Pattern));
    // No page tree: an error, not a panic.
    let mut bare = Document::new_empty();
    let root = bare.root().unwrap();
    bare.update_dict(root, |c| {
        c.remove(b"Pages");
    })
    .unwrap();
    assert!(to_gray(&mut bare).is_err());
}

/// Every pixel of every page of `bytes` is gray (R = G = B, ±1).
fn assert_all_gray(bytes: Vec<u8>, pages: usize) {
    let mut r = pdfcraft_render::PageRenderer::new(Arc::new(bytes), pdfcraft_render::RenderConfig::default());
    for page in 0..pages {
        let out = r.render(pdfcraft_render::RenderRequest { page, scale: 1.0, ..Default::default() });
        assert!(out.error.is_none(), "{:?}", out.error);
        let mut colour = 0;
        for [r, g, b, _] in out.rgba.as_chunks::<4>().0 {
            let (lo, hi) = (*r.min(g).min(b), *r.max(g).max(b));
            if hi - lo > 1 {
                colour += 1;
            }
        }
        assert_eq!(colour, 0, "page {page}: {colour} coloured pixels");
    }
}

fn colourful_document() -> Document {
    let mut doc = Document::new_empty();
    let (w, h) = (64, 48);
    let px = photo(w, h, 3);
    let rgb = image(&mut doc, w, h, Object::name("DeviceRGB"), flate(&px));
    let photo_jpeg = image(&mut doc, w, h, Object::name("DeviceRGB"), jpeg(&px, w, h));
    let cmyk = image(&mut doc, w, h, Object::name("DeviceCMYK"), flate(&photo(w, h, 4)));
    let icc3 = icc(&mut doc, 3);
    let indexed = Object::Array(vec![
        Object::name("Indexed"),
        icc3.clone(),
        Object::Int(1),
        Object::String(pdfcraft_cos::PdfString::literal(vec![255, 128, 0, 0, 128, 255])),
    ]);
    let pal = image(&mut doc, 2, 1, indexed, flate(&[0, 1]));
    let res = dict(vec![
        ("XObject", xobjects(&[("A", rgb), ("B", photo_jpeg), ("C", cmyk), ("P", pal)])),
        ("ColorSpace", Object::Dict(dict(vec![("RGB3", icc3)]))),
    ]);
    add_page(
        &mut doc,
        res,
        b"1 0 0 rg 0 0 50 50 re f 0 0.5 1 RG 4 w 60 60 m 190 190 l S 0.2 0.9 0.1 0 k 0 100 50 50 re f\n\
          /RGB3 cs 0.9 0.2 0.5 sc 100 0 40 40 re f\n\
          q 64 0 0 48 100 100 cm /A Do Q q 64 0 0 48 0 150 cm /B Do Q q 30 0 0 30 150 50 cm /C Do Q q 40 0 0 20 60 50 cm /P Do Q\n\
          BT /F1 1 Tf ET",
    );
    doc
}

#[test]
fn the_result_renders_entirely_in_gray() {
    let mut doc = colourful_document();
    let before = write_full(&doc, &SaveOptions::default()).unwrap();
    // The fixture really is in colour.
    let mut r = pdfcraft_render::PageRenderer::new(Arc::new(before), pdfcraft_render::RenderConfig::default());
    let out = r.render(pdfcraft_render::RenderRequest { page: 0, scale: 1.0, ..Default::default() });
    assert!(out.rgba.as_chunks::<4>().0.iter().any(|p| p[0].abs_diff(p[1]) > 10));
    let report = to_gray(&mut doc).unwrap();
    assert!(report.kept.is_empty(), "{report:?}");
    assert_all_gray(write_full(&doc, &SaveOptions::default()).unwrap(), 1);
}

#[test]
fn a_colour_photo_gets_smaller() {
    let mut doc = Document::new_empty();
    let (w, h) = (300, 200);
    let px = photo(w, h, 3);
    let a = image(&mut doc, w, h, Object::name("DeviceRGB"), flate(&px));
    let b = image(&mut doc, w, h, Object::name("DeviceRGB"), jpeg(&px, w, h));
    add_page(&mut doc, dict(vec![("XObject", xobjects(&[("A", a), ("B", b)]))]), b"q 150 0 0 100 0 0 cm /A Do Q q 150 0 0 100 0 100 cm /B Do Q");
    let before = write_full(&doc, &SaveOptions::default()).unwrap().len();
    to_gray(&mut doc).unwrap();
    let after = write_full(&doc, &SaveOptions::default()).unwrap();
    assert!(after.len() * 2 < before, "{before} → {}", after.len());
    assert_all_gray(after, 1);
}
