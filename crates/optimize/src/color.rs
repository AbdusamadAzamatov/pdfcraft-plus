//! Convert to grayscale (execution plan CU3): every colour in the document becomes a shade of
//! gray, as vector operators and re-encoded images, so the file reads and prints in black and
//! white and usually gets smaller.
//!
//! - **Formulas** (ISO 32000-2 §10.4.2): RGB → gray is the Rec. 601 luma
//!   `0.299 R + 0.587 G + 0.114 B` (as Print ▸ grayscale); CMYK → gray is
//!   `1 − min(1, 0.3 C + 0.59 M + 0.11 Y + K)`. ICC-based spaces with 3 or 4 components and
//!   CalRGB are read as RGB and CMYK.
//! - **Content streams** (pages, form XObjects, annotation appearances, Type 3 glyphs defined
//!   with `d0`; forms nested up to 12 deep): `rg`/`RG` and `k`/`K` become `g`/`G`; `cs`/`CS`
//!   selecting an RGB or CMYK space become `/DeviceGray cs`, and their `sc`/`scn` take one
//!   component. A form, appearance or glyph that sets a colour without choosing a space first
//!   draws in its caller's space, which this has made gray: 3 numbers are read as RGB and 4 as
//!   CMYK there. Glyphs defined with `d1` must not set colours and are left as they are.
//!   Inline images are converted like image XObjects when unfiltered or Flate.
//! - **Images**: 8-bit RGB and CMYK images (Flate or other lossless filters, and RGB JPEGs) are
//!   decoded, converted and stored again: Flate, or JPEG at the optimizer's quality for kept
//!   JPEGs ([`crate::Compression::Retain`]). Indexed images keep their indices and get a gray
//!   palette. `/SMask` and `/Mask` stay; a colour-key `/Mask` array (which names colours that
//!   no longer exist) becomes an equivalent soft mask, and a soft mask's `/Matte` turns gray.
//! - **Left alone and reported** ([`Kept`]): shadings, patterns, spot colours (Separation and
//!   DeviceN), Lab colours, other colour spaces, and images it can't decode faithfully (CMYK
//!   JPEGs, JPEG 2000, 16-bit samples, decode arrays).
//!
//! Every image in the file is converted once, shared or not; that is why the scope is the
//! whole document.

use std::collections::{BTreeMap, HashMap, HashSet};

use pdfcraft_content::{Op, Pieces, num, write_op};
use pdfcraft_cos::{DecodeBudget, Dict, Document, ObjRef, Object, PdfString, Stream};

use crate::images::{content_bytes_list, is_dct, jpeg_retain};
use crate::{OptimizeError, images::page_resources};

/// Forms nested deeper than this aren't followed (as the optimizer's measurement).
const MAX_FORM_DEPTH: usize = 12;
/// Images with more pixels than this are left alone (memory), as the optimizer.
const MAX_PIXELS: u64 = 80_000_000;

/// What [`to_gray`] left in colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeptKind {
    /// Smooth shadings painted with `sh`.
    Shading,
    /// Pattern fills and strokes (shading and tiling patterns).
    Pattern,
    /// Separation and DeviceN colours (spot inks).
    SpotColor,
    /// CIE Lab colours.
    LabColor,
    /// Colours in another space (Indexed vector colours, unknown names).
    OtherColor,
    /// Image XObjects that could not be decoded faithfully, or in another colour space.
    Image,
    /// Inline images that could not be converted.
    InlineImage,
    /// Page content that could not be decoded within the size limits, left as it was.
    PageContent,
}

impl KeptKind {
    /// The name tools use.
    pub fn name(self) -> &'static str {
        match self {
            KeptKind::Shading => "shading",
            KeptKind::Pattern => "pattern",
            KeptKind::SpotColor => "spot_color",
            KeptKind::LabColor => "lab_color",
            KeptKind::OtherColor => "other_color",
            KeptKind::Image => "image",
            KeptKind::InlineImage => "inline_image",
            KeptKind::PageContent => "page_content",
        }
    }
}

/// How many things of one kind kept their colour on one page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    pub kind: KeptKind,
    /// 1-based page where it was found first; `None` for images no page draws directly.
    pub page: Option<usize>,
    pub count: usize,
}

/// What [`to_gray`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrayReport {
    /// Colour operators and annotation colours made gray.
    pub colors: usize,
    /// Content streams rewritten.
    pub streams: usize,
    /// Images decoded and stored again as gray (XObjects and inline images).
    pub images: usize,
    /// Indexed images whose palette became gray.
    pub palettes: usize,
    /// What kept its colour, by kind, then page.
    pub kept: Vec<Kept>,
}

impl GrayReport {
    /// Whether anything changed.
    pub fn changed(&self) -> bool {
        self.colors + self.streams + self.images + self.palettes > 0
    }

