//! Edit a PDF ▸ Add content ▸ Paging seal: one stamp picture cut into strips across the edges of
//! the chosen pages, so it reads whole when the pages are fanned out (a cross-page seal). Apply
//! adds the strips as added images in one undoable step ([`pdfcraft_engine::Edit::AddPagingSeal`]).

use std::sync::Arc;

use egui::{Align, Layout};
use pdfcraft_engine::paging_seal::{MAX_PAGES_PER_SEAL, PagingSeal, SealEdge};

use crate::divide_ui::Range;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Dialog state.
#[derive(Clone, Debug, PartialEq)]
pub struct SealDraft {
    /// The seal picture's file name and bytes, once chosen.
    pub picture: Option<(String, Arc<Vec<u8>>)>,
    pub seal: PagingSeal,
    pub range: Range,
    /// The pages (0-based) selected when the dialog opened.
    pub selection: Vec<usize>,
}

impl Default for SealDraft {
    fn default() -> Self {
        Self { picture: None, seal: PagingSeal::default(), range: Range::All, selection: Vec::new() }
    }
}

impl SealDraft {
    /// The pages (0-based, in order) to seal in a document of `count` pages.
    pub fn pages(&self, count: usize) -> Vec<usize> {
        match self.range {
            Range::All => (0..count).collect(),
            Range::Selection => self.selection.iter().copied().filter(|p| *p < count).collect(),
            Range::Pages(a, b) => (a.max(1) - 1..b.min(count)).collect(),
        }
    }
}

/// What the dialog's buttons asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Clicked {
    pub apply: bool,
    pub cancel: bool,
    pub choose: bool,
}

impl PdfCraftApp {
    /// Open Paging seal on the active document: the selected pages when there are any, else
    /// every page. The picture and the other choices stay from last time.
    pub fn open_paging_seal(&mut self) {
        let Some((i, _)) = self.active_ids() else { return };
        let selection: Vec<usize> = self.views.get(i).map(|v| v.selected.iter().copied().collect()).unwrap_or_default();
        self.seal_draft.range = if selection.is_empty() { Range::All } else { Range::Selection };
        self.seal_draft.selection = selection;
        self.dialog = Some(crate::Dialog::PagingSeal);
    }

    /// Choose picture…: the seal picture, kept in the draft.
    pub(crate) fn choose_seal_picture(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.pick_image(tl!("Choose the seal picture"), |app, name, bytes| app.seal_draft.picture = Some((name, Arc::new(bytes))));
        #[cfg(target_arch = "wasm32")]
        self.notify_tr("Adding images arrives on the web with file pickers for images");
    }

    /// Apply: the strips on the chosen pages. Returns whether the seal was added.
    pub(crate) fn seal_with_draft(&mut self) -> bool {
        let Some((_, id)) = self.active_ids() else { return false };
        let Some(count) = self.session.get(id).map(|d| d.info.pages.len()) else { return false };
        let Some((name, bytes)) = self.seal_draft.picture.clone() else { return false };
        let pages = self.seal_draft.pages(count);
        self.apply_edit(pdfcraft_engine::Edit::AddPagingSeal { pages, name, bytes, seal: self.seal_draft.seal })
    }
}

/// Draw the dialog.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> Clicked {
    let mut clicked = Clicked::default();
    let Some((_, id)) = app.active_ids() else {
        clicked.cancel = true;
        return clicked;
    };
    let count = app.session.get(id).map_or(1, |d| d.info.pages.len().max(1));
    let d = &mut app.seal_draft;
    ui.set_width(440.0);
    ui.label(egui::RichText::new(tl!("Paging seal…").trim_end_matches('…')).font(theme::semibold(18.0)));
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(tl!("One seal is cut into strips, one on the edge of each page, so it reads whole when the pages are fanned out."))
            .small()
            .color(t.text_muted),
    );
    ui.add_space(8.0);
    egui::Grid::new("seal-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(tl!("Picture:"));
        ui.horizontal(|ui| {
            match &d.picture {
                Some((name, _)) => ui.label(name.as_str()),
                None => ui.label(egui::RichText::new(tl!("None chosen")).color(t.text_muted)),
            };
            if widgets::pill_button(ui, tl!("Choose picture…"), false).clicked() {
                clicked.choose = true;
            }
        });
        ui.end_row();
        let l = ui.label(tl!("Width:"));
        ui.add(egui::DragValue::new(&mut d.seal.width).range(10.0..=400.0).speed(0.5).suffix(" pt")).labelled_by(l.id);
        ui.end_row();
        ui.label(tl!("Position:"));
        ui.horizontal(|ui| {
            ui.label(tl!("Top"));
            let mut percent = d.seal.position * 100.0;
            if ui.add(egui::Slider::new(&mut percent, 0.0..=100.0).show_value(false)).changed() {
                d.seal.position = (percent / 100.0).clamp(0.0, 1.0);
            }
            ui.label(tl!("Bottom"));
        });
        ui.end_row();
        ui.label(tl!("Edge:"));
        ui.horizontal(|ui| {
            widgets::radio_value(ui, &mut d.seal.edge, SealEdge::Right, tl!("Right"));
            widgets::radio_value(ui, &mut d.seal.edge, SealEdge::Left, tl!("Left"));
        });
        ui.end_row();
        ui.label(tl!("Pages:"));
        ui.vertical(|ui| {
            widgets::radio_value(ui, &mut d.range, Range::All, tl!("All"));
            if !d.selection.is_empty() {
                widgets::radio_value(ui, &mut d.range, Range::Selection, tl!("Selected pages"));
            }
            let (mut a, mut b) = match d.range {
                Range::Pages(a, b) => (a, b),
                _ => (1, count),
            };
            ui.horizontal(|ui| {
                if ui.add(widgets::radio(matches!(d.range, Range::Pages(..)), tl!("From"))).clicked() {
                    d.range = Range::Pages(a, b);
                }
                let ra = ui.add(egui::DragValue::new(&mut a).range(1..=count));
                ui.label(tl!("to"));
                let rb = ui.add(egui::DragValue::new(&mut b).range(1..=count));
                if ra.changed() || rb.changed() {
                    d.range = Range::Pages(a.min(b), b.max(a));
                }
            });
        });
        ui.end_row();
    });
    ui.add_space(8.0);
    let pages = d.pages(count).len();
    if pages > MAX_PAGES_PER_SEAL {
        let n = MAX_PAGES_PER_SEAL.to_string();
        ui.label(
            egui::RichText::new(crate::i18n::fmt(tl!("More than {n} pages are split into groups, each with the whole seal."), &[("n", &n)]))
                .small()
                .color(t.text_muted),
        );
    }
    ui.add_space(12.0);
    let ready = d.picture.is_some() && pages > 0;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui.add_enabled_ui(ready, |ui| widgets::pill_button(ui, tl!("Apply"), true)).inner.clicked() {
            clicked.apply = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            clicked.cancel = true;
        }
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_draft_chooses_its_pages() {
        let mut d = SealDraft::default();
        assert_eq!(d.pages(3), [0, 1, 2]);
        d.range = Range::Pages(2, 9);
        assert_eq!(d.pages(3), [1, 2], "kept within the document");
        d.range = Range::Selection;
        d.selection = vec![2, 0, 7];
        assert_eq!(d.pages(3), [2, 0], "in the order selected, a page past the end left out");
    }
}
