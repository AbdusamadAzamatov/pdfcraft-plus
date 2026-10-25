//! Linearized saving, "Fast Web View" (ISO 32000-2 Annex F).
//!
//! A linearized file shows its first page before the rest has arrived: the linearization
//! parameter dictionary is the first object, the first page's objects and a cross-reference
//! table for them come next, and a hint stream tells a viewer where every later page and every
//! shared object starts. The layout written here (Annex F.3):
//!
//! ```text
//! header
//! linearization dictionary                      part 2   (object N_main)
//! first-page xref + trailer (/Prev → main xref) part 3
//! catalog and document-level objects            part 4
//! primary hint stream                           part 5   (the last object number)
//! first page: page object (/O), contents, rest  part 6   (ends at /E)
//! pages 2..n: page object, contents, private    part 7   (page 2 is object 1)
//! objects shared by later pages                 part 8
//! outlines, then everything else (page tree, …) part 9
//! main xref (objects 0..N_main) + trailer       part 11
//! ```
//!
//! Document-level objects are the catalog, the encryption dictionary and what a viewer reads on
//! opening: `/ViewerPreferences`, `/OpenAction`, `/AcroForm` (with its widgets, even though pages
//! list them too) and `/Threads`. `/Info` is not among them. With `/PageMode /UseOutlines` the
//! outline tree follows the first page in part 6 instead of leading part 9. These assignments are
//! the ones qpdf's `--check-linearization`, the independent oracle, computes.
//!
//! Always a full rewrite with classic cross-reference tables and **no object streams**, so
//! nothing in the first-page section waits on a compressed container. Hint-table offsets are
//! computed as if the primary hint stream were absent (Annex F.4), which lets the file be laid
//! out in two passes without iteration: every object is serialized once, the hint tables are
//! built from those positions, the hint stream goes in front of part 6, and the fixed-width
//! header is rendered again with the real values.

use std::collections::{HashMap, HashSet};
use std::io::Write;

use super::{SaveOptions, collect_refs, ensure_id, prepared, renumber, reread, stamp_mod_date, trailer_key_rewritten, write_dict, write_indirect};
use crate::CosError;
use crate::document::{Document, ObjectReader};
use crate::object::{Dict, ObjRef, Object, Stream};

/// Page-tree nodes visited before giving up (the model's page walk uses the same cap).
const MAX_TREE_NODES: usize = 1_000_000;
/// References examined while working out which page uses which object.
const MAX_EDGE_VISITS: usize = 50_000_000;
/// Page memberships kept at once (the sum of every page's closure size).
const MAX_MEMBERSHIPS: usize = 20_000_000;
/// The attributes a page inherits from its ancestors (§7.7.3.4).
const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

fn fail(detail: &str) -> CosError {
    CosError::Linearize(detail.to_string())
}

fn too_large() -> CosError {
    fail("the file would be larger than 4 GiB")
}

fn too_many() -> CosError {
    fail("too many objects to save")
}

fn too_shared() -> CosError {
    fail("too many objects are shared between pages")
}

fn u32_of(v: u64) -> Result<u32, CosError> {
    u32::try_from(v).map_err(|_| too_large())
}

fn count(n: usize) -> Result<u32, CosError> {
    u32::try_from(n).map_err(|_| too_many())
}

/// One object of the graph reachable from the trailer: its kind and its references, with the
/// values of a top-level `/Parent` or `/Thumb` key kept apart: page membership never climbs to a
/// parent, and a page's thumbnail image is not needed to display it (it goes to part 9).
struct Node {
    is_stream: bool,
    refs: Vec<ObjRef>,
    aside: Vec<ObjRef>,
}

/// Where an object is written (Annex F.3 parts).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Part {
    /// Catalog and document-level objects (part 4).
    Document,
    /// Needed by the first page (part 6).
    FirstPage,
    /// Used by exactly one later page, with that page's index (part 7).
    Page(usize),
    /// Used by two or more later pages and not by the first (part 8).
    Shared,
    /// The outline hierarchy: after the first page when the document opens with outlines
    /// showing, otherwise first in part 9.
    Outline,
    /// Not associated with any page (part 9).
    Other,
}

