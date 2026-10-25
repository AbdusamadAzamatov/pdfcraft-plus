//! Compress ▸ Clean up ▸ Rasterize pages (CU2): the dialog (pages, resolution, greyscale, what
//! happens to comments) and running it. Each chosen page becomes one picture of itself
//! ([`pdfcraft_engine::Session::rasterize_edit`]), applied as one undoable edit. The dialog warns
//! that the text stops being searchable, and points at Recognize text.

use egui::{Align, Layout};
use pdfcraft_engine::rasterize::{DEFAULT_DPI, MAX_DPI, MIN_DPI, RasterizeOptions};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Which pages to rasterize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasterPages {
    All,
    Current,
    /// The pages picked in the thumbnails (offered only when some are).
    Selected,
    Range,
}

/// The dialog's choices (kept between runs; the pages start from the pages picked).
#[derive(Clone, Debug, PartialEq)]
pub struct RasterizeDraft {
    pub pages: RasterPages,
    /// 1-based first and last page of the range.
    pub from: usize,
    pub to: usize,
    pub dpi: u32,
    pub grayscale: bool,
    /// Burn the comments into the pictures (otherwise they stay comments).
    pub burn_comments: bool,
}

impl Default for RasterizeDraft {
    fn default() -> Self {
        RasterizeDraft { pages: RasterPages::All, from: 1, to: 1, dpi: DEFAULT_DPI as u32, grayscale: false, burn_comments: false }
    }
}

impl RasterizeDraft {
    /// The 0-based pages chosen in a document of `count` pages, `current` being shown and
    /// `selected` picked.
    pub fn pages(&self, count: usize, current: usize, selected: &[usize]) -> Vec<usize> {
        let last = count.saturating_sub(1);
        match self.pages {
            RasterPages::All => (0..count).collect(),
            RasterPages::Current => vec![current.min(last)],
            RasterPages::Selected => selected.iter().copied().filter(|&p| p < count).collect(),
            RasterPages::Range => {
                let (a, b) = (self.from.min(self.to).max(1), self.from.max(self.to).min(count));
                (a.saturating_sub(1)..b).collect()
            }
        }
    }

    pub fn options(&self) -> RasterizeOptions {
        RasterizeOptions { dpi: f64::from(self.dpi).clamp(MIN_DPI, MAX_DPI), grayscale: self.grayscale, burn_comments: self.burn_comments }
    }
}

impl PdfCraftApp {
    /// Open the Rasterize pages dialog on the active document.
    pub fn open_rasterize(&mut self) {
        let Some((i, _)) = self.active_ids() else { return };
        let picked = !self.views[i].selected.is_empty();
        let d = &mut self.rasterize_draft;
        d.pages = match (picked, d.pages) {
            (true, _) => RasterPages::Selected,
            (false, RasterPages::Selected) => RasterPages::All,
            (false, other) => other,
        };
        self.dialog = Some(crate::Dialog::Rasterize);
    }

    /// Rasterize: draw the chosen pages as pictures and put them in place, as one undo step.
    pub(crate) fn rasterize_with_draft(&mut self) {
        let Some((i, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let view = &self.views[i];
        let selected: Vec<usize> = view.selected.iter().copied().collect();
        let pages = self.rasterize_draft.pages(doc.info.pages.len(), view.current, &selected);
        match self.session.rasterize_edit(id, &pages, self.rasterize_draft.options()) {
            Ok(edit) => {
                self.apply_edit(edit);
            }
            Err(e) => self.notify_fmt("Couldn't rasterize the pages: {e}", &[("e", &e.to_string())]),
        }
    }
}

/// Draw the dialog; returns (rasterize, cancel).
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> (bool, bool) {
    let Some((i, id)) = app.active_ids() else { return (false, true) };
    let count = app.session.get(id).map_or(1, |d| d.info.pages.len().max(1));
    let picked = !app.views[i].selected.is_empty();
    let d = &mut app.rasterize_draft;
    d.to = d.to.clamp(1, count);
    d.from = d.from.clamp(1, d.to);
    ui.label(egui::RichText::new(tl!("Rasterize pages…").trim_end_matches('…')).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    let group = |ui: &mut egui::Ui, title: &str, body: &mut dyn FnMut(&mut egui::Ui)| {
        ui.label(egui::RichText::new(tl!(title)).font(theme::semibold(13.0)));
        egui::Frame::new().fill(t.hover).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
        ui.add_space(8.0);
    };
    group(ui, "Pages", &mut |ui| {
        widgets::radio_value(ui, &mut d.pages, RasterPages::All, tl!("All pages"));
        widgets::radio_value(ui, &mut d.pages, RasterPages::Current, tl!("Current page"));
        if picked {
            widgets::radio_value(ui, &mut d.pages, RasterPages::Selected, tl!("Selected pages"));
        }
        ui.horizontal(|ui| {
            widgets::radio_value(ui, &mut d.pages, RasterPages::Range, tl!("From"));
            let on = d.pages == RasterPages::Range;
            ui.add_enabled(on, egui::DragValue::new(&mut d.from).range(1..=count));
            ui.label(tl!("to"));
            ui.add_enabled(on, egui::DragValue::new(&mut d.to).range(1..=count));
        });
    });
    group(ui, "Settings", &mut |ui| {
        ui.horizontal(|ui| {
            let l = ui.label(tl!("Resolution:"));
            ui.add(egui::DragValue::new(&mut d.dpi).range(MIN_DPI as u32..=MAX_DPI as u32).speed(1.0).suffix(" dpi")).labelled_by(l.id);
        });
        ui.add(widgets::checkbox(&mut d.grayscale, tl!("Greyscale")));
    });
    group(ui, "Comments", &mut |ui| {
        widgets::radio_value(ui, &mut d.burn_comments, false, tl!("Keep as comments"));
        widgets::radio_value(ui, &mut d.burn_comments, true, tl!("Burn into the image"));
    });
    ui.label(
        egui::RichText::new(tl!(
            "The text on these pages will no longer be searchable or copyable. Run Recognize text afterwards to make it searchable again."
        ))
        .small()
        .color(t.text_muted),
    );
    ui.add_space(12.0);
    let (mut go, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("Rasterize"), true).clicked() {
                go = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                cancel = true;
            }
        })
    });
    (go, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pages_follow_the_choice_and_stay_in_the_document() {
        let mut d = RasterizeDraft::default();
        assert_eq!(d.pages(3, 1, &[]), [0, 1, 2]);
        d.pages = RasterPages::Current;
        assert_eq!(d.pages(3, 7, &[]), [2], "a stale current page is clamped");
        d.pages = RasterPages::Selected;
        assert_eq!(d.pages(3, 0, &[0, 2, 9]), [0, 2]);
        d.pages = RasterPages::Range;
        (d.from, d.to) = (3, 2);
        assert_eq!(d.pages(3, 0, &[]), [1, 2], "a reversed range still counts");
        (d.from, d.to) = (2, 40);
        assert_eq!(d.pages(3, 0, &[]), [1, 2]);
        d.dpi = 5000;
        assert_eq!(d.options().dpi, 600.0);
        assert_eq!(RasterizeDraft::default().options(), RasterizeOptions::default(), "150 dpi, colour, comments kept");
    }
}