    /// One English line per [`Kept`] entry, e.g. "2 shadings on page 4 kept their colour".
    pub fn kept_lines(&self) -> Vec<String> {
        self.kept
            .iter()
            .map(|k| {
                let (one, many) = match k.kind {
                    KeptKind::Shading => ("shading", "shadings"),
                    KeptKind::Pattern => ("pattern", "patterns"),
                    KeptKind::SpotColor => ("spot colour", "spot colours"),
                    KeptKind::LabColor => ("Lab colour", "Lab colours"),
                    KeptKind::OtherColor => ("other colour", "other colours"),
                    KeptKind::Image => ("image", "images"),
                    KeptKind::InlineImage => ("inline image", "inline images"),
                    KeptKind::PageContent => ("page content", "page contents"),
                };
                let what = if k.count == 1 { one } else { many };
                let verb = if k.count == 1 { "kept its colour" } else { "kept their colour" };
                match k.page {
                    Some(p) => format!("{} {what} on page {p} {verb}", k.count),
                    None => format!("{} {what} {verb}", k.count),
                }
            })
            .collect()
    }
}

/// RGB (0–1) to gray: Rec. 601 luma.
pub fn rgb_to_gray(r: f64, g: f64, b: f64) -> f64 {
    (0.299 * unit(r) + 0.587 * unit(g) + 0.114 * unit(b)).clamp(0.0, 1.0)
}

/// A component clamped to 0–1 (NaN reads as 0), so every result is finite.
fn unit(x: f64) -> f64 {
    if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) }
}

/// CMYK (0–1) to gray (ISO 32000-2 §10.4.2.4 and §10.4.2.2).
pub fn cmyk_to_gray(c: f64, m: f64, y: f64, k: f64) -> f64 {
    1.0 - (0.3 * unit(c) + 0.59 * unit(m) + 0.11 * unit(y) + unit(k)).min(1.0)
}

fn rgb8(r: u8, g: u8, b: u8) -> u8 {
    // Integer Rec. 601 with rounding: exact for gray inputs.
    ((299 * r as u32 + 587 * g as u32 + 114 * b as u32 + 500) / 1000).min(255) as u8
}

fn cmyk8(c: u8, m: u8, y: u8, k: u8) -> u8 {
    let ink = 30 * c as u32 + 59 * m as u32 + 11 * y as u32 + 100 * k as u32;
    let ink = ink.min(25_500);
    ((25_500 - ink + 50) / 100) as u8
}

/// A colour space, as far as conversion cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Space {
    /// Not chosen in this stream: the caller's (which this conversion has made gray).
    Inherited,
    Gray,
    Rgb,
    Cmyk,
    Pattern,
    Spot,
    Lab,
    Other,
}

/// Classify a colour space object (a name or an array).
fn classify(doc: &Document, cs: &Object, depth: usize) -> Space {
    let cs = doc.resolve(cs);
    match &*cs {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"G" | b"CalGray" => Space::Gray,
            b"DeviceRGB" | b"RGB" => Space::Rgb,
            b"DeviceCMYK" | b"CMYK" => Space::Cmyk,
            b"Pattern" => Space::Pattern,
            _ => Space::Other,
        },
        Object::Array(a) => match a.first().map(|f| doc.resolve(f)).as_deref().and_then(Object::as_name) {
            Some(b"CalGray") => Space::Gray,
            Some(b"CalRGB") => Space::Rgb,
            Some(b"ICCBased") => match a.get(1).map(|s| doc.resolve(s)).as_deref() {
                Some(Object::Stream(icc)) => match icc.dict.int(b"N") {
                    Some(1) => Space::Gray,
                    Some(3) => Space::Rgb,
                    Some(4) => Space::Cmyk,
                    // An unusable profile: its alternate space, as readers fall back to.
                    _ => match icc.dict.get(b"Alternate") {
                        Some(alt) if depth < 4 => classify(doc, alt, depth + 1),
                        _ => Space::Other,
                    },
                },
                _ => Space::Other,
            },
            Some(b"Separation" | b"DeviceN" | b"NChannel") => Space::Spot,
            Some(b"Lab") => Space::Lab,
            Some(b"Pattern") => Space::Pattern,
            _ => Space::Other,
        },
        _ => Space::Other,
    }
}

/// A colour space operand of `cs`/`CS` (or an inline image's `/CS`): a device name, or a name in
/// the resources' `/ColorSpace` dictionary.
fn space_named(doc: &Document, resources: &Dict, name: &[u8]) -> Space {
    match name {
        b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" | b"Pattern" => classify(doc, &Object::Name(name.to_vec()), 0),
        _ => match named_space(doc, resources, name) {
            Some(o) => classify(doc, &o, 0),
            None => Space::Other,
        },
    }
}