/// Write `doc` as a linearized file (Fast Web View). A full rewrite: only objects reachable
/// from the trailer, renumbered, and encrypted with the document's output security exactly as
/// [`super::write_full`] would (the same keys when nothing changed). `opts.object_streams` is
/// ignored: linearized output never uses object streams. `mod_date` and `id_seed` apply as for
/// a full save. Attributes a page inherits from the page tree are copied onto the page, so the
/// first page needs nothing outside the first-page section.
///
/// Like any full rewrite this invalidates digital signatures; callers refuse signed documents.
pub fn write_linearized(doc: &Document, opts: &SaveOptions) -> Result<Vec<u8>, CosError> {
    if doc.stream_limited() {
        return Err(CosError::ReadOnlyLimit);
    }
    let mut reader = doc.object_reader();
    stamp_mod_date(&mut reader.document, opts);
    let trailer_in = reader.document.trailer().clone();
    let root = trailer_in.reference(b"Root").ok_or_else(|| fail("the document has no catalog"))?;
    let (pages, tree_nodes) = walk_pages(&mut reader, root)?;
    let (order, nodes) = build_graph(&mut reader, &trailer_in);
    if !nodes.contains_key(&root) {
        return Err(fail("the document has no catalog"));
    }
    let pages: Vec<ObjRef> = pages.into_iter().filter(|p| nodes.contains_key(p)).collect();
    if pages.is_empty() {
        return Err(fail("the document has no pages"));
    }
    let mut encryption: HashSet<ObjRef> = HashSet::new();
    let mut pending: Vec<ObjRef> = trailer_in.reference(b"Encrypt").into_iter().collect();
    while let Some(r) = pending.pop() {
        if let Some(node) = nodes.get(&r)
            && encryption.insert(r)
        {
            pending.extend(node.refs.iter().chain(&node.aside).copied());
        }
    }
    let page_set: HashSet<ObjRef> = pages.iter().copied().collect();
    if page_set.contains(&root) || pages.iter().any(|p| encryption.contains(p)) {
        return Err(fail("the page tree is malformed"));
    }

    // What each page needs: its object first, then its content streams, then the rest.
    let mut budget = MAX_EDGE_VISITS;
    let mut memberships = 0usize;
    let mut closures: Vec<Vec<ObjRef>> = Vec::with_capacity(pages.len());
    let mut contents: Vec<HashSet<ObjRef>> = Vec::with_capacity(pages.len());
    for &page in &pages {
        let (streams, array) = content_refs(&mut reader, page, &nodes);
        let blocked = |r: ObjRef| r == root || tree_nodes.contains(&r) || encryption.contains(&r) || (r != page && page_set.contains(&r));
        let closure = closure(&nodes, page, &streams, array, &blocked, &mut budget)?;
        memberships = memberships.saturating_add(closure.len());
        if memberships > MAX_MEMBERSHIPS {
            return Err(too_shared());
        }
        closures.push(closure);
        contents.push(streams.into_iter().collect());
    }

    // Classification in priority order; every object lands in exactly one part.
    let mut part: HashMap<ObjRef, Part> = HashMap::with_capacity(order.len());
    let structural = |r: &ObjRef| page_set.contains(r) || tree_nodes.contains(r);
    let catalog = reader.get(root);
    let catalog = catalog.as_dict();
    let catalog_refs = |keys: &[&[u8]]| {
        let mut refs = Vec::new();
        for key in keys {
            if let Some(v) = catalog.and_then(|c| c.get(key)) {
                collect_refs(v, &mut |r| refs.push(r));
            }
        }
        refs
    };
    // Part 4: the catalog, the encryption dictionary and what a viewer reads on opening the
    // document. They come before the pages, so a form's widgets are document-level objects
    // even though pages list them too.
    part.insert(root, Part::Document);
    let mut document = vec![root];
    let mut seeds: Vec<ObjRef> = trailer_in.reference(b"Encrypt").into_iter().collect();
    seeds.extend(catalog_refs(&[b"ViewerPreferences", b"OpenAction", b"AcroForm", b"Threads"]));
    document.extend(gather(&nodes, seeds, Part::Document, &mut part, &structural, &mut budget)?);
    // The outline hierarchy is one contiguous group with its own hint table: part of the
    // first-page section when the document opens with outlines showing, else first in part 9.
    let use_outlines = catalog.and_then(|c| c.name(b"PageMode")) == Some(&b"UseOutlines"[..]);
    let outlines = gather(&nodes, catalog_refs(&[b"Outlines"]), Part::Outline, &mut part, &structural, &mut budget)?;
    let first = closures.first().ok_or_else(|| fail("the document has no pages"))?;
    for r in first {
        part.entry(*r).or_insert(Part::FirstPage);
    }
    let mut users: HashMap<ObjRef, (u32, usize)> = HashMap::new();
    for (k, closure) in closures.iter().enumerate().skip(1) {
        for r in closure {
            if !part.contains_key(r) {
                let entry = users.entry(*r).or_insert((0, k));
                entry.0 = entry.0.saturating_add(1);
            }
        }
    }
    for (r, (users, k)) in users {
        part.insert(r, if users == 1 { Part::Page(k) } else { Part::Shared });
    }

    // File order. Main section: later pages, shared objects, the rest. First-page section:
    // linearization dictionary, document-level objects, first page; the hint stream last.
    let mut main: Vec<ObjRef> = Vec::new();
    let mut page_lists: Vec<Vec<ObjRef>> = Vec::with_capacity(pages.len());
    let mut first_page: Vec<ObjRef> = first.iter().copied().filter(|r| part.get(r) == Some(&Part::FirstPage)).collect();
    if use_outlines {
        first_page.extend(&outlines);
    }
    page_lists.push(first_page.clone());
    for (k, closure) in closures.iter().enumerate().skip(1) {
        let list: Vec<ObjRef> = closure.iter().copied().filter(|r| part.get(r) == Some(&Part::Page(k))).collect();
        main.extend(&list);
        page_lists.push(list);
    }
    let mut shared: Vec<ObjRef> = Vec::new();
    let mut placed: HashSet<ObjRef> = HashSet::new();
    for closure in closures.iter().skip(1) {
        for r in closure {
            if part.get(r) == Some(&Part::Shared) && placed.insert(*r) {
                shared.push(*r);
            }
        }
    }
    drop(placed);
    main.extend(&shared);
    if !use_outlines {
        main.extend(&outlines);
    }
    for r in &order {
        if !part.contains_key(r) {
            part.insert(*r, Part::Other);
            main.push(*r);
        }
    }
    if main.len() + document.len() + first_page.len() != order.len() {
        return Err(fail("objects could not be assigned to pages consistently"));
    }

    let n_main = count(main.len())?.checked_add(1).ok_or_else(too_many)?;
    let lin_num = n_main;
    let o_num = count(document.len())?.checked_add(lin_num).and_then(|n| n.checked_add(1)).ok_or_else(too_many)?;
    let hint_num = count(first_page.len())?.checked_add(o_num).ok_or_else(too_many)?;
    let n_total = hint_num.checked_add(1).ok_or_else(too_many)?;
    let mut map: HashMap<ObjRef, u32> = HashMap::with_capacity(order.len());
    for (i, r) in main.iter().enumerate() {
        map.insert(*r, count(i)? + 1);
    }
    for (i, r) in document.iter().enumerate() {
        map.insert(*r, lin_num + 1 + count(i)?);
    }
    for (i, r) in first_page.iter().enumerate() {
        map.insert(*r, o_num + count(i)?);
    }

    // Shared object identifiers (Table F.4 item 4): first-page objects by number from /O, then
    // the shared-objects section by number. Each later page lists those it uses.
    let n_first = first_page.len();
    let shared_index: HashMap<ObjRef, u64> = first_page.iter().chain(&shared).enumerate().map(|(i, r)| (*r, i as u64)).collect();
    let mut page_shared: Vec<Vec<u64>> = Vec::with_capacity(pages.len());
    for (k, closure) in closures.iter().enumerate() {
        let mut ids: Vec<u64> = if k == 0 { Vec::new() } else { closure.iter().filter_map(|r| shared_index.get(r).copied()).collect() };
        ids.sort_unstable();
        page_shared.push(ids);
    }
    drop(shared_index);
    drop(closures);
    drop(part);

    // Trailer of the first-page section (the one readers use): everything a full save keeps.
    let mut trailer = Dict::new();
    trailer.set(b"Size".to_vec(), Object::Int(i64::from(n_total)));
    for key in [&b"Root"[..], b"Info", b"Encrypt"] {
        if let Some(r) = trailer_in.reference(key).and_then(|r| map.get(&r)) {
            trailer.set(key.to_vec(), Object::Ref(ObjRef::new(*r, 0)));
        }
    }
    if let Some(Object::Dict(d)) = trailer_in.get(b"Encrypt") {
        trailer.set(b"Encrypt".to_vec(), Object::Dict(d.clone()));
    }
    if let Some(id) = trailer_in.get(b"ID") {
        trailer.set(b"ID".to_vec(), id.clone());
    }
    if reader.document.output_security().0.is_none() {
        ensure_id(&mut trailer, opts, reader.document.bytes());
    }
    let mut main_trailer = Dict::new();
    main_trailer.set(b"Size".to_vec(), Object::Int(i64::from(n_main)));
    for key in [&b"Root"[..], b"Info", b"Encrypt", b"ID"] {
        if let Some(v) = trailer.get(key) {
            main_trailer.set(key.to_vec(), v.clone());
        }
    }
    for (key, value) in trailer_in.iter() {
        if !trailer_key_rewritten(key) {
            trailer.set(key.clone(), renumber(value, &map));
        }
    }

    let version = reader.document.version().chars().take(3).collect::<String>();
    let version = if version.as_str() < "1.2" { "1.2".to_string() } else { version };
    let mut trailer_bytes = Vec::new();
    write_dict(&trailer, &mut trailer_bytes);
    trailer_bytes.truncate(trailer_bytes.len().saturating_sub(2)); // the closing `>>`: /Prev follows
    let first_rows = usize::try_from(n_total - n_main).map_err(|_| too_many())?;
    let mut layout = Prefix { version, n_main, o_num, pages: pages.len(), trailer: trailer_bytes, rows: vec![0; first_rows], ..Prefix::default() };
    let (prefix, first_xref_at) = layout.render();
    let p = prefix.len() as u64;

    // Pass 1: every object once. `pos[num]` = (offset as if the hint stream were absent, length).
    let mut pos: Vec<(u64, u64)> = Vec::new();
    pos.try_reserve_exact(usize::try_from(n_total).map_err(|_| too_many())?).map_err(|_| fail("not enough memory to write the file"))?;
    pos.resize(n_total as usize, (0, 0));
    let stream_of = |r: &ObjRef| nodes.get(r).is_some_and(|n| n.is_stream);
    let mut part4 = Vec::new();
    for r in &document {
        emit(&mut reader, &map, *r, stream_of(r), p, &mut part4, &mut pos)?;
    }
    let hint_at = p + part4.len() as u64;
    let mut rest = Vec::new();
    for r in &first_page {
        emit(&mut reader, &map, *r, stream_of(r), hint_at, &mut rest, &mut pos)?;
    }
    let end_first = hint_at + rest.len() as u64;
    for r in &main {
        emit(&mut reader, &map, *r, stream_of(r), hint_at, &mut rest, &mut pos)?;
    }
    u32_of(hint_at + rest.len() as u64)?;

    // The hint stream, from those positions.
    let at = |r: &ObjRef| map.get(r).and_then(|n| pos.get(*n as usize)).copied().unwrap_or((0, 0));
    let mut entries = Vec::with_capacity(page_lists.len());
    for (k, list) in page_lists.iter().enumerate() {
        let streams = contents.get(k);
        let is_content = |r: &ObjRef| streams.is_some_and(|s| s.contains(r));
        let (mut content_offset, mut content_length) = (0, 0);
        if let (Some(page), Some(first_content)) = (list.first(), list.get(1))
            && is_content(first_content)
        {
            content_offset = at(page).1;
            content_length = list.iter().skip(1).take_while(|r| is_content(r)).map(|r| at(r).1).sum();
        }
        entries.push(PageEntry {
            objects: list.len() as u64,
            length: list.iter().map(|r| at(r).1).sum(),
            shared: page_shared.get(k).cloned().unwrap_or_default(),
            content_offset,
            content_length,
        });
    }
    let hints = Hints {
        pages: entries,
        first_page_offset: first_page.first().map(|r| at(r).0).unwrap_or(hint_at),
        first_page_objects: n_first as u64,
        shared_start: shared.first().map(|r| (u64::from(map.get(r).copied().unwrap_or(0)), at(r).0)).unwrap_or((0, 0)),
        group_lengths: first_page.iter().chain(&shared).map(|r| at(r).1).collect(),
        outlines: outlines.first().map(|r| OutlineGroup {
            first: u64::from(map.get(r).copied().unwrap_or(0)),
            offset: at(r).0,
            objects: outlines.len() as u64,
            length: outlines.iter().map(|r| at(r).1).sum(),
        }),
    };
    let (data, s_offset, o_offset) = hints.encode()?;
    let mut hint_dict = Dict::new();
    hint_dict.set(b"S".to_vec(), Object::Int(i64::try_from(s_offset).map_err(|_| too_large())?));
    if let Some(o) = o_offset {
        hint_dict.set(b"O".to_vec(), Object::Int(i64::try_from(o).map_err(|_| too_large())?));
    }
    let hint_obj = Object::Stream(Stream::flate(hint_dict, &data));
    let mut hint = Vec::new();
    write_indirect(hint_num, 0, &prepared(&reader.document, u32::MAX, hint_num, 0, &hint_obj), &mut hint);
    let h = hint.len() as u64;
    let actual = |adjusted: u64| if adjusted >= hint_at { adjusted + h } else { adjusted };

    // Main cross-reference table and trailer, at their final offsets.
    let main_xref_at = hint_at + h + rest.len() as u64;
    let subsection = format!("0 {n_main}");
    // /T: the end-of-line before the main table's first entry (Annex F, Table F.1).
    let t = main_xref_at + b"xref\n".len() as u64 + subsection.len() as u64;
    let _ = write!(rest, "xref\n{subsection}\n0000000000 65535 f \n");
    for num in 1..n_main {
        let (off, _) = pos.get(num as usize).copied().unwrap_or((0, 0));
        let _ = writeln!(rest, "{:010} 00000 n ", actual(off));
    }
    rest.extend_from_slice(b"trailer\n");
    write_dict(&main_trailer, &mut rest);
    let _ = write!(rest, "\nstartxref\n{first_xref_at}\n%%EOF\n");

    // Pass 2: the header again with real values; fixed-width fields keep its length.
    let total = p + part4.len() as u64 + h + rest.len() as u64;
    layout.length = u32_of(total)?;
    layout.hint = (u32_of(hint_at)?, u32_of(h)?);
    layout.end_first = u32_of(actual(end_first))?;
    layout.main_xref = u32_of(main_xref_at)?;
    layout.t = u32_of(t)?;
    for (i, row) in layout.rows.iter_mut().enumerate() {
        let num = (n_main as usize).saturating_add(i);
        *row = if num == hint_num as usize { u32_of(hint_at)? } else { u32_of(actual(pos.get(num).map(|p| p.0).unwrap_or(0)))? };
    }
    let (prefix, _) = layout.render();
    if prefix.len() as u64 != p {
        return Err(fail("the first-page header changed size while it was written"));
    }
    let mut out = Vec::new();
    out.try_reserve_exact(usize::try_from(total).map_err(|_| too_large())?).map_err(|_| fail("not enough memory to write the file"))?;
    out.extend_from_slice(&prefix);
    out.extend_from_slice(&part4);
    out.extend_from_slice(&hint);
    out.extend_from_slice(&rest);
    Ok(out)
}

