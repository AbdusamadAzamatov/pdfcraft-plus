//! Organize ▸ Layout ▸ Collage: the chosen pages arranged on sheets of paper, a grid of them per
//! sheet, at their real size or fitted to the cells; for instance an identity card's front and
//! back on one A4 sheet. Create opens the result as a new, unsaved document
//! ([`pdfcraft_engine::Session::collage_new`]); the document itself is not changed.

use egui::{Align, Layout};
use pdfcraft_engine::{COLLAGE_A4, COLLAGE_LETTER, CollageOptions, MAX_COLLAGE_GAP, MAX_COLLAGE_GRID, MAX_COLLAGE_MARGIN, collage_layout};

use crate::divide_ui::Range;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Dialog state.
#[derive(Clone, Debug, PartialEq)]
pub struct CollageDraft {
    pub cols: usize,
    pub rows: usize,
    pub actual_size: bool,
    pub letter: bool,
    pub landscape: bool,
    pub margin: f64,
    pub gap: f64,
    pub range: Range,
    /// The pages (0-based) selected when the dialog opened.
    pub selection: Vec<usize>,
}

impl Default for CollageDraft {
    fn default() -> Self {
        let o = CollageOptions::id_card();
        Self {
            cols: o.cols,
            rows: o.rows,
            actual_size: o.actual_size,
            letter: false,
            landscape: false,
            margin: o.margin,
            gap: o.gap,
            range: Range::All,
            selection: Vec::new(),
        }
    }
}

impl CollageDraft {
    /// The pages (0-based, in order) to arrange from a document of `count` pages.
    pub fn pages(&self, count: usize) -> Vec<usize> {
        match self.range {
            Range::All => (0..count).collect(),
            Range::Selection => self.selection.iter().copied().filter(|p| *p < count).collect(),
            Range::Pages(a, b) => (a.max(1) - 1..b.min(count)).collect(),
        }
    }

    pub fn options(&self) -> CollageOptions {
        let paper = if self.letter { COLLAGE_LETTER } else { COLLAGE_A4 };
        CollageOptions {
            cols: self.cols,
            rows: self.rows,
            sheet: if self.landscape { (paper.1, paper.0) } else { paper },
            actual_size: self.actual_size,
            margin: self.margin,
            gap: self.gap,
        }
    }

    /// Whether this is the identity-card layout: front and back, one above the other, at real size.
    fn is_id_card(&self) -> bool {
        (self.cols, self.rows, self.actual_size) == (1, 2, true)
    }
}

impl PdfCraftApp {
    /// Open Collage on the active document: the selected pages when there are any, else every
    /// page. The other choices stay from last time.
    pub fn open_collage(&mut self) {
        let Some((i, _)) = self.active_ids() else { return };
        let selection: Vec<usize> = self.views.get(i).map(|v| v.selected.iter().copied().collect()).unwrap_or_default();
        self.collage_draft.range = if selection.is_empty() { Range::All } else { Range::Selection };
        self.collage_draft.selection = selection;
        self.dialog = Some(crate::Dialog::Collage);
    }

    /// Create: arrange the pages and open the result as a new document.
    pub(crate) fn collage_with_draft(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let pages = self.collage_draft.pages(doc.info.pages.len());
        let name = pdfcraft_engine::collage_name(&doc.name);
        match self.session.collage_new(id, &pages, &self.collage_draft.options()) {
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
    let d = &mut app.collage_draft;
    ui.label(egui::RichText::new(tl!("Collage…").trim_end_matches('…')).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    egui::Grid::new("collage-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(tl!("Layout:"));
        ui.horizontal(|ui| {
            if ui.add(widgets::radio(d.is_id_card(), tl!("ID card (front and back)"))).clicked() {
                (d.cols, d.rows, d.actual_size) = (1, 2, true);
            }
            if ui.add(widgets::radio(!d.is_id_card(), tl!("Grid"))).clicked() && d.is_id_card() {
                (d.cols, d.rows, d.actual_size) = (2, 2, false);
            }
        });
        ui.end_row();
        let l = ui.label(tl!("Columns:"));
        ui.add(egui::DragValue::new(&mut d.cols).range(1..=MAX_COLLAGE_GRID)).labelled_by(l.id);
        ui.end_row();
        let l = ui.label(tl!("Rows:"));
        ui.add(egui::DragValue::new(&mut d.rows).range(1..=MAX_COLLAGE_GRID)).labelled_by(l.id);
        ui.end_row();
        ui.label(tl!("Size:"));
        ui.horizontal(|ui| {
            widgets::radio_value(ui, &mut d.actual_size, true, tl!("Actual size"));
            widgets::radio_value(ui, &mut d.actual_size, false, tl!("Fit to cells"));
        });
        ui.end_row();
        ui.label(tl!("Paper:"));
        ui.horizontal(|ui| {
            widgets::radio_value(ui, &mut d.letter, false, "A4");
            widgets::radio_value(ui, &mut d.letter, true, tl!("Letter"));
            ui.separator();
            widgets::radio_value(ui, &mut d.landscape, false, tl!("Portrait"));
            widgets::radio_value(ui, &mut d.landscape, true, tl!("Landscape"));
        });
        ui.end_row();
        let l = ui.label(tl!("Margin:"));
        ui.add(egui::DragValue::new(&mut d.margin).range(0.0..=MAX_COLLAGE_MARGIN).speed(0.5).suffix(" pt")).labelled_by(l.id);
        ui.end_row();
        let l = ui.label(tl!("Gap:"));
        ui.add(egui::DragValue::new(&mut d.gap).range(0.0..=MAX_COLLAGE_GAP).speed(0.5).suffix(" pt")).labelled_by(l.id);
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
    let laid_out = collage_layout(&chosen, &d.options());
    match &laid_out {
        Ok(sheets) => {
            let n = sheets.len().to_string();
            ui.label(egui::RichText::new(crate::i18n::fmt(tl!("Sheets: {n}"), &[("n", &n)])).small().color(t.text_muted));
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
    fn the_draft_turns_into_options_and_pages() {
        let mut d = CollageDraft::default();
        assert!(d.is_id_card());
        assert_eq!(d.options(), CollageOptions::id_card());
        (d.letter, d.landscape, d.cols, d.rows, d.actual_size) = (true, true, 3, 2, false);
        let o = d.options();
        assert_eq!((o.sheet, o.cols, o.rows, o.actual_size), ((792.0, 612.0), 3, 2, false));
        assert!(!d.is_id_card());
        d.range = Range::Pages(2, 3);
        assert_eq!(d.pages(5), [1, 2]);
        d.range = Range::Selection;
        d.selection = vec![4, 9];
        assert_eq!(d.pages(5), [4], "a selected page past the end is left out");
    }
}
