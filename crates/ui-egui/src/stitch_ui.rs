//! Organize ▸ Layout ▸ Stitch pages (execution plan PL6): the chosen pages one after another on a
//! single long page, top to bottom or left to right, with a gap, a white or no background and
//! the narrower pages aligned across the strip. Create opens the result as a new, unsaved
//! document ([`pdfcraft_engine::Session::stitch_new`]); the document itself is not changed.

use egui::{Align, Layout};
use pdfcraft_engine::{MAX_STITCH_GAP, StitchAlign, StitchDirection, StitchOptions, stitch_layout};

use crate::divide_ui::Range;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Dialog state.
#[derive(Clone, Debug, PartialEq)]
pub struct StitchDraft {
    pub options: StitchOptions,
    pub range: Range,
    /// The pages (0-based) selected when the dialog opened.
    pub selection: Vec<usize>,
}

impl Default for StitchDraft {
    fn default() -> Self {
        Self { options: StitchOptions::default(), range: Range::All, selection: Vec::new() }
    }
}

impl StitchDraft {
    /// The pages (0-based, in order) to stitch from a document of `count` pages.
    pub fn pages(&self, count: usize) -> Vec<usize> {
        match self.range {
            Range::All => (0..count).collect(),
            Range::Selection => self.selection.iter().copied().filter(|p| *p < count).collect(),
            Range::Pages(a, b) => (a.max(1) - 1..b.min(count)).collect(),
        }
    }
}

impl PdfCraftApp {
    /// Open Stitch pages on the active document: the selected pages when there are any, else
    /// every page. The other choices stay from last time.
    pub fn open_stitch(&mut self) {
        let Some((i, _)) = self.active_ids() else { return };
        let selection: Vec<usize> = self.views.get(i).map(|v| v.selected.iter().copied().collect()).unwrap_or_default();
        self.stitch_draft.range = if selection.is_empty() { Range::All } else { Range::Selection };
        self.stitch_draft.selection = selection;
        self.dialog = Some(crate::Dialog::Stitch);
    }

    /// Create: stitch the pages and open the result as a new document.
    pub(crate) fn stitch_with_draft(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let pages = self.stitch_draft.pages(doc.info.pages.len());
        let name = pdfcraft_engine::stitch_name(&doc.name);
        match self.session.stitch_new(id, &pages, &self.stitch_draft.options) {
            Ok((bytes, _)) => {
                let message = crate::i18n::fmt(tl!("Created {name}"), &[("name", &name)]);
                self.open_created(&name, bytes, &message);
            }
            Err(e) => self.notify_fmt("Couldn't create a PDF: {e}", &[("e", &e.to_string())]),
        }
    }
}

/// Draw the dialog; returns (create, cancel).
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> (bool, bool) {
    let Some((_, id)) = app.active_ids() else { return (false, true) };
    let Some(doc) = app.session.get(id) else { return (false, true) };
    let sizes: Vec<(f64, f64)> = doc.info.pages.iter().map(|p| (f64::from(p.width), f64::from(p.height))).collect();
    let count = sizes.len().max(1);
    let d = &mut app.stitch_draft;
    ui.label(egui::RichText::new(tl!("Stitch pages…").trim_end_matches('…')).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    egui::Grid::new("stitch-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        let o = &mut d.options;
        ui.label(tl!("Direction:"));
        ui.horizontal(|ui| {
            widgets::radio_value(ui, &mut o.direction, StitchDirection::Vertical, tl!("Vertical"));
            widgets::radio_value(ui, &mut o.direction, StitchDirection::Horizontal, tl!("Horizontal"));
        });
        ui.end_row();
        let l = ui.label(tl!("Gap:"));
        ui.add(egui::DragValue::new(&mut o.gap).range(0.0..=MAX_STITCH_GAP).speed(0.5).suffix(" pt")).labelled_by(l.id);
        ui.end_row();
        ui.label(tl!("Background:"));
        ui.horizontal(|ui| {
            widgets::radio_value(ui, &mut o.white, true, tl!("White"));
            widgets::radio_value(ui, &mut o.white, false, tl!("None"));
        });
        ui.end_row();
        // Across the strip: left to right when the pages run down, top to bottom when across.
        ui.label(tl!("Alignment:"));
        let (start, end) = match o.direction {
            StitchDirection::Vertical => (tl!("Left"), tl!("Right")),
            StitchDirection::Horizontal => (tl!("Top"), tl!("Bottom")),
        };
        ui.horizontal(|ui| {
            widgets::radio_value(ui, &mut o.align, StitchAlign::Start, start);
            widgets::radio_value(ui, &mut o.align, StitchAlign::Centre, tl!("Centre"));
            widgets::radio_value(ui, &mut o.align, StitchAlign::End, end);
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
    let chosen: Vec<(f64, f64)> = d.pages(sizes.len()).iter().filter_map(|p| sizes.get(*p).copied()).collect();
    let laid_out = stitch_layout(&chosen, &d.options);
    match &laid_out {
        Ok(_) => {
            ui.label(egui::RichText::new(tl!("Creates a new document; this one is not changed.")).small().color(t.text_muted));
            ui.label(egui::RichText::new(tl!("Links and comments are not carried over.")).small().color(t.text_muted));
        }
        Err(e) => {
            ui.label(egui::RichText::new(tl!(&e.to_string())).small().color(t.text_muted));
        }
    }
    ui.add_space(12.0);
    let (mut create, mut cancel) = (false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui.add_enabled_ui(laid_out.is_ok(), |ui| widgets::pill_button(ui, tl!("Create"), true)).inner.clicked() {
            create = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            cancel = true;
        }
    });
    (create, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_pick_all_the_selected_or_a_range_of_pages_in_order() {
        let mut d = StitchDraft::default();
        assert_eq!(d.pages(3), [0, 1, 2]);
        d.range = Range::Selection;
        d.selection = vec![2, 0, 9];
        assert_eq!(d.pages(3), [2, 0], "a page past the end is dropped");
        d.range = Range::Pages(2, 9);
        assert_eq!(d.pages(3), [1, 2]);
        assert_eq!(StitchDraft::default().options, StitchOptions::default());
    }
}
