//! PDF object layer (`craft-pdf::cos`).

pub use craft_pdf::cos::*;

/// What the security the next save writes allows, including pending protection changes: `None`
/// (everything) for unencrypted output, so removing security lifts its restrictions at once.
/// [`Document::permissions`] reports the opening password's rights instead.
pub fn output_permissions(doc: &Document) -> Option<Permissions> {
    doc.output_handler().map(|h| h.permissions())
}