/// Serialize object `r` as its new number at file offset `base + out.len()`, recording where.
fn emit(
    reader: &mut ObjectReader,
    map: &HashMap<ObjRef, u32>,
    r: ObjRef,
    is_stream: bool,
    base: u64,
    out: &mut Vec<u8>,
    pos: &mut [(u64, u64)],
) -> Result<(), CosError> {
    let num = map.get(&r).copied().ok_or_else(|| fail("an object has no number"))?;
    let o = renumber(&*reread(reader, r, is_stream)?, map);
    let start = out.len();
    write_indirect(num, 0, &prepared(&reader.document, r.num, num, 0, &o), out);
    if let Some(slot) = pos.get_mut(num as usize) {
        *slot = (base + start as u64, (out.len() - start) as u64);
    }
    Ok(())
}

/// Breadth first from `seeds`, every object not yet in a part and not a page or page-tree node
/// goes into `target`, in visit order.
fn gather(
    nodes: &HashMap<ObjRef, Node>,
    mut queue: Vec<ObjRef>,
    target: Part,
    part: &mut HashMap<ObjRef, Part>,
    structural: &dyn Fn(&ObjRef) -> bool,
    budget: &mut usize,
) -> Result<Vec<ObjRef>, CosError> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(r) = queue.get(cursor).copied() {
        cursor += 1;
        if part.contains_key(&r) || structural(&r) {
            continue;
        }
        let Some(node) = nodes.get(&r) else { continue };
        part.insert(r, target);
        out.push(r);
        *budget = budget.checked_sub(node.refs.len()).ok_or_else(too_shared)?;
        queue.extend(node.refs.iter().copied());
    }
    Ok(out)
}

