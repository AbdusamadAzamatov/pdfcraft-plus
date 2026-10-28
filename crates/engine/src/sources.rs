//! Other files to combine with or copy pages from: opening them, and why one can't be used.

use std::sync::Arc;

use crate::{EditError, SourceProblem, print};

/// A file to combine: its name (the bookmark title), bytes, and page range (`None`: all).
pub type CombineSource = (String, Arc<Vec<u8>>, Option<String>);

/// What a combine produced: the new PDF bytes, and the fields renamed to keep colliding names
/// separate (old fully-qualified name → new one), in order across the sources.
#[derive(Debug)]
pub struct Combined {
    pub bytes: Arc<Vec<u8>>,
    pub renamed_fields: Vec<(String, String)>,
}

/// Parse another PDF to copy pages from.
pub(crate) fn open_source(name: &str, bytes: &Arc<Vec<u8>>) -> Result<pdfcraft_cos::Document, EditError> {
    open_source_with(name, bytes, None)
}

/// [`open_source`], authenticating with `password` (user or owner) for an encrypted file.
pub(crate) fn open_source_with(name: &str, bytes: &Arc<Vec<u8>>, password: Option<&str>) -> Result<pdfcraft_cos::Document, EditError> {
    source_document(bytes, password).map_err(|p| EditError::Source(format!("{name}: {p}")))
}

/// Open combine sources where sources with the same `groups` key (by position) are one file:
/// the documents (one per file), each source's file in that list, and each source's chosen pages
/// (its range, or `None` for all). A group's later sources use its first source's password.
#[allow(clippy::type_complexity)]
pub(crate) fn open_grouped(
    sources: &[CombineSource],
    groups: &[u64],
    passwords: &[Option<&str>],
) -> Result<(Vec<pdfcraft_cos::Document>, Vec<usize>, Vec<Option<Vec<usize>>>), EditError> {
    let (mut keys, mut docs, mut file): (Vec<Option<u64>>, Vec<pdfcraft_cos::Document>, Vec<usize>) = (Vec::new(), Vec::new(), Vec::new());
    for (i, (name, bytes, _)) in sources.iter().enumerate() {
        let key = groups.get(i).copied();
        let known = key.and_then(|k| keys.iter().position(|g| *g == Some(k)));
        let at = match known {
            Some(at) => at,
            None => {
                docs.push(open_source_with(name, bytes, passwords.get(i).copied().flatten())?);
                keys.push(key);
                docs.len() - 1
            }
        };
        file.push(at);
    }
    let mut pages = Vec::with_capacity(sources.len());
    for ((name, _, range), at) in sources.iter().zip(&file) {
        let range = range.as_deref().map(str::trim).filter(|r| !r.is_empty());
        let Some(d) = docs.get(*at) else { return Err(EditError::Write(format!("{name}: not opened"))) };
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
    Ok((docs, file, pages))
}

/// What [`Session::impose_new`] says when the sheets can't be laid out: what was wrong, and what to
/// change.
pub(crate) fn impose_error(e: pdfcraft_print::PrintError) -> EditError {
    use pdfcraft_print::PrintError as P;
    match e {
        P::NoPages => EditError::Invalid("there are no pages to lay out: choose at least one page".into()),
        P::OutOfRange(..) | P::NotAPage(_) => EditError::Invalid(format!("{e}; give pages such as \"1-3, 6\"")),
        P::Invalid(why) => EditError::Invalid(format!("{why}; change the layout or paper and try again")),
        P::Spool(why) => EditError::Print(why),
        P::Cos(e) => EditError::Write(e.to_string()),
    }
}

/// The name [`Session::impose_new`]'s result opens under: `<name> (2-up).pdf`, `<name> (booklet).pdf`
/// or `<name> (poster).pdf`, from the document's name with its `.pdf` taken off.
pub fn impose_name(name: &str, layout: &print::Layout) -> String {
    let stem = name.strip_suffix(".pdf").or_else(|| name.strip_suffix(".PDF")).unwrap_or(name);
    let what = match layout {
        print::Layout::Multiple { per_sheet, .. } => format!("{per_sheet}-up"),
        print::Layout::Booklet { .. } => "booklet".into(),
        print::Layout::Poster { .. } => "poster".into(),
        print::Layout::Size(_) | print::Layout::Label { .. } => "print".into(),
    };
    format!("{stem} ({what}).pdf")
}

/// The name [`Session::collage_new`]'s result opens under: `<name> (collage).pdf`.
pub fn collage_name(name: &str) -> String {
    let stem = name.strip_suffix(".pdf").or_else(|| name.strip_suffix(".PDF")).unwrap_or(name);
    format!("{stem} (collage).pdf")
}

/// The name [`Session::stitch_new`]'s result opens under: `<name> (stitched).pdf`.
pub fn stitch_name(name: &str) -> String {
    let stem = name.strip_suffix(".pdf").or_else(|| name.strip_suffix(".PDF")).unwrap_or(name);
    format!("{stem} (stitched).pdf")
}

fn source_document(bytes: &Arc<Vec<u8>>, password: Option<&str>) -> Result<pdfcraft_cos::Document, SourceProblem> {
    match std::panic::catch_unwind(|| pdfcraft_cos::Document::open_with_password(bytes.clone(), password)) {
        Ok(Ok(d)) if d.permissions().is_some_and(|p| !p.assemble()) => Err(SourceProblem::NotPermitted),
        Ok(Ok(d)) => Ok(d),
        Ok(Err(pdfcraft_cos::CosError::NeedsPassword)) => Err(SourceProblem::Password),
        Ok(Err(pdfcraft_cos::CosError::WrongPassword)) => Err(SourceProblem::WrongPassword),
        Ok(Err(e)) => Err(SourceProblem::Unreadable(e.to_string())),
        Err(_) => Err(SourceProblem::Unreadable("the file could not be read".into())),
    }
}

/// Whether `bytes` can be combined, opened with `password` (user or owner) if given, and if not
/// why: what Combine would refuse it for, so the Combine files list can say so before Combine
/// is pressed. The owner (permissions) password lifts the restriction on copying pages.
pub fn combine_source_check(bytes: &Arc<Vec<u8>>, password: Option<&str>) -> Result<(), SourceProblem> {
    source_document(bytes, password).map(|_| ())
}

/// The number of pages of `bytes`, opened with `password` (user or owner) if given, whatever
/// its permissions allow; `None` when it can't be read.
pub fn source_page_count(bytes: &Arc<Vec<u8>>, password: Option<&str>) -> Option<usize> {
    let doc = std::panic::catch_unwind(|| pdfcraft_cos::Document::open_with_password(bytes.clone(), password)).ok()?.ok()?;
    pdfcraft_organize::page_count(&doc).ok()
}