fn named_space(doc: &Document, resources: &Dict, name: &[u8]) -> Option<Object> {
    let spaces = resources.get(b"ColorSpace").map(|c| doc.resolve(c))?;
    spaces.as_dict()?.get(name).cloned()
}

/// Graphics state the conversion follows: the fill and stroke colour spaces.
#[derive(Clone, Copy)]
struct ColorState {
    fill: Space,
    stroke: Space,
}

/// One conversion pass over a document.
struct Gray<'a> {
    doc: &'a mut Document,
    /// Forms, appearance streams and glyphs already converted (each is converted once).
    done: HashSet<ObjRef>,
    /// Font dictionaries whose glyphs were already looked at.
    fonts: HashSet<ObjRef>,
    /// The first page drawing each image XObject.
    image_page: HashMap<ObjRef, usize>,
    kept: BTreeMap<(KeptKind, Option<usize>), usize>,
    report: GrayReport,
}

/// Convert every colour of `doc` to gray, in place. See the module documentation.
pub fn to_gray(doc: &mut Document) -> Result<GrayReport, OptimizeError> {
    let pages = pdfcraft_annot::page_refs(doc).map_err(|_| OptimizeError::NoPages)?;
    let mut g =
        Gray { doc, done: HashSet::new(), fonts: HashSet::new(), image_page: HashMap::new(), kept: BTreeMap::new(), report: GrayReport::default() };
    for (i, p) in pages.iter().enumerate() {
        g.page(*p, i.saturating_add(1))?;
    }
    g.images();
    let Gray { kept, mut report, .. } = g;
    report.kept = kept.into_iter().map(|((kind, page), count)| Kept { kind, page, count }).collect();
    Ok(report)
}

