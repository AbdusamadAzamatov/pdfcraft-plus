use super::{
    Arc, CombineSource, Combined, DataFormat, DocId, EditError, Hidden, ImageResolution, LOW_RESOLUTION_PRINT_DPI, OptimizeReport, SaveOptions,
    Session, SourceKind, SplitPart, decode_text, guard, impose_error, open_grouped, open_source, open_source_with, optimize, print, print_raster,
    set_default_layer_state, source_kind, writable, write_full,
};
use crate::{CollageOptions, CollageSheets, Edit, StitchLayout, StitchOptions};

impl Session {
    /// The page count of another PDF (Replace Pages, Insert Pages dialogs).
    pub fn page_count_of(&self, name: &str, bytes: &Arc<Vec<u8>>) -> Result<usize, EditError> {
        Ok(pdfcraft_organize::page_count(&open_source(name, bytes)?)?)
    }

    /// A new blank document (Create ▸ Blank page).
    pub fn create_blank(&self, width: f64, height: f64, pages: usize) -> Result<Arc<Vec<u8>>, EditError> {
        self.write_new(&pdfcraft_create::blank(width, height, pages)?)
    }

    /// A new document with one page per image (PNG, JPEG).
    pub fn create_from_images(&self, images: &[(String, Vec<u8>)]) -> Result<Arc<Vec<u8>>, EditError> {
        self.write_new(&pdfcraft_create::from_images(images)?)
    }

    /// Create image pages at embedded resolution or a fixed dpi, without resampling.
    pub fn create_from_images_with_resolution(&self, images: &[(String, Vec<u8>)], resolution: ImageResolution) -> Result<Arc<Vec<u8>>, EditError> {
        self.write_new(&pdfcraft_create::from_images_with_resolution(images, resolution)?)
    }

    /// A new document from plain text (US Letter, 11 pt Helvetica). Text Helvetica can't draw is
    /// refused, naming the line and the character (never set as `?`).
    pub fn create_from_text(&self, title: &str, text: &str) -> Result<Arc<Vec<u8>>, EditError> {
        self.write_new(&pdfcraft_create::from_text(title, text, pdfcraft_create::LETTER, 11.0)?)
    }

    /// [`Self::create_from_text`] for a report PdfCraft writes from documents (comment summaries,
    /// compare reports), which can quote any language: characters Helvetica can't draw are written
    /// as `[U+XXXX]`, and the report's first line says so, rather than the report being refused.
    pub(crate) fn create_report(&self, title: &str, text: &str) -> Result<Arc<Vec<u8>>, EditError> {
        let (doc, _escaped) = pdfcraft_create::from_text_escaped(title, text, pdfcraft_create::LETTER, 11.0)?;
        self.write_new(&doc)
    }

    /// Convert a file Create understands (an image or plain text) to PDF bytes; a PDF is checked
    /// (it must open and allow copying pages) and returned as it is.
    pub fn convert_to_pdf(&self, name: &str, bytes: &Arc<Vec<u8>>) -> Result<(SourceKind, Arc<Vec<u8>>), EditError> {
        let Some(kind) = source_kind(name, bytes) else {
            return Err(EditError::Source(format!("{name}: this file type can't be converted; use a PDF, an image or a .txt file")));
        };
        let title = name.rsplit_once('.').map_or(name, |(s, _)| s);
        // Image decoders read untrusted bytes: a panic in one must not take the app down.
        let created = guard(|| match kind {
            SourceKind::Pdf => open_source(name, bytes).map(|_| bytes.clone()),
            SourceKind::Image => self.create_from_images(&[(name.to_string(), bytes.to_vec())]),
            SourceKind::Text => self.create_from_text(title, &decode_text(bytes)),
        })
        .map_err(|_| EditError::Source(format!("{name}: the file could not be read")))?;
        Ok((kind, created?))
    }

