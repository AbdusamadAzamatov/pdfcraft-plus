//! PdfCraft Plus tools whose handling is more than a call: rotation by any angle, rasterizing,
//! greyscale, look-alike fonts and removing restrictions. `dispatch` routes to them.

use super::error::{Result, ToolError, failed};
use super::json::{Args, security, summary};
use super::{Automation, Edit, Value, json};

impl Automation {
    /// Rotate page content by any angle (organize.rotate-angle), optionally flattening first.
    pub(crate) fn page_rotate_angle(&mut self, a: &Args) -> Result<Value> {
        let degrees = a.opt_num("degrees")?.ok_or_else(|| ToolError::InvalidArgs("degrees is required: a number from -180 to 180".into()))?;
        let fit = match a.opt_str("fit")? {
            None => pdfcraft_engine::RotateFit::Expand,
            Some(f) => pdfcraft_engine::RotateFit::from_name(f)
                .ok_or_else(|| ToolError::InvalidArgs(format!("unknown fit {f:?} (expand, keep_size, crop)")))?,
        };
        let pages = match a.opt_ints("pages")? {
            Some(_) => self.pages(a, "pages")?,
            None => (0..self.doc(a)?.info.pages.len()).collect(),
        };
        let n = pages.len();
        if n == 0 {
            return Err(ToolError::InvalidArgs("pages is empty: list the 1-based pages to rotate, or omit it for all".into()));
        }
        let edit = if a.opt_bool("flatten")?.unwrap_or(false) {
            Edit::flatten_and_rotate(pages, degrees, fit)
        } else {
            Edit::RotateContent { pages, degrees, fit }
        };
        let mut out = self.apply(a, edit)?;
        out["rotated"] = json!(n);
        Ok(out)
    }

    /// Turn pages into images at a resolution (PdfCraft Plus), keeping or burning in comments.
    pub(crate) fn page_rasterize(&mut self, a: &Args) -> Result<Value> {
        let pages = match a.opt_ints("pages")? {
            Some(_) => self.pages(a, "pages")?,
            None => (0..self.doc(a)?.info.pages.len()).collect(),
        };
        let dpi = a.opt_num("dpi")?.unwrap_or(pdfcraft_engine::rasterize::DEFAULT_DPI);
        if !dpi.is_finite() || !(pdfcraft_engine::rasterize::MIN_DPI..=pdfcraft_engine::rasterize::MAX_DPI).contains(&dpi) {
            return Err(ToolError::InvalidArgs(format!("dpi must be between 72 and 600, not {dpi}")));
        }
        let burn_comments = match a.opt_str("comments")?.unwrap_or("keep") {
            "keep" => false,
            "burn" => true,
            c => return Err(ToolError::InvalidArgs(format!("unknown comments {c:?} (keep, burn)"))),
        };
        let options = pdfcraft_engine::rasterize::RasterizeOptions { dpi, grayscale: a.opt_bool("grayscale")?.unwrap_or(false), burn_comments };
        let id = self.doc(a)?.id;
        let n = {
            let mut d = pages.clone();
            d.sort_unstable();
            d.dedup();
            d.len()
        };
        self.session.rasterize_pages(id, &pages, options).map_err(failed)?;
        let mut out = summary(self.doc(a)?);
        out["rasterized"] = json!(n);
        Ok(out)
    }

    /// Convert the document to greyscale and report what was converted and what stayed in colour.
    pub(crate) fn doc_grayscale(&mut self, a: &Args) -> Result<Value> {
        let id = self.doc(a)?.id;
        let r = self.session.grayscale(id).map_err(failed)?;
        let mut out = summary(self.doc(a)?);
        out["colors"] = json!(r.colors);
        out["streams"] = json!(r.streams);
        out["images"] = json!(r.images);
        out["palettes"] = json!(r.palettes);
        out["kept"] = json!(r.kept.iter().map(|k| json!({ "kind": k.kind.name(), "page": k.page, "count": k.count })).collect::<Vec<_>>());
        out["kept_lines"] = json!(r.kept_lines());
        Ok(out)
    }

    /// The subset fonts on a page and the free look-alike each can be shown with.
    pub(crate) fn font_lookalikes(&mut self, a: &Args) -> Result<Value> {
        let page = self.page(a)?;
        let lines = self.doc(a)?.text_lines(page);
        let mut fonts: Vec<String> = Vec::new();
        for line in lines {
            let subset = line.base_font.split_once('+').is_some_and(|(tag, _)| tag.len() == 6 && tag.bytes().all(|b| b.is_ascii_uppercase()));
            if subset && !fonts.contains(&line.base_font) {
                fonts.push(line.base_font);
            }
        }
        let listed: Vec<Value> = fonts
            .iter()
            .map(|base| {
                let lookalike = pdfcraft_fonts::lookalike::for_font(base).map(|(l, bold, italic)| {
                    let files: Vec<Value> = l
                        .files
                        .iter()
                        .map(|f| json!({"style": f.style, "file": f.file, "url": f.url, "sha256": f.sha256, "size": f.size, "installed": pdfcraft_fonts::lookalike::is_installed(f)}))
                        .collect();
                    let needed = l.file(bold, italic).map(|f| f.file);
                    json!({"family": l.family, "replaces": l.replaces, "licence": l.licence, "needed": needed, "files": files})
                });
                json!({"font": base, "subset": true, "lookalike": lookalike})
            })
            .collect();
        Ok(json!({"page": page + 1, "fonts": listed}))
    }

    /// Install downloaded look-alike font files, each checked against the catalogue.
    pub(crate) fn font_lookalike_install(&mut self, a: &Args) -> Result<Value> {
        let paths = a.get("paths").and_then(Value::as_array).ok_or_else(|| ToolError::InvalidArgs("paths must list the downloaded files".into()))?;
        if paths.is_empty() || paths.len() > 20 {
            return Err(ToolError::InvalidArgs("paths must list 1 to 20 files".into()));
        }
        let mut installed = Vec::new();
        let mut refused = Vec::new();
        for p in paths {
            let p = p.as_str().ok_or_else(|| ToolError::InvalidArgs("each path must be a string".into()))?;
            let path = self.resolve(p, false)?;
            use std::io::Read;
            let mut bytes = Vec::new();
            // Every catalogue file is under 1 MB.
            let read = std::fs::File::open(&path).and_then(|f| f.take((1 << 20) + 1).read_to_end(&mut bytes));
            match read.map_err(|e| e.to_string()).and_then(|_| pdfcraft_fonts::lookalike::install(bytes)) {
                Ok(f) => installed.push(json!({"path": p, "file": f.file})),
                Err(e) => refused.push(json!({"path": p, "error": e})),
            }
        }
        Ok(json!({"installed": installed, "refused": refused}))
    }

    /// Remove permission restrictions, once the caller confirms the right to edit the file.
    pub(crate) fn doc_remove_restrictions(&mut self, a: &Args) -> Result<Value> {
        if a.opt_bool("confirmed_rights")? != Some(true) {
            return Err(ToolError::InvalidArgs("confirmed_rights must be true to confirm that you have the right to edit this file".into()));
        }
        let mut out = self.apply(a, Edit::RemoveRestrictions { confirmed_rights: true })?;
        out["security"] = security(self.doc(a)?);
        Ok(out)
    }
}