impl Gray<'_> {
    fn keep(&mut self, kind: KeptKind, page: usize) {
        *self.kept.entry((kind, (page > 0).then_some(page))).or_insert(0) += 1;
    }

    fn page(&mut self, page: ObjRef, number: usize) -> Result<(), OptimizeError> {
        let Some(dict) = self.doc.get(page).as_dict().cloned() else { return Ok(()) };
        let resources = page_resources(self.doc, page).unwrap_or_default();
        if let Some(contents) = dict.get(b"Contents") {
            let refs: Vec<ObjRef> = match contents {
                Object::Ref(r) => match &*self.doc.get(*r) {
                    // An array of streams stored as its own object.
                    Object::Array(a) => a.iter().filter_map(Object::as_ref).collect(),
                    _ => vec![*r],
                },
                Object::Array(a) => a.iter().filter_map(Object::as_ref).collect(),
                _ => Vec::new(),
            };
            let Some(datas) = content_bytes_list(self.doc, &refs, &mut DecodeBudget::page()) else {
                self.keep(KeptKind::PageContent, number);
                return Ok(());
            };
            let pieces = Pieces::join(&datas);
            let edits = self.content(pieces.data(), &resources, number, 0, false);
            if !edits.is_empty() {
                let new = pieces.splice(edits);
                for ((r, old), new) in refs.iter().zip(&datas).zip(new) {
                    if *old != new {
                        self.rewrite(*r, &new);
                    }
                }
            }
        }
        // Annotation appearances and colours.
        let annots = dict.get(b"Annots").map(|a| self.doc.resolve(a)).and_then(|a| a.as_array().cloned()).unwrap_or_default();
        for a in annots {
            let Some(ar) = a.as_ref() else { continue };
            let Some(ad) = self.doc.get(ar).as_dict().cloned() else { continue };
            let mut colors = 0;
            let mut changed = Dict::new();
            for key in [&b"C"[..], b"IC"] {
                if let Some(Object::Array(c)) = ad.get(key).map(|c| self.doc.resolve(c)).as_deref()
                    && let Some(v) = gray_of_components(c)
                {
                    changed.set(key.to_vec(), Object::Array(vec![num(v)]));
                    colors += 1;
                }
            }
            if colors > 0 {
                self.doc.update_dict(ar, |d| {
                    for (k, v) in changed.iter() {
                        d.set(k.clone(), v.clone());
                    }
                })?;
                self.report.colors += colors;
            }
            let Some(ap) = ad.get(b"AP").map(|a| self.doc.resolve(a)).and_then(|a| a.as_dict().cloned()) else { continue };
            for (_, v) in ap.iter() {
                let streams: Vec<ObjRef> = match v {
                    Object::Ref(r) => match &*self.doc.get(*r) {
                        Object::Stream(_) => vec![*r],
                        Object::Dict(states) => states.iter().filter_map(|(_, s)| s.as_ref()).collect(),
                        _ => Vec::new(),
                    },
                    Object::Dict(states) => states.iter().filter_map(|(_, s)| s.as_ref()).collect(),
                    _ => Vec::new(),
                };
                for s in streams {
                    self.form(s, &Dict::new(), number, 0);
                }
            }
        }
        Ok(())
    }

    /// Replace stream `r`'s data with `data` (Flate), keeping its dictionary.
    fn rewrite(&mut self, r: ObjRef, data: &[u8]) {
        let obj = self.doc.get(r);
        let Object::Stream(s) = &*obj else { return };
        let mut d = s.dict.clone();
        for k in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL"] {
            d.remove(k);
        }
        self.doc.set(r, Object::Stream(Stream::flate(d, data)));
        self.report.streams += 1;
    }

    /// Convert form (or appearance stream, or glyph) `r` once, with its own resources or
    /// `caller`'s. `glyph`: a Type 3 glyph, left alone when it is defined with `d1`.
    fn form_or_glyph(&mut self, r: ObjRef, caller: &Dict, page: usize, depth: usize, glyph: bool) {
        if depth >= MAX_FORM_DEPTH || !self.done.insert(r) {
            return;
        }
        let obj = self.doc.get(r);
        let Object::Stream(s) = &*obj else { return };
        let Ok(data) = s.decoded() else { return };
        let own = s.dict.get(b"Resources").map(|x| self.doc.resolve(x)).and_then(|x| x.as_dict().cloned());
        let resources = own.unwrap_or_else(|| caller.clone());
        let pieces = Pieces::join(&[&data]);
        let edits = self.content(pieces.data(), &resources, page, depth.saturating_add(1), glyph);
        if edits.is_empty() {
            return;
        }
        if let Some(new) = pieces.splice(edits).into_iter().next()
            && new != data
        {
            self.rewrite(r, &new);
        }
    }

    fn form(&mut self, r: ObjRef, caller: &Dict, page: usize, depth: usize) {
        self.form_or_glyph(r, caller, page, depth, false);
    }

    /// The glyphs of the Type 3 fonts in `resources`.
    fn type3_glyphs(&mut self, resources: &Dict, page: usize, depth: usize) {
        let fonts = resources.get(b"Font").map(|f| self.doc.resolve(f)).and_then(|f| f.as_dict().cloned()).unwrap_or_default();
        for (_, f) in fonts.iter() {
            let Some(fr) = f.as_ref() else { continue };
            if !self.fonts.insert(fr) {
                continue;
            }
            let Some(fd) = self.doc.get(fr).as_dict().cloned() else { continue };
            if fd.name(b"Subtype") != Some(b"Type3") {
                continue;
            }
            let own = fd.get(b"Resources").map(|x| self.doc.resolve(x)).and_then(|x| x.as_dict().cloned());
            let glyph_resources = own.unwrap_or_else(|| resources.clone());
            let procs = fd.get(b"CharProcs").map(|c| self.doc.resolve(c)).and_then(|c| c.as_dict().cloned()).unwrap_or_default();
            for (_, p) in procs.iter() {
                if let Some(pr) = p.as_ref() {
                    self.form_or_glyph(pr, &glyph_resources, page, depth, true);
                }
            }
        }
    }

    /// Convert one content stream (`data`, already joined): the edits to splice in, as (joined
    /// range, replacement). Forms and glyphs it draws are converted on the way.
    fn content(&mut self, data: &[u8], resources: &Dict, page: usize, depth: usize, glyph: bool) -> Vec<(std::ops::Range<usize>, Vec<u8>)> {
        let parsed = pdfcraft_content::parse(data);
        // A glyph defined with d1 must not set colours (ISO 32000-2 §9.6.4): leave it be.
        if glyph && parsed.ops.first().is_some_and(|o| o.is("d1")) {
            return Vec::new();
        }
        self.type3_glyphs(resources, page, depth);
        let xobjects = resources.get(b"XObject").map(|x| self.doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
        let mut stack = vec![ColorState { fill: Space::Inherited, stroke: Space::Inherited }];
        let mut edits = Vec::new();
        for op in parsed.ops {
            let state = stack.last().copied().unwrap_or(ColorState { fill: Space::Inherited, stroke: Space::Inherited });
            let replacement: Option<Op> = match op.op.as_slice() {
                b"q" => {
                    // Bounded by the stream's length: each `q` is at least a byte.
                    stack.push(state);
                    None
                }
                b"Q" => {
                    if stack.len() > 1 {
                        stack.pop();
                    }
                    None
                }
                b"g" | b"G" => {
                    set(&mut stack, op.op == b"G", Space::Gray);
                    None
                }
                b"rg" | b"RG" => {
                    let stroke = op.op == b"RG";
                    set(&mut stack, stroke, Space::Gray);
                    op.nums::<3>().filter(|_| op.operands.len() == 3).map(|[r, g, b]| gray_op(stroke, rgb_to_gray(r, g, b)))
                }
                b"k" | b"K" => {
                    let stroke = op.op == b"K";
                    set(&mut stack, stroke, Space::Gray);
                    op.nums::<4>().filter(|_| op.operands.len() == 4).map(|[c, m, y, k]| gray_op(stroke, cmyk_to_gray(c, m, y, k)))
                }
                b"cs" | b"CS" => {
                    let stroke = op.op == b"CS";
                    let space = op.name(0).map_or(Space::Other, |n| space_named(self.doc, resources, n));
                    set(&mut stack, stroke, space);
                    match space {
                        Space::Rgb | Space::Cmyk => Some(Op::new(if stroke { "CS" } else { "cs" }, vec![Object::name("DeviceGray")])),
                        Space::Spot => {
                            self.keep(KeptKind::SpotColor, page);
                            None
                        }
                        Space::Lab => {
                            self.keep(KeptKind::LabColor, page);
                            None
                        }
                        Space::Other => {
                            self.keep(KeptKind::OtherColor, page);
                            None
                        }
                        _ => None,
                    }
                }
                b"sc" | b"scn" | b"SC" | b"SCN" => {
                    let stroke = op.op.first() == Some(&b'S');
                    let space = if stroke { state.stroke } else { state.fill };
                    let name = op.operands.last().and_then(Object::as_name).is_some();
                    let nums: Option<Vec<f64>> = op.operands.iter().map(Object::as_f64).collect();
                    let keyword = String::from_utf8_lossy(&op.op).into_owned();
                    match (space, nums.as_deref()) {
                        (Space::Rgb | Space::Inherited, Some(&[r, g, b])) => Some(Op::new(&keyword, vec![num(round(rgb_to_gray(r, g, b)))])),
                        (Space::Cmyk | Space::Inherited, Some(&[c, m, y, k])) => Some(Op::new(&keyword, vec![num(round(cmyk_to_gray(c, m, y, k)))])),
                        _ => {
                            if name || space == Space::Pattern {
                                self.keep(KeptKind::Pattern, page);
                            }
                            None
                        }
                    }
                }
                b"sh" => {
                    self.keep(KeptKind::Shading, page);
                    None
                }
                b"Do" => {
                    if let Some(r) = op.name(0).and_then(|n| xobjects.get(n)).and_then(Object::as_ref) {
                        let subtype = match &*self.doc.get(r) {
                            Object::Stream(s) => s.dict.name(b"Subtype").map(<[u8]>::to_vec),
                            _ => None,
                        };
                        match subtype.as_deref() {
                            Some(b"Image") => {
                                self.image_page.entry(r).or_insert(page);
                            }
                            Some(b"Form") => self.form(r, resources, page, depth),
                            _ => {}
                        }
                    }
                    None
                }
                b"BI" => self.inline_image(&op, resources, page),
                _ => None,
            };
            if let Some(new) = replacement {
                let mut bytes = Vec::new();
                write_op(&new, &mut bytes);
                if !new.op.starts_with(b"BI") && bytes.last() == Some(&b'\n') {
                    bytes.pop();
                }
                if !matches!(new.op.as_slice(), b"cs" | b"CS" | b"BI") {
                    self.report.colors += 1;
                }
                edits.push((op.span.clone(), bytes));
            }
        }
        edits
    }

    /// An inline image as gray, or `None` (reported when it stays in colour).
    fn inline_image(&mut self, op: &Op, resources: &Dict, page: usize) -> Option<Op> {
        let (dict, data) = op.inline.as_ref()?;
        let get = |full: &[u8], short: &[u8]| dict.get(full).or_else(|| dict.get(short)).cloned();
        if matches!(get(b"ImageMask", b"IM"), Some(Object::Bool(true))) {
            return None;
        }
        let cs = get(b"ColorSpace", b"CS")?;
        // A name not among the abbreviations is a resource.
        let cs_obj = match &cs {
            Object::Name(n)
                if !matches!(n.as_slice(), b"G" | b"RGB" | b"CMYK" | b"I" | b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" | b"Indexed") =>
            {
                named_space(self.doc, resources, n)?
            }
            other => other.clone(),
        };
        let resolved = self.doc.resolve(&cs_obj);
        let indexed = matches!(resolved.as_array().and_then(|a| a.first()).and_then(Object::as_name), Some(b"Indexed" | b"I"));
        let space = if indexed { Space::Other } else { classify(self.doc, &cs_obj, 0) };
        let mut new = dict.clone();
        for k in [&b"ColorSpace"[..], b"CS"] {
            new.remove(k);
        }
        if indexed {
            let Some(palette) = gray_palette(self.doc, &resolved) else {
                if !matches!(palette_base(self.doc, &resolved), Some(Space::Gray)) {
                    self.keep(KeptKind::InlineImage, page);
                }
                return None;
            };
            new.set(b"CS".to_vec(), palette);
            self.report.palettes += 1;
            return Some(Op { op: b"BI".to_vec(), operands: Vec::new(), inline: Some((new, data.clone())), span: 0..0 });
        }
        let n = match space {
            Space::Rgb => 3,
            Space::Cmyk => 4,
            Space::Gray => return None,
            _ => {
                self.keep(KeptKind::InlineImage, page);
                return None;
            }
        };
        let filter = get(b"Filter", b"F");
        let flate = match filter.as_ref() {
            None | Some(Object::Null) => false,
            Some(Object::Name(f)) if matches!(f.as_slice(), b"FlateDecode" | b"Fl") => true,
            Some(Object::Array(a)) if a.is_empty() => false,
            _ => {
                self.keep(KeptKind::InlineImage, page);
                return None;
            }
        };
        let w = get(b"Width", b"W").and_then(|v| v.as_int()).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        let h = get(b"Height", b"H").and_then(|v| v.as_int()).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        let bpc = get(b"BitsPerComponent", b"BPC").and_then(|v| v.as_int());
        if bpc != Some(8) || get(b"Decode", b"D").is_some() || get(b"DecodeParms", b"DP").is_some() || w == 0 || h == 0 {
            self.keep(KeptKind::InlineImage, page);
            return None;
        }
        let samples = if flate {
            let mut d = Dict::new();
            d.set(b"Filter".to_vec(), Object::name("FlateDecode"));
            Stream::from_raw(d, data.clone()).decoded_within(slack(pixel_bytes(w, h, n).unwrap_or(0))).ok()
        } else {
            Some(data.clone())
        };
        let Some(gray) = samples.and_then(|s| to_gray_samples(&s, w, h, n)) else {
            self.keep(KeptKind::InlineImage, page);
            return None;
        };
        new.set(b"CS".to_vec(), Object::name("G"));
        let body = if flate { Stream::flate(Dict::new(), &gray).raw.to_vec() } else { gray };
        self.report.images += 1;
        Some(Op { op: b"BI".to_vec(), operands: Vec::new(), inline: Some((new, body)), span: 0..0 })
    }

    /// Every image XObject in the file, once.
    fn images(&mut self) {
        let mut smask_matte: Vec<ObjRef> = Vec::new();
        for n in self.doc.object_numbers() {
            let r = ObjRef { num: n, generation: self.doc.generation(n) };
            let obj = self.doc.get(r);
            let Object::Stream(s) = &*obj else { continue };
            if s.dict.name(b"Subtype") != Some(b"Image") {
                continue;
            }
            let page = self.image_page.get(&r).copied().unwrap_or(0);
            match self.image(s) {
                ImageOutcome::Unchanged => {}
                ImageOutcome::Kept => self.keep(KeptKind::Image, page),
                ImageOutcome::Palette(new) => {
                    self.doc.set(r, Object::Stream(new));
                    self.report.palettes += 1;
                }
                ImageOutcome::Converted(new, mask) => {
                    if let Some(m) = new.dict.reference(b"SMask") {
                        smask_matte.push(m);
                    }
                    let mut new = new;
                    if let Some(mask) = mask {
                        let mr = self.doc.add(Object::Stream(mask));
                        new.dict.set(b"SMask".to_vec(), Object::Ref(mr));
                    }
                    self.doc.set(r, Object::Stream(new));
                    self.report.images += 1;
                }
            }
        }
        // A soft mask's matte colour is in its image's colour space, now gray.
        for m in smask_matte {
            let obj = self.doc.get(m);
            let Object::Stream(s) = &*obj else { continue };
            let Some(Object::Array(matte)) = s.dict.get(b"Matte").map(|x| self.doc.resolve(x)).as_deref().cloned() else { continue };
            let Some(v) = gray_of_components(&matte) else { continue };
            let mut s = s.clone();
            s.dict.set(b"Matte".to_vec(), Object::Array(vec![num(v)]));
            self.doc.set(m, Object::Stream(s));
        }
    }

    fn image(&self, s: &Stream) -> ImageOutcome {
        let d = &s.dict;
        if matches!(d.get(b"ImageMask"), Some(Object::Bool(true))) {
            return ImageOutcome::Unchanged;
        }
        let Some(cs) = d.get(b"ColorSpace").cloned() else {
            // JPEG 2000 carries its own colour space.
            return if d.contains(b"Filter") && is_dct(s).is_none() { ImageOutcome::Kept } else { ImageOutcome::Unchanged };
        };
        let resolved = self.doc.resolve(&cs);
        if matches!(resolved.as_array().and_then(|a| a.first()).and_then(Object::as_name), Some(b"Indexed" | b"I")) {
            return match gray_palette(self.doc, &resolved) {
                Some(palette) => {
                    let mut s = s.clone();
                    s.dict.set(b"ColorSpace".to_vec(), palette);
                    ImageOutcome::Palette(s)
                }
                None if matches!(palette_base(self.doc, &resolved), Some(Space::Gray)) => ImageOutcome::Unchanged,
                None => ImageOutcome::Kept,
            };
        }
        let n = match classify(self.doc, &cs, 0) {
            Space::Gray => return ImageOutcome::Unchanged,
            Space::Rgb => 3,
            Space::Cmyk => 4,
            _ => return ImageOutcome::Kept,
        };
        let Some(dct) = is_dct(s) else { return ImageOutcome::Kept };
        if d.int(b"BitsPerComponent") != Some(8) || d.contains(b"Decode") || (dct && n == 4) {
            return ImageOutcome::Kept;
        }
        let (Some(w), Some(h)) = (d.int(b"Width").and_then(|v| u32::try_from(v).ok()), d.int(b"Height").and_then(|v| u32::try_from(v).ok())) else {
            return ImageOutcome::Kept;
        };
        if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
            return ImageOutcome::Kept;
        }
        let Some(need) = pixel_bytes(w, h, n) else { return ImageOutcome::Kept };
        let samples = if dct {
            image::load_from_memory_with_format(&s.raw, image::ImageFormat::Jpeg)
                .ok()
                .filter(|img| img.width() == w && img.height() == h)
                .map(|img| img.into_rgb8().into_raw())
        } else {
            s.decoded_within(slack(need)).ok()
        };
        let Some(samples) = samples.filter(|v| v.len() >= need) else { return ImageOutcome::Kept };
        let Some(gray) = to_gray_samples(&samples, w, h, n) else { return ImageOutcome::Kept };
        // A colour-key mask names colours that are gone: the same transparency as a soft mask.
        let mut mask = None;
        let key = d.get(b"Mask").map(|m| self.doc.resolve(m)).and_then(|m| m.as_array().cloned());
        if let Some(key) = &key
            && !d.contains(b"SMask")
        {
            let ranges: Option<Vec<i64>> = key.iter().map(|v| self.doc.resolve(v).as_int()).collect();
            let Some(ranges) = ranges.filter(|r| r.len() == 2 * n) else { return ImageOutcome::Kept };
            let alpha: Vec<u8> = samples
                .chunks_exact(n)
                .take(need / n)
                .map(|px| {
                    let masked = px.iter().zip(ranges.as_chunks::<2>().0).all(|(v, [lo, hi])| (*lo..=*hi).contains(&i64::from(*v)));
                    if masked { 0 } else { 255 }
                })
                .collect();
            let mut md = Dict::new();
            md.set(b"Type".to_vec(), Object::name("XObject"));
            md.set(b"Subtype".to_vec(), Object::name("Image"));
            md.set(b"Width".to_vec(), Object::Int(i64::from(w)));
            md.set(b"Height".to_vec(), Object::Int(i64::from(h)));
            md.set(b"ColorSpace".to_vec(), Object::name("DeviceGray"));
            md.set(b"BitsPerComponent".to_vec(), Object::Int(8));
            mask = Some(Stream::flate(md, &alpha));
        }
        let mut nd = d.clone();
        if key.is_some() {
            nd.remove(b"Mask");
        }
        let Some(mut new) = (if dct { jpeg_retain(&nd, &gray, w, h) } else { Some(gray_flate(&nd, &gray)) }) else { return ImageOutcome::Kept };
        new.dict.set(b"ColorSpace".to_vec(), Object::name("DeviceGray"));
        ImageOutcome::Converted(new, mask)
    }
}