/// The fixed-width start of the file: header, linearization dictionary, first-page
/// cross-reference table and trailer. Unknown numbers are zero on the first render; every
/// one is padded to ten characters so the second render has the same length.
#[derive(Default)]
struct Prefix {
    version: String,
    n_main: u32,
    o_num: u32,
    pages: usize,
    /// The first-page trailer without its closing `>>`.
    trailer: Vec<u8>,
    /// Offsets of objects `n_main + 1 ..` (the first row, the dictionary itself, is filled in).
    rows: Vec<u32>,
    length: u32,
    hint: (u32, u32),
    end_first: u32,
    main_xref: u32,
    t: u32,
}

impl Prefix {
    /// The bytes, and the offset of the first-page `xref` keyword.
    fn render(&self) -> (Vec<u8>, u64) {
        let mut out = Vec::new();
        let _ = writeln!(out, "%PDF-{}", self.version);
        out.extend_from_slice(&[b'%', 0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
        let lin_at = out.len();
        let _ = write!(
            out,
            "{} 0 obj\n<< /Linearized 1 /L {:<10} /H [ {:<10} {:<10} ] /O {} /E {:<10} /N {} /T {:<10} >>\nendobj\n",
            self.n_main, self.length, self.hint.0, self.hint.1, self.o_num, self.end_first, self.pages, self.t
        );
        let xref_at = out.len();
        let _ = write!(out, "xref\n{} {}\n", self.n_main, self.rows.len());
        for (i, row) in self.rows.iter().enumerate() {
            let offset = if i == 0 { lin_at as u64 } else { u64::from(*row) };
            let _ = writeln!(out, "{offset:010} 00000 n ");
        }
        out.extend_from_slice(b"trailer\n");
        out.extend_from_slice(&self.trailer);
        let _ = write!(out, "/Prev {:<10}>>\nstartxref\n0\n%%EOF\n", self.main_xref);
        (out, xref_at as u64)
    }
}

/// Leaf pages in order, and the intermediate tree nodes. Attributes a page inherits are copied
/// onto it, in the reader's private copy of the document only.
fn walk_pages(reader: &mut ObjectReader, root: ObjRef) -> Result<(Vec<ObjRef>, HashSet<ObjRef>), CosError> {
    struct Frame {
        kids: Vec<Object>,
        next: usize,
        inherited: [Option<Object>; 4],
    }
    let top = reader.get(root).as_dict().and_then(|d| d.reference(b"Pages"));
    let mut stack = vec![Frame { kids: top.map(Object::Ref).into_iter().collect(), next: 0, inherited: Default::default() }];
    let mut seen: HashSet<ObjRef> = HashSet::new();
    let mut pages = Vec::new();
    let mut tree = HashSet::new();
    // A "page" that is not a dictionary (a stream, say) can't take the attributes it inherits.
    let mut all_pushed = true;
    while let Some(frame) = stack.last_mut() {
        let Some(kid) = frame.kids.get(frame.next) else {
            stack.pop();
            continue;
        };
        // A successful get means `next` is below the array length.
        frame.next += 1;
        let Some(child) = kid.as_ref() else {
            // A direct (invalid) kid still inherits from the tree: keep the tree's attributes.
            all_pushed &= !matches!(kid, Object::Dict(_));
            continue;
        };
        let mut inherited = frame.inherited.clone();
        if seen.len() >= MAX_TREE_NODES {
            return Err(fail("the page tree is too large"));
        }
        if !seen.insert(child) {
            continue;
        }
        let obj = reader.get(child);
        let Some(d) = obj.as_dict() else { continue };
        for (key, slot) in INHERITABLE.iter().zip(&mut inherited) {
            if let Some(v) = d.get(key) {
                *slot = Some(v.clone());
            }
        }
        let is_pages = d.name(b"Type") == Some(b"Pages") || (d.contains(b"Kids") && d.name(b"Type") != Some(b"Page"));
        if is_pages {
            tree.insert(child);
            let kids = match d.get(b"Kids") {
                Some(Object::Ref(r)) => reader.get(*r).as_array().cloned(),
                Some(Object::Array(a)) => Some(a.clone()),
                _ => None,
            };
            if let Some(kids) = kids {
                stack.push(Frame { kids, next: 0, inherited });
            }
            continue;
        }
        if let Object::Dict(page) = &*obj {
            let missing: Vec<(&[u8], Object)> =
                INHERITABLE.iter().zip(inherited).filter(|(key, _)| !page.contains(key)).filter_map(|(key, v)| Some((*key, v?))).collect();
            if !missing.is_empty() {
                let mut page = page.clone();
                for (key, v) in missing {
                    page.set(key.to_vec(), v);
                }
                reader.document.set(child, Object::Dict(page));
            }
        } else {
            all_pushed = false;
        }
        pages.push(child);
    }
    // Every page now carries what it inherited, so the tree nodes no longer need to: Annex F
    // readers (qpdf among them) expect no inheritable attributes left in the tree.
    for node in tree.iter().filter(|_| all_pushed) {
        if let Object::Dict(d) = &*reader.get(*node)
            && INHERITABLE.iter().any(|key| d.contains(key))
        {
            let mut d = d.clone();
            for key in INHERITABLE {
                d.remove(key);
            }
            reader.document.set(*node, Object::Dict(d));
        }
    }
    Ok((pages, tree))
}

/// Everything reachable from the trailer in breadth-first order, with each object's references.
fn build_graph(reader: &mut ObjectReader, trailer: &Dict) -> (Vec<ObjRef>, HashMap<ObjRef, Node>) {
    let mut queue: Vec<ObjRef> = Vec::new();
    for key in [&b"Root"[..], b"Info", b"Encrypt"] {
        queue.extend(trailer.reference(key));
    }
    // Unknown trailer entries are document data (as in `write_full`).
    for (key, value) in trailer.iter() {
        if !trailer_key_rewritten(key) {
            collect_refs(value, &mut |r| queue.push(r));
        }
    }
    let mut order = Vec::new();
    let mut nodes: HashMap<ObjRef, Node> = HashMap::new();
    let mut cursor = 0;
    while let Some(r) = queue.get(cursor).copied() {
        cursor += 1;
        // Drop what has been processed once it is most of the queue.
        if cursor > 1 << 16 && cursor * 2 > queue.len() {
            queue.drain(..cursor);
            cursor = 0;
        }
        if nodes.contains_key(&r) {
            continue;
        }
        let o = reader.get(r);
        let (refs, aside) = match &*o {
            Object::Null => continue, // dangling: renumbering writes null in its place
            Object::Dict(d) => split_refs(d),
            Object::Stream(s) => split_refs(&s.dict),
            other => {
                let mut refs = Vec::new();
                collect_refs(other, &mut |x| refs.push(x));
                (refs, Vec::new())
            }
        };
        queue.extend(refs.iter().chain(&aside).copied());
        order.push(r);
        nodes.insert(r, Node { is_stream: matches!(*o, Object::Stream(_)), refs, aside });
    }
    (order, nodes)
}

fn split_refs(d: &Dict) -> (Vec<ObjRef>, Vec<ObjRef>) {
    let (mut refs, mut aside) = (Vec::new(), Vec::new());
    for (k, v) in d.iter() {
        if matches!(k.as_slice(), b"Parent" | b"Thumb") {
            collect_refs(v, &mut |x| aside.push(x));
        } else {
            collect_refs(v, &mut |x| refs.push(x));
        }
    }
    (refs, aside)
}

/// A page's content streams in `/Contents` order, and the indirect array holding them, if any.
fn content_refs(reader: &mut ObjectReader, page: ObjRef, nodes: &HashMap<ObjRef, Node>) -> (Vec<ObjRef>, Option<ObjRef>) {
    let page_obj = reader.get(page);
    let Some(contents) = page_obj.as_dict().and_then(|d| d.get(b"Contents")).cloned() else { return (Vec::new(), None) };
    let refs = |a: &[Object]| a.iter().filter_map(Object::as_ref).collect::<Vec<_>>();
    let (list, array) = match contents {
        Object::Ref(r) => match nodes.get(&r) {
            Some(n) if n.is_stream => (vec![r], None),
            Some(_) => (reader.get(r).as_array().map(|a| refs(a)).unwrap_or_default(), Some(r)),
            None => (Vec::new(), None),
        },
        Object::Array(a) => (refs(&a), None),
        _ => (Vec::new(), None),
    };
    let mut seen = HashSet::new();
    let streams = list.into_iter().filter(|r| nodes.get(r).is_some_and(|n| n.is_stream) && seen.insert(*r)).collect();
    (streams, array)
}

/// What `page` needs, breadth first: the page object, its content streams, the indirect
/// `/Contents` array, then everything else it reaches without climbing to a `/Parent`, taking
/// its `/Thumb`nail, or entering anything `blocked` (the catalog, the page tree, other pages,
/// encryption).
fn closure(
    nodes: &HashMap<ObjRef, Node>,
    page: ObjRef,
    streams: &[ObjRef],
    array: Option<ObjRef>,
    blocked: &dyn Fn(ObjRef) -> bool,
    budget: &mut usize,
) -> Result<Vec<ObjRef>, CosError> {
    let mut out = vec![page];
    let mut seen: HashSet<ObjRef> = HashSet::from([page]);
    for r in streams.iter().copied().chain(array) {
        if nodes.contains_key(&r) && !blocked(r) && seen.insert(r) {
            out.push(r);
        }
    }
    let mut cursor = 0;
    while let Some(r) = out.get(cursor).copied() {
        cursor += 1;
        let Some(node) = nodes.get(&r) else { continue };
        for child in &node.refs {
            *budget = budget.checked_sub(1).ok_or_else(too_shared)?;
            if nodes.contains_key(child) && !blocked(*child) && seen.insert(*child) {
                out.push(*child);
            }
        }
    }
    Ok(out)
}

/// What the page offset hint table says about one page (Annex F, Table F.4). Lengths and
/// offsets are in bytes; a page's objects are its page object and the objects only it uses
/// (for the first page: the whole first-page section).
#[derive(Clone, Debug, PartialEq)]
struct PageEntry {
    objects: u64,
    length: u64,
    /// Shared object identifiers, ascending.
    shared: Vec<u64>,
    content_offset: u64,
    content_length: u64,
}

/// Everything the primary hint stream encodes. Offsets are as if the hint stream were absent.
#[derive(Clone, Debug)]
struct Hints {
    pages: Vec<PageEntry>,
    /// Offset of the first page's page object (Table F.3 item 2).
    first_page_offset: u64,
    /// Objects in the first-page section; they are the first shared-object entries.
    first_page_objects: u64,
    /// Number and offset of the first object in the shared-objects section, or zeros.
    shared_start: (u64, u64),
    /// Length of every shared-object group (one object each): first-page objects, then part 8.
    group_lengths: Vec<u64>,
    /// The outline hierarchy, when the document has one.
    outlines: Option<OutlineGroup>,
}

/// A generic hint table (Annex F.4.6): one contiguous group of objects.
#[derive(Clone, Debug)]
struct OutlineGroup {
    first: u64,
    offset: u64,
    objects: u64,
    length: u64,
}

/// Bits per field for values `0..=range`. Rounded up to whole bytes (8, 16 or 32 bits): any
/// width that holds the values is valid, and byte-sized fields leave no doubt about padding
/// between the per-item arrays.
fn bits(range: u64) -> Result<u32, CosError> {
    match 64 - range.leading_zeros() {
        0 => Ok(0),
        1..=8 => Ok(8),
        9..=16 => Ok(16),
        17..=32 => Ok(32),
        _ => Err(too_large()),
    }
}

/// Most-significant-bit-first bit packing (Annex F.4).
#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    current: u8,
    used: u32,
}

impl BitWriter {
    fn put(&mut self, value: u64, width: u32) -> Result<(), CosError> {
        if width > 32 || (value >> width) != 0 {
            return Err(fail("a hint table value does not fit its field"));
        }
        for i in (0..width).rev() {
            self.current = (self.current << 1) | ((value >> i) & 1) as u8;
            self.used += 1;
            if self.used == 8 {
                self.out.push(self.current);
                self.current = 0;
                self.used = 0;
            }
        }
        Ok(())
    }

