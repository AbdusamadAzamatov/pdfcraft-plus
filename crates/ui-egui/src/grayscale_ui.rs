//! Compress ▸ Clean up ▸ Convert to grayscale (CU3): a confirmation, then the conversion in one
//! undo step ([`pdfcraft_engine::Session::grayscale`]), then what was converted and what kept
//! its colour, by page.

use pdfcraft_engine::optimize::color::{GrayReport, KeptKind};
use pdfcraft_engine::{Edit, EditError};

use crate::theme::{self, Tokens};
use crate::{Dialog, PdfCraftApp, widgets};

fn kind_label(kind: KeptKind) -> &'static str {
    match kind {
        KeptKind::Shading => tl!("Shadings"),
        KeptKind::Pattern => tl!("Patterns"),
        KeptKind::SpotColor => tl!("Spot colours"),
        KeptKind::LabColor => tl!("Lab colours"),
        KeptKind::OtherColor => tl!("Other colours"),
        KeptKind::Image => tl!("Images"),
        KeptKind::InlineImage => tl!("Inline images"),
        KeptKind::PageContent => tl!("Page content"),
    }
}

/// The dialog: the confirmation (Convert, Cancel) before, the report (OK) after. Returns
/// (convert, close).
pub(crate) fn body(ui: &mut egui::Ui, report: Option<&GrayReport>, t: &Tokens) -> (bool, bool) {
    ui.set_width(460.0);
    ui.label(egui::RichText::new(tl!("Convert to grayscale")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    let Some(r) = report else {
        ui.label(tl!("Every colour in the document becomes a shade of gray: text, drawings and pictures. What can't be converted keeps its colour and is listed afterwards."));
        ui.add_space(4.0);
        ui.label(egui::RichText::new(tl!("Save the document afterwards; saving rewrites the whole file.")).small().color(t.text_muted));
        ui.add_space(12.0);
        let (mut convert, mut cancel) = (false, false);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                convert = widgets::pill_button(ui, tl!("Convert"), true).clicked();
                cancel = widgets::pill_button(ui, tl!("Cancel"), false).clicked();
            })
        });
        return (convert, cancel);
    };
    let n = |v: usize| v.to_string();
    ui.label(crate::i18n::fmt(tl!("Colours made gray: {n}"), &[("n", &n(r.colors))]));
    ui.label(crate::i18n::fmt(tl!("Images made gray: {n}"), &[("n", &n(r.images + r.palettes))]));
    if !r.kept.is_empty() {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(tl!("Kept their colour:")).font(theme::semibold(13.0)));
        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
            egui::Grid::new("gray-kept").num_columns(3).striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
                for k in &r.kept {
                    ui.label(match k.page {
                        Some(p) => crate::i18n::fmt(tl!("Page {page}"), &[("page", &p.to_string())]),
                        None => "—".to_string(),
                    });
                    ui.label(kind_label(k.kind));
                    ui.label(k.count.to_string());
                    ui.end_row();
                }
            });
        });
    }
    ui.add_space(12.0);
    let mut ok = false;
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ok = widgets::pill_button(ui, tl!("OK"), true).clicked())
    });
    (false, ok)
}

impl PdfCraftApp {
    /// Open Convert to grayscale's confirmation for the active document.
    pub(crate) fn open_grayscale(&mut self) {
        if self.active_ids().is_none() {
            return;
        }
        self.gray_result = None;
        self.dialog = Some(Dialog::Grayscale);
    }

    /// Convert the active document to gray (one undo step); the report goes to the dialog.
    /// Returns whether it converted; if not, a notice says why. Signed documents are refused.
    pub(crate) fn convert_to_grayscale(&mut self) -> bool {
        // What's typed in a form field is part of the document (#166).
        if !self.commit_form_typing() {
            return false;
        }
        let Some((_, id)) = self.active_ids() else { return false };
        if self.session.get(id).is_some_and(|d| d.is_signed()) {
            let label = crate::i18n::edit_label(&Edit::Grayscale.label());
            self.notify_fmt("{label} failed: {e}", &[("label", &label), ("e", &EditError::Signed.to_string())]);
            return false;
        }
        if !self.apply_edit(Edit::Grayscale) {
            return false;
        }
        self.gray_result = Some(self.session.get(id).and_then(|d| d.gray_report.clone()).unwrap_or_default());
        true
    }
}