enum ImageOutcome {
    Unchanged,
    Kept,
    Palette(Stream),
    /// The gray image, and a soft mask made from a colour-key mask.
    Converted(Stream, Option<Stream>),
}

fn gray_flate(old: &Dict, px: &[u8]) -> Stream {
    let mut d = old.clone();
    for k in [&b"Filter"[..], b"DecodeParms", b"Decode", b"Length", b"DL"] {
        d.remove(k);
    }
    Stream::flate(d, px)
}

/// A decoding limit for data that should be `need` bytes: writers sometimes pad samples, so
/// some more is accepted (and ignored) before decoding stops with an error.
fn slack(need: usize) -> usize {
    need.saturating_mul(2).saturating_add(65_536)
}

/// `w × h × n` bytes, if that fits.
fn pixel_bytes(w: u32, h: u32, n: usize) -> Option<usize> {
    usize::try_from(w).ok()?.checked_mul(usize::try_from(h).ok()?)?.checked_mul(n)
}

/// 8-bit RGB (`n` = 3) or CMYK (4) samples to 8-bit gray.
fn to_gray_samples(samples: &[u8], w: u32, h: u32, n: usize) -> Option<Vec<u8>> {
    let need = pixel_bytes(w, h, n)?;
    let samples = samples.get(..need)?;
    Some(match n {
        3 => samples.as_chunks::<3>().0.iter().map(|[r, g, b]| rgb8(*r, *g, *b)).collect(),
        4 => samples.as_chunks::<4>().0.iter().map(|[c, m, y, k]| cmyk8(*c, *m, *y, *k)).collect(),
        _ => return None,
    })
}