    /// A file for Combine as PDF bytes: an image or text file is converted; a PDF is returned as
    /// it is, unchecked (Combine reads it itself, with its password), as is a file named .pdf
    /// without a `%PDF-` header, which the reader may still open.
    pub fn as_pdf(&self, name: &str, bytes: &Arc<Vec<u8>>) -> Result<Arc<Vec<u8>>, EditError> {
        let pdf = match source_kind(name, bytes) {
            Some(SourceKind::Pdf) => true,
            Some(SourceKind::Image | SourceKind::Text) => false,
            None => name.to_ascii_lowercase().ends_with(".pdf"),
        };
        if pdf { Ok(bytes.clone()) } else { self.convert_to_pdf(name, bytes).map(|(_, pdf)| pdf) }
    }

    /// Convert to grayscale: every colour of document `id` becomes gray, in one undo step (see
    /// [`optimize::color::to_gray`]); the next save rewrites the file. Signed documents are
    /// refused, as the optimizer refuses them. Returns what was converted and what kept its
    /// colour.
    pub fn grayscale(&mut self, id: DocId) -> Result<optimize::color::GrayReport, EditError> {
        if self.get(id).ok_or(EditError::NoDocument)?.is_signed() {
            return Err(EditError::Signed);
        }
        self.apply(id, Edit::Grayscale)?;
        Ok(self.get(id).and_then(|d| d.gray_report.clone()).unwrap_or_default())
    }

    /// Reduce File Size: Acrobat's defaults (images above 225 ppi to 150 ppi, JPEG medium
    /// quality; thumbnails dropped), identical resources merged, unused objects dropped, objects
    /// packed into compressed object streams. Returns the bytes and how many objects were
    /// merged. The open document is not changed (Acrobat saves the reduced copy as a new file).
    pub fn reduced_bytes(&self, id: DocId) -> Result<(Arc<Vec<u8>>, usize), EditError> {
        let (bytes, report) = self.optimized_bytes(id, &optimize::Settings::default(), &[])?;
        Ok((bytes, report.merged))
    }

    /// Optimize PDF ▸ Advanced optimization: `settings` for images and objects, plus Remove
    /// Hidden Information's `discard` categories (user data). A full rewrite: signed documents
    /// are refused. The open document is not changed. This is [`Self::optimize_job`] run in
    /// place, without progress.
    pub fn optimized_bytes(&self, id: DocId, settings: &optimize::Settings, discard: &[Hidden]) -> Result<(Arc<Vec<u8>>, OptimizeReport), EditError> {
        self.optimize_job(id, settings, discard)?.run(|_| true)
    }