    fn align(&mut self) {
        if self.used > 0 {
            self.out.push(self.current << (8 - self.used));
            self.current = 0;
            self.used = 0;
        }
    }
}

/// Least value and the field width for the differences from it.
fn spread(values: impl Iterator<Item = u64> + Clone) -> Result<(u64, u32), CosError> {
    let least = values.clone().min().unwrap_or(0);
    let most = values.max().unwrap_or(0);
    Ok((least, bits(most - least)?))
}

impl Hints {
    /// The hint stream data and the offsets in it of the shared object hint table (`/S`) and
    /// of the outline hint table (`/O`), if there is one.
    fn encode(&self) -> Result<(Vec<u8>, u64, Option<u64>), CosError> {
        let mut w = BitWriter::default();
        let pages = &self.pages;
        let (least_objects, objects_bits) = spread(pages.iter().map(|p| p.objects))?;
        let (least_length, length_bits) = spread(pages.iter().map(|p| p.length))?;
        let (least_offset, offset_bits) = spread(pages.iter().map(|p| p.content_offset))?;
        let (least_content, content_bits) = spread(pages.iter().map(|p| p.content_length))?;
        let shared_count_bits = bits(pages.iter().map(|p| p.shared.len() as u64).max().unwrap_or(0))?;
        let identifier_bits = bits(pages.iter().flat_map(|p| p.shared.iter().copied()).max().unwrap_or(0))?;
        // Page offset hint table header (Table F.3).
        w.put(least_objects, 32)?;
        w.put(self.first_page_offset, 32)?;
        w.put(u64::from(objects_bits), 16)?;
        w.put(least_length, 32)?;
        w.put(u64::from(length_bits), 16)?;
        w.put(least_offset, 32)?;
        w.put(u64::from(offset_bits), 16)?;
        w.put(least_content, 32)?;
        w.put(u64::from(content_bits), 16)?;
        w.put(u64::from(shared_count_bits), 16)?;
        w.put(u64::from(identifier_bits), 16)?;
        w.put(0, 16)?; // numerators of fractional positions: not given
        w.put(1, 16)?; // their denominator
        // Per-page entries (Table F.4), item by item: each item for every page in turn.
        for p in pages {
            w.put(p.objects - least_objects, objects_bits)?;
        }
        for p in pages {
            w.put(p.length - least_length, length_bits)?;
        }
        for p in pages {
            w.put(p.shared.len() as u64, shared_count_bits)?;
        }
        for p in pages {
            for id in &p.shared {
                w.put(*id, identifier_bits)?;
            }
        }
        // Item 5, the numerators, takes no bits.
        for p in pages {
            w.put(p.content_offset - least_offset, offset_bits)?;
        }
        for p in pages {
            w.put(p.content_length - least_content, content_bits)?;
        }
        w.align();
        let s = w.out.len() as u64;
        // Shared object hint table header (Table F.5).
        let groups = &self.group_lengths;
        let (least_group, group_bits) = spread(groups.iter().copied())?;
        w.put(self.shared_start.0, 32)?;
        w.put(self.shared_start.1, 32)?;
        w.put(self.first_page_objects, 32)?;
        w.put(groups.len() as u64, 32)?;
        w.put(0, 16)?; // every group is a single object
        w.put(least_group, 32)?;
        w.put(u64::from(group_bits), 16)?;
        // Shared object groups (Table F.6), item by item.
        for g in groups {
            w.put(g - least_group, group_bits)?;
        }
        for _ in groups {
            w.put(0, 1)?; // no MD5 signature
        }
        // Item 4, objects per group minus one, takes no bits.
        w.align();
        let o = match &self.outlines {
            Some(group) => {
                let at = w.out.len() as u64;
                for value in [group.first, group.offset, group.objects, group.length] {
                    w.put(value, 32)?;
                }
                Some(at)
            }
            None => None,
        };
        Ok((w.out, s, o))
    }
}

/// Whether `bytes` is a linearized file as saved: its first object, within the first 1024
/// bytes, is a linearization parameter dictionary whose `/L` is the file's length. Appending
/// an incremental update changes the length, so an updated file is no longer linearized.
pub fn is_linearized(bytes: &[u8]) -> bool {
    let window = bytes.get(..bytes.len().min(1024)).unwrap_or_default();
    let Some(h) = window.windows(5).position(|w| w == b"%PDF-") else { return false };
    let mut at = h;
    // Skip the header line, then comments and white space.
    loop {
        while bytes.get(at).is_some_and(|b| !matches!(b, b'\r' | b'\n')) {
            at += 1;
        }
        while bytes.get(at).is_some_and(|b| crate::is_whitespace(*b)) {
            at += 1;
        }
        if bytes.get(at) != Some(&b'%') {
            break;
        }
    }
    let Some(limited) = bytes.get(..bytes.len().min(h.saturating_add(1024))) else { return false };
    let Ok((_, Object::Dict(d))) = crate::parser::parse_indirect(limited, at, &|_| None) else { return false };
    if d.get(b"Linearized").and_then(Object::as_f64).is_none() {
        return false;
    }
    match d.get(b"L") {
        Some(Object::Int(l)) => u64::try_from(*l).is_ok_and(|l| l == bytes.len() as u64 || l == (bytes.len() - h) as u64),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_are_packed_most_significant_first_and_aligned() {
        let mut w = BitWriter::default();
        w.put(0b101, 3).unwrap();
        w.put(1, 1).unwrap();
        w.align();
        w.put(0xABCD, 16).unwrap();
        w.put(0, 0).unwrap();
        w.put(1, 1).unwrap();
        w.align();
        assert_eq!(w.out, vec![0b1011_0000, 0xAB, 0xCD, 0b1000_0000]);
        assert!(w.put(2, 1).is_err(), "a value wider than its field");
        assert!(w.put(1, 0).is_err(), "a value in a zero-width field");
        assert!(w.put(0, 33).is_err(), "fields are at most 32 bits");
    }

    #[test]
    fn field_widths_round_up_to_whole_bytes() {
        assert_eq!(bits(0).unwrap(), 0);
        assert_eq!(bits(1).unwrap(), 8);
        assert_eq!(bits(255).unwrap(), 8);
        assert_eq!(bits(256).unwrap(), 16);
        assert_eq!(bits(65_536).unwrap(), 32);
        assert_eq!(bits(u64::from(u32::MAX)).unwrap(), 32);
        assert!(bits(1 << 32).is_err());
    }

    #[test]
    fn hint_tables_have_the_annex_f_layout() {
        let page = |objects, length, shared: Vec<u64>| PageEntry { objects, length, shared, content_offset: 0, content_length: 0 };
        let hints = Hints {
            pages: vec![page(3, 300, vec![]), page(2, 200, vec![0, 3])],
            first_page_offset: 1000,
            first_page_objects: 3,
            shared_start: (7, 2000),
            group_lengths: vec![100, 100, 100, 50],
            outlines: Some(OutlineGroup { first: 9, offset: 3000, objects: 2, length: 120 }),
        };
        let (data, s, o) = hints.encode().unwrap();
        // Header 36 bytes; objects 1 byte each; lengths 1 byte each; counts 1 byte each; 2 ids.
        assert_eq!(s, 36 + 2 + 2 + 2 + 2);
        let be32 = |at: usize| u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
        let be16 = |at: usize| u16::from_be_bytes(data[at..at + 2].try_into().unwrap());
        assert_eq!((be32(0), be32(4), be16(8)), (2, 1000, 8));
        assert_eq!((be32(10), be16(14)), (200, 8));
        assert_eq!((be16(28), be16(30), be16(32), be16(34)), (8, 8, 0, 1));
        assert_eq!(&data[36..44], &[1, 0, 100, 0, 0, 2, 0, 3]);
        let s = s as usize;
        assert_eq!((be32(s), be32(s + 4), be32(s + 8), be32(s + 12)), (7, 2000, 3, 4));
        assert_eq!((be16(s + 16), be32(s + 18), be16(s + 22)), (0, 50, 8));
        assert_eq!(&data[s + 24..s + 29], &[50, 50, 50, 0, 0]);
        assert_eq!(o, Some(s as u64 + 29));
        assert_eq!((be32(s + 29), be32(s + 33), be32(s + 37), be32(s + 41)), (9, 3000, 2, 120));
        assert_eq!(data.len(), s + 45);
    }

    #[test]
    fn detection_rejects_everything_but_a_matching_dictionary() {
        let lin = |l: &str| format!("%PDF-1.7\n%\u{e2}\u{e3}\n1 0 obj\n<< /Linearized 1 /L {l} >>\nendobj\n");
        let exact = |pre: &str| {
            let mut s = format!("{pre}{}", lin("0000"));
            let n = s.len();
            s = s.replace("0000", &format!("{n:04}"));
            s.into_bytes()
        };
        assert!(is_linearized(&exact("")));
        assert!(is_linearized(&exact("junk\n")), "bytes before the header");
        assert!(!is_linearized(b""));
        assert!(!is_linearized(b"not a pdf at all"));
        let mut appended = exact("");
        appended.extend_from_slice(b"1 0 obj\nnull\nendobj\n");
        assert!(!is_linearized(&appended), "an incremental update makes /L stale");
        assert!(!is_linearized(lin("9").as_bytes()), "wrong length");
        assert!(!is_linearized(lin("(12)").as_bytes()), "a string length");
        assert!(!is_linearized(lin("-1").as_bytes()), "a negative length");
        let truncated = exact("");
        assert!(!is_linearized(&truncated[..truncated.len() - 12]), "truncated dictionary");
        let late = format!("%PDF-1.7\n%{}\n1 0 obj\n<< /Linearized 1 /L 2000 >>\nendobj\n", " ".repeat(1100));
        let late = format!("{late:<2000}");
        assert!(!is_linearized(late.as_bytes()), "the dictionary must start within 1024 bytes");
        let plain = format!("%PDF-1.7\n1 0 obj\n<< /Type /Catalog /L {} >>\nendobj\n", 46);
        assert!(!is_linearized(plain.as_bytes()), "not a linearization dictionary");
    }
}