fn set(stack: &mut [ColorState], stroke: bool, space: Space) {
    if let Some(s) = stack.last_mut() {
        if stroke {
            s.stroke = space;
        } else {
            s.fill = space;
        }
    }
}

fn round(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn gray_op(stroke: bool, v: f64) -> Op {
    Op::new(if stroke { "G" } else { "g" }, vec![num(round(v))])
}

/// An annotation colour array of 3 (RGB) or 4 (CMYK) numbers as gray.
fn gray_of_components(c: &[Object]) -> Option<f64> {
    let v: Vec<f64> = c.iter().map(Object::as_f64).collect::<Option<_>>()?;
    let g = match v.as_slice() {
        [r, g, b] => rgb_to_gray(*r, *g, *b),
        [c, m, y, k] => cmyk_to_gray(*c, *m, *y, *k),
        _ => return None,
    };
    g.is_finite().then(|| round(g))
}

/// An Indexed space's base, classified.
fn palette_base(doc: &Document, indexed: &Object) -> Option<Space> {
    let a = indexed.as_array()?;
    Some(classify(doc, a.get(1)?, 0))
}

/// `[/Indexed base hival lookup]` with an RGB or CMYK base as `[/Indexed /DeviceGray hival
/// grays]`; `None` for other bases or an unreadable table.
fn gray_palette(doc: &Document, indexed: &Object) -> Option<Object> {
    let a = indexed.as_array()?;
    let n = match palette_base(doc, indexed)? {
        Space::Rgb => 3,
        Space::Cmyk => 4,
        _ => return None,
    };
    let hival = doc.resolve(a.get(2)?).as_int()?;
    let entries = usize::try_from(hival).ok().filter(|h| *h <= 255)?.checked_add(1)?;
    let lookup = match &*doc.resolve(a.get(3)?) {
        Object::String(s) => s.bytes.clone(),
        Object::Stream(s) => s.decoded_within(slack(entries * n)).ok()?,
        _ => return None,
    };
    // Missing entries read as 0, as renderers do with a short table.
    let table: Vec<u8> = (0..entries)
        .map(|i| {
            let c = |k: usize| lookup.get(i * n + k).copied().unwrap_or(0);
            if n == 3 { rgb8(c(0), c(1), c(2)) } else { cmyk8(c(0), c(1), c(2), c(3)) }
        })
        .collect();
    Some(Object::Array(vec![
        Object::name("Indexed"),
        Object::name("DeviceGray"),
        Object::Int(hival),
        Object::String(PdfString { bytes: table, hex: true }),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_match_iso_32000() {
        assert!((rgb_to_gray(1.0, 0.0, 0.0) - 0.299).abs() < 1e-9);
        assert!((rgb_to_gray(0.0, 1.0, 0.0) - 0.587).abs() < 1e-9);
        assert!((rgb_to_gray(0.0, 0.0, 1.0) - 0.114).abs() < 1e-9);
        assert!((rgb_to_gray(1.0, 1.0, 1.0) - 1.0).abs() < 1e-9);
        assert_eq!(cmyk_to_gray(0.0, 0.0, 0.0, 0.0), 1.0);
        assert_eq!(cmyk_to_gray(0.0, 0.0, 0.0, 1.0), 0.0);
        assert!((cmyk_to_gray(1.0, 0.0, 0.0, 0.0) - 0.7).abs() < 1e-9);
        assert_eq!(cmyk_to_gray(1.0, 1.0, 1.0, 1.0), 0.0, "clamped at black");
        assert_eq!(rgb8(255, 0, 0), 76);
        assert_eq!(rgb8(200, 200, 200), 200, "gray stays gray");
        assert_eq!(cmyk8(0, 0, 0, 0), 255);
        assert_eq!(cmyk8(255, 0, 0, 0), 179);
        assert_eq!(cmyk8(255, 255, 255, 255), 0);
        assert_eq!(rgb_to_gray(f64::NAN, f64::INFINITY, 0.0), 0.587, "always finite");
    }
}