    /// Export comments and/or form data: XFDF and FDF carry either or both; XML, CSV and text
    /// are form data only.
    pub fn export_data(&self, id: DocId, format: DataFormat, comments: bool, fields: bool) -> Result<Vec<u8>, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let cos = match doc.editor.as_ref() {
            Some(e) => e.cos.clone(),
            None => {
                pdfcraft_cos::Document::open_with_password(doc.bytes.clone(), doc.password.as_deref()).map_err(|e| EditError::Write(e.to_string()))?
            }
        };
        // An XFA form's data is its datasets, as Acrobat exports it: XML or an XDP.
        let xfa = doc.info.xfa.is_some();
        if format == DataFormat::Xdp && !xfa {
            return Err(EditError::Invalid("XDP holds XFA form data; this document is not an XFA form (use .xml, .csv or .txt)".into()));
        }
        guard(|| match format {
            DataFormat::Xfdf => Ok(pdfcraft_xfdf::export_xfdf(&cos, comments, fields, &doc.name).into_bytes()),
            DataFormat::Fdf => pdfcraft_xfdf::export_fdf(&cos, comments, fields, &doc.name).map_err(|e| e.to_string()),
            DataFormat::Xml | DataFormat::Xdp if xfa => {
                pdfcraft_xfa::export_data(&cos, format == DataFormat::Xdp, &doc.name).map(String::into_bytes).map_err(|e| e.to_string())
            }
            other => Ok(pdfcraft_xfdf::export_data(&cos, other).into_bytes()),
        })
        .map_err(EditError::Write)?
        .map_err(EditError::Write)
    }

    /// The print-ready PDF for `settings` (sheets laid out for the paper; see `pdfcraft-print`).
    pub fn print_pdf(&self, id: DocId, settings: &print::Settings) -> Result<Vec<u8>, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        if !doc.allows_printing() {
            return Err(EditError::NotPermitted("printing"));
        }
        // Table 22, bit 12 clear: only a low-level representation may be printed, so the pages
        // always go as pictures, at no more than the low resolution.
        let low_res = doc.permissions().is_some_and(|p| !p.print_high_quality()).then(|| print::Raster {
            dpi: settings.raster.map_or(LOW_RESOLUTION_PRINT_DPI, |r| r.dpi.min(LOW_RESOLUTION_PRINT_DPI)),
            grayscale: settings.raster.is_some_and(|r| r.grayscale),
        });
        // Print as image: the pages used become pictures first, then the picture document is
        // imposed in their place (its page k is the k-th distinct source page used).
        if let Some(raster) = low_res.as_ref().or(settings.raster.as_ref()) {
            let mut distinct: Vec<usize> = Vec::new();
            for &p in &settings.pages {
                if !distinct.contains(&p) {
                    distinct.push(p);
                }
            }
            let bytes = print_raster::rasterize(doc, &distinct, raster, settings.content).map_err(EditError::Print)?;
            let pictures = pdfcraft_cos::Document::open(Arc::new(bytes)).map_err(|e| EditError::Print(e.to_string()))?;
            let remapped = print::Settings {
                pages: settings.pages.iter().map(|p| distinct.iter().position(|d| d == p).unwrap_or(0)).collect(),
                content: print::Content::Document,
                raster: None,
                ..settings.clone()
            };
            return pdfcraft_print::impose(&pictures, &remapped).map_err(|e| EditError::Print(e.to_string()));
        }
        let mut cos = match doc.editor.as_ref() {
            Some(e) => e.cos.clone(),
            None => {
                pdfcraft_cos::Document::open_with_password(doc.bytes.clone(), doc.password.as_deref()).map_err(|e| EditError::Write(e.to_string()))?
            }
        };
        // #3046: the layers print as they are shown. Once the Layers panel (or a set-layer
        // action) has changed one, the print copy's default configuration says which are on, so a
        // hidden layer stays off on paper too; untouched, the document's own configuration prints.
        if !doc.config.layers.is_empty() {
            set_default_layer_state(&mut cos, &doc.info.layers);
        }
        pdfcraft_print::impose(&cos, settings).map_err(|e| EditError::Print(e.to_string()))
    }

    /// For each page, whether its text mostly runs bottom-to-top ([`pdfcraft_print::text_reads_up`]);
    /// only `pages` are read, the others are `false`.
    pub fn print_text_up(&self, id: DocId, pages: &[usize]) -> Vec<bool> {
        let Some(doc) = self.get(id) else { return Vec::new() };
        let cos = match doc.editor.as_ref() {
            Some(e) => e.cos.clone(),
            None => match pdfcraft_cos::Document::open_with_password(doc.bytes.clone(), doc.password.as_deref()) {
                Ok(c) => c,
                Err(_) => return Vec::new(),
            },
        };
        (0..doc.info.pages.len()).map(|i| pages.contains(&i) && pdfcraft_print::text_reads_up(&cos, i)).collect()
    }

    /// Organize ▸ Layout: N-up, booklet or poster sheets as a **new** PDF (to open with
    /// [`Self::open_new`], named with [`impose_name`]). The pages are laid out as Print does
    /// ([`pdfcraft_print::impose`]), from the document as it is now, unsaved changes included; the
    /// document itself is not changed.
    ///
    /// The result is not encrypted, so a restricted document needs the Assembly permission (or
    /// the owner password), as Extract pages does. Only the multiple, booklet and poster layouts
    /// are made here, and never as pictures (`settings.raster` must be `None`).
    pub fn impose_new(&self, id: DocId, settings: print::Settings) -> Result<Arc<Vec<u8>>, EditError> {
        let src = self.cos(id)?;
        if src.permissions().is_some_and(|p| !p.assemble()) {
            return Err(EditError::NotPermitted("making a new document from its pages"));
        }
        if !matches!(settings.layout, print::Layout::Multiple { .. } | print::Layout::Booklet { .. } | print::Layout::Poster { .. }) {
            return Err(EditError::Invalid("choose a layout of several pages per sheet, a booklet or a poster".into()));
        }
        if settings.raster.is_some() {
            return Err(EditError::Invalid("a new document's pages can't be made as pictures here; leave print as image off".into()));
        }
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let mut cos = src.clone();
        // The layers come out as they are shown, as in Print.
        if !doc.config.layers.is_empty() {
            set_default_layer_state(&mut cos, &doc.info.layers);
        }
        let bytes = guard(|| pdfcraft_print::impose(&cos, &settings)).map_err(EditError::Write)?.map_err(impose_error)?;
        let bytes = Arc::new(bytes);
        // Checked like any new file before anyone writes or opens it: it reopens with its sheets.
        let out = pdfcraft_cos::Document::open(bytes.clone()).map_err(|e| EditError::Reopen(e.to_string()))?;
        let sheets = pdfcraft_organize::page_count(&out)?;
        pdfcraft_render::check_pages(bytes.clone(), None, sheets).map_err(|e| EditError::Reopen(e.to_string()))?;
        Ok(bytes)
    }

    /// Organize ▸ Layout ▸ Stitch pages: `pages` (0-based, in this order) placed one after another
    /// on a single long page, as a **new** PDF (to open with [`Self::open_new`], named with
    /// [`stitch_name`]), with where everything went. The pages come as they are now, unsaved
    /// changes included, and their layers as shown; links, comments and form fields are not
    /// carried over. The document itself is not changed.
    ///
    /// The result is not encrypted, so a restricted document needs the Assembly permission (or
    /// the owner password), as [`Self::impose_new`] does.
    pub fn stitch_new(&self, id: DocId, pages: &[usize], opts: &StitchOptions) -> Result<(Arc<Vec<u8>>, StitchLayout), EditError> {
        let src = self.cos(id)?;
        if src.permissions().is_some_and(|p| !p.assemble()) {
            return Err(EditError::NotPermitted("making a new document from its pages"));
        }
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let mut cos = src.clone();
        if !doc.config.layers.is_empty() {
            set_default_layer_state(&mut cos, &doc.info.layers);
        }
        let (out, layout) = guard(|| pdfcraft_organize::stitch_pages(&cos, pages, opts)).map_err(EditError::Write)??;
        Ok((self.write_new(&out)?, layout))
    }

    /// Organize ▸ Layout ▸ Collage: `pages` (0-based, in this order) arranged on sheets of paper,
    /// a grid of them per sheet, at their real size or fitted to the cells, as a **new** PDF (to
    /// open with [`Self::open_new`], named with [`collage_name`]), with what went on each sheet.
    /// The pages come as they are now, unsaved changes included, and their layers as shown;
    /// links, comments and form fields are not carried over. The document itself is not changed.
    ///
    /// The result is not encrypted, so a restricted document needs the Assembly permission (or
    /// the owner password), as [`Self::stitch_new`] does.
    pub fn collage_new(&self, id: DocId, pages: &[usize], opts: &CollageOptions) -> Result<(Arc<Vec<u8>>, CollageSheets), EditError> {
        let src = self.cos(id)?;
        if src.permissions().is_some_and(|p| !p.assemble()) {
            return Err(EditError::NotPermitted("making a new document from its pages"));
        }
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let mut cos = src.clone();
        if !doc.config.layers.is_empty() {
            set_default_layer_state(&mut cos, &doc.info.layers);
        }
        let (out, sheets) = guard(|| pdfcraft_organize::collage_pages(&cos, pages, opts)).map_err(EditError::Write)??;
        Ok((self.write_new(&out)?, sheets))
    }

    /// Combine whole files, in order, into new PDF bytes (one bookmark per file).
    pub fn combine(&self, sources: &[(String, Arc<Vec<u8>>)]) -> Result<Arc<Vec<u8>>, EditError> {
        let docs = sources.iter().map(|(n, b)| open_source(n, b)).collect::<Result<Vec<_>, _>>()?;
        let named: Vec<(&str, &pdfcraft_cos::Document)> = sources.iter().map(|(n, _)| n.as_str()).zip(docs.iter()).collect();
        let out = pdfcraft_organize::combine(&named)?;
        self.write_new(&out)
    }

    /// Combine Files with a page range per file ("1-3, 6"; `None` or empty for all pages).
    pub fn combine_ranges(&self, sources: &[CombineSource]) -> Result<Arc<Vec<u8>>, EditError> {
        self.combine_unlocked(sources, &[])
    }

    /// Combine sources one at a time, in order (Create ▸ Multiple files), to the same result as
    /// [`Self::combine_ranges`]. Each item is a source's bookmark title, its PDF bytes (see
    /// [`Self::convert_to_pdf`]) and its page range (`None` for all pages). Items are pulled one at a
    /// time: a source is opened, its pages are copied into the result, and its bytes are released
    /// before the next item is read. So the number of sources is not limited, and one source's
    /// document is held at a time. An item's error stops the combine and is returned as it is.
    /// `Ok(None)` when there are no sources.
    pub fn combine_each<I, E>(&self, sources: I) -> Result<Option<Arc<Vec<u8>>>, E>
    where
        I: IntoIterator<Item = Result<CombineSource, E>>,
        E: From<EditError>,
    {
        let mut combiner = pdfcraft_organize::Combiner::new();
        let mut added = false;
        for source in sources {
            let (title, bytes, range) = source?;
            let doc = open_source(&title, &bytes)?;
            let range = range.as_deref().map(str::trim).filter(|r| !r.is_empty());
            let pages = match range {
                Some(r) => {
                    let n = pdfcraft_organize::page_count(&doc).map_err(EditError::from)?;
                    let p = pdfcraft_print::select_pages(n, Some(r), &[], pdfcraft_print::Subset::All, false)
                        .map_err(|e| EditError::Print(format!("{title}: {e}")))?;
                    Some(p)
                }
                None => None,
            };
            combiner.add(&title, &doc, pages.as_deref()).map_err(EditError::from)?;
            added = true;
        }
        if !added {
            return Ok(None);
        }
        let out = combiner.finish().map_err(EditError::from)?;
        Ok(Some(self.write_new(&out)?))
    }

    /// [`Self::combine_ranges`] with the password each encrypted source is opened with, by
    /// position (missing or `None`: no password). The result is not encrypted.
    pub fn combine_unlocked(&self, sources: &[CombineSource], passwords: &[Option<&str>]) -> Result<Arc<Vec<u8>>, EditError> {
        Ok(self.combine_unlocked_reporting(sources, passwords, pdfcraft_organize::FieldConflicts::Rename)?.bytes)
    }

    /// The pre-flight over byte sources: what combining them, in order, would rename in each one
    /// (whole files; page ranges are the combine's to apply). A source that cannot be opened
    /// previews as nothing — its row already says why it cannot be combined.
    pub fn combine_field_conflicts(&self, sources: &[CombineSource], passwords: &[Option<&str>]) -> Vec<Vec<(String, String)>> {
        let docs: Vec<Option<pdfcraft_cos::Document>> =
            sources.iter().enumerate().map(|(i, (n, b, _))| open_source_with(n, b, passwords.get(i).copied().flatten()).ok()).collect();
        let refs: Vec<&pdfcraft_cos::Document> = docs.iter().filter_map(Option::as_ref).collect();
        let mut opened = pdfcraft_organize::combine_field_conflicts(&refs).into_iter();
        // One preview per source, in step with it: a file that cannot be opened has none.
        docs.iter().map(|d| if d.is_some() { opened.next().unwrap_or_default() } else { Default::default() }).collect()
    }

    /// [`Self::combine_unlocked`] reporting what the conflict resolution renamed.
    pub fn combine_unlocked_reporting(
        &self,
        sources: &[CombineSource],
        passwords: &[Option<&str>],
        field_conflicts: pdfcraft_organize::FieldConflicts,
    ) -> Result<Combined, EditError> {
        let docs = sources
            .iter()
            .enumerate()
            .map(|(i, (n, b, _))| open_source_with(n, b, passwords.get(i).copied().flatten()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut pages = Vec::with_capacity(docs.len());
        for ((name, _, range), d) in sources.iter().zip(&docs) {
            let range = range.as_deref().map(str::trim).filter(|r| !r.is_empty());
            pages.push(match range {
                Some(r) => {
                    let n = pdfcraft_organize::page_count(d)?;
                    let p = pdfcraft_print::select_pages(n, Some(r), &[], pdfcraft_print::Subset::All, false)
                        .map_err(|e| EditError::Print(format!("{name}: {e}")))?;
                    Some(p)
                }
                None => None,
            });
        }
        let named: Vec<(&str, &pdfcraft_cos::Document, Option<&[usize]>)> =
            sources.iter().zip(docs.iter()).zip(&pages).map(|(((n, _, _), d), p)| (n.as_str(), d, p.as_deref())).collect();
        let out = pdfcraft_organize::combine_selected_with(&named, field_conflicts)?;
        let bytes = self.write_new(&out.doc)?;
        Ok(Combined { bytes, renamed_fields: out.renamed_fields })
    }

    /// [`Self::combine_unlocked`] where sources with the same `groups` key (by position) are one
    /// file split into several runs placed apart: each file is opened and copied once, so it keeps
    /// one bookmark, its links between its own pages, its fields and its attachments whole (see
    /// `pdfcraft_organize::combine_grouped`). A group's later sources use its first source's bytes
    /// and password; a source without a key is a file of its own.
    pub fn combine_grouped(&self, sources: &[CombineSource], groups: &[u64], passwords: &[Option<&str>]) -> Result<Arc<Vec<u8>>, EditError> {
        Ok(self.combine_grouped_reporting(sources, groups, passwords, pdfcraft_organize::FieldConflicts::Rename)?.bytes)
    }

    /// [`Self::combine_grouped`] reporting what the conflict resolution renamed.
    pub fn combine_grouped_reporting(
        &self,
        sources: &[CombineSource],
        groups: &[u64],
        passwords: &[Option<&str>],
        field_conflicts: pdfcraft_organize::FieldConflicts,
    ) -> Result<Combined, EditError> {
        let (docs, file, pages) = open_grouped(sources, groups, passwords)?;
        let mut runs: Vec<pdfcraft_organize::Run<'_>> = Vec::with_capacity(sources.len());
        for (((name, _, _), at), p) in sources.iter().zip(&file).zip(&pages) {
            let Some(d) = docs.get(*at) else { return Err(EditError::Write(format!("{name}: not opened"))) };
            runs.push((*at, name.as_str(), d, p.as_deref()));
        }
        let out = pdfcraft_organize::combine_grouped_with(&runs, field_conflicts)?;
        let bytes = self.write_new(&out.doc)?;
        Ok(Combined { bytes, renamed_fields: out.renamed_fields })
    }

    /// Interleave files (organize.interleave): up to `per_turn` pages from each source in turn,
    /// round and round (A1, B1, A2, B2, …); a source whose `reverse` entry (by position) is `true`
    /// gives its pages last to first, as the backs of a duplex scan come out; when a source runs out
    /// the others go on. Each source's pages are its range, or all of them. Sources share files as
    /// in [`Self::combine_grouped`] (`groups` may be empty: each source a file of its own), and
    /// each file is copied once, keeping its bookmarks, links and fields. Errors when `per_turn`
    /// is 0.
    pub fn combine_interleaved_reporting(
        &self,
        sources: &[CombineSource],
        groups: &[u64],
        passwords: &[Option<&str>],
        per_turn: usize,
        reverse: &[bool],
        field_conflicts: pdfcraft_organize::FieldConflicts,
    ) -> Result<Combined, EditError> {
        let (docs, file, pages) = open_grouped(sources, groups, passwords)?;
        let mut lists = Vec::with_capacity(sources.len());
        for ((name, _, _), (at, chosen)) in sources.iter().zip(file.iter().zip(pages)) {
            lists.push(match chosen {
                Some(p) => p,
                None => {
                    let Some(d) = docs.get(*at) else { return Err(EditError::Write(format!("{name}: not opened"))) };
                    (0..pdfcraft_organize::page_count(d)?).collect()
                }
            });
        }
        let turns = pdfcraft_organize::interleave_runs(&lists, per_turn, reverse)?;
        let mut runs: Vec<pdfcraft_organize::Run<'_>> = Vec::with_capacity(turns.len());
        for (source, chosen) in &turns {
            let (Some((name, _, _)), Some(at)) = (sources.get(*source), file.get(*source)) else { continue };
            let Some(d) = docs.get(*at) else { return Err(EditError::Write(format!("{name}: not opened"))) };
            runs.push((*at, name.as_str(), d, Some(chosen.as_slice())));
        }
        if runs.is_empty() {
            return Err(EditError::Print("there are no pages to interleave".into()));
        }
        let out = pdfcraft_organize::combine_grouped_with(&runs, field_conflicts)?;
        let bytes = self.write_new(&out.doc)?;
        Ok(Combined { bytes, renamed_fields: out.renamed_fields })
    }

    /// New PDF bytes containing copies of `pages` of the document (Extract Pages).
    pub fn extract(&self, id: DocId, pages: &[usize]) -> Result<Arc<Vec<u8>>, EditError> {
        let src = self.cos(id)?;
        if src.permissions().is_some_and(|p| !p.assemble()) {
            return Err(EditError::NotPermitted("extracting pages"));
        }
        let out = pdfcraft_organize::extract_pages(src, pages)?;
        self.write_new(&out)
    }

    /// Split the document into several new PDFs.
    pub fn split(&self, id: DocId, by: &pdfcraft_organize::SplitBy) -> Result<Vec<SplitPart>, EditError> {
        let src = self.cos(id)?;
        if src.permissions().is_some_and(|p| !p.assemble()) {
            return Err(EditError::NotPermitted("splitting the document"));
        }
        let n = pdfcraft_organize::page_count(src)?;
        pdfcraft_organize::split_ranges(n, by)
            .into_iter()
            .map(|r| {
                let doc = pdfcraft_organize::extract_pages(src, &r.clone().collect::<Vec<_>>())?;
                Ok((r.start + 1, r.end, self.write_new(&doc)?))
            })
            .collect()
    }

    /// Split into parts of at most `max_bytes` each (Acrobat's "File size"): pages are taken in
    /// order while their size (as single-page files, an over-estimate because shared fonts and
    /// images count once per part) fits; a page larger than the limit is a part of its own.
    pub fn split_by_size(&self, id: DocId, max_bytes: usize) -> Result<Vec<SplitPart>, EditError> {
        let src = self.cos(id)?;
        if src.permissions().is_some_and(|p| !p.assemble()) {
            return Err(EditError::NotPermitted("splitting the document"));
        }
        let n = pdfcraft_organize::page_count(src)?;
        let mut cuts = Vec::new();
        let mut used = 0usize;
        for p in 0..n {
            let size = self.write_unchecked(&pdfcraft_organize::extract_pages(src, &[p])?)?.len();
            if used > 0 && used + size > max_bytes {
                cuts.push(p);
                used = 0;
            }
            used += size;
        }
        self.split(id, &pdfcraft_organize::SplitBy::Before(cuts))
    }

    pub(crate) fn cos(&self, id: DocId) -> Result<&pdfcraft_cos::Document, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        doc.editor.as_ref().map(|e| &e.cos).ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))
    }

    /// A new file (Combine, Extract, Split, Create) as bytes, checked like a save before anyone
    /// writes it (#1698, #2166): it reopens, and shows the pages it was built from.
    fn write_new(&self, doc: &pdfcraft_cos::Document) -> Result<Arc<Vec<u8>>, EditError> {
        let bytes = self.write_unchecked(doc)?;
        let expected = pdfcraft_organize::page_count(doc)?;
        pdfcraft_cos::Document::open(bytes.clone()).map_err(|e| EditError::Reopen(e.to_string()))?;
        pdfcraft_render::check_pages(bytes.clone(), None, expected).map_err(|e| EditError::Reopen(e.to_string()))?;
        Ok(bytes)
    }

    /// [`Self::write_new`] without the check, for bytes nobody writes: measuring a part's size.
    /// Like a save, a new file never carries a typed Password-field value (#3926): Extract and
    /// Split copy the fields of the pages they take.
    fn write_unchecked(&self, doc: &pdfcraft_cos::Document) -> Result<Arc<Vec<u8>>, EditError> {
        let opts = SaveOptions { mod_date: self.now().map(pdfcraft_cos::pdf_date), ..SaveOptions::default() };
        let doc = writable(doc)?;
        write_full(&doc, &opts).map(Arc::new).map_err(|e| EditError::Write(e.to_string()))
    }
}
