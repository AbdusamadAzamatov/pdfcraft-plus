//! Organize ▸ Layout ▸ Trim white margins (PL8): the dialog (pages, padding) and running it, plus
//! the Remove white margins button of Set Page Boxes. Each chosen page is cropped to what is drawn
//! on it ([`pdfcraft_engine::trim`]), all in one undo step; pages with nothing to remove are left
//! alone and named in the message that follows.

use egui::{Align, Layout};
use pdfcraft_engine::TrimOptions;
use pdfcraft_engine::trim::MAX_PADDING;

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Which pages to trim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrimPages {
    All,
    Current,
    /// The pages picked in the thumbnails (offered only when some are).
    Selected,
    Range,
}

/// The dialog's choices (kept between runs).
#[derive(Clone, Debug, PartialEq)]
pub struct TrimDraft {
    pub pages: TrimPages,
    /// 1-based first and last page of the range.
    pub from: usize,
    pub to: usize,
    /// White space kept around the ink, in points (0 to [`MAX_PADDING`]).
    pub padding: f64,
}

impl Default for TrimDraft {
    fn default() -> Self {
        TrimDraft { pages: TrimPages::All, from: 1, to: 1, padding: 0.0 }
    }
}

impl TrimDraft {
    /// The 0-based pages chosen in a document of `count` pages, `current` being shown and
    /// `selected` picked.
    pub fn pages(&self, count: usize, current: usize, selected: &[usize]) -> Vec<usize> {
        let last = count.saturating_sub(1);
        match self.pages {
            TrimPages::All => (0..count).collect(),
            TrimPages::Current => vec![current.min(last)],
            TrimPages::Selected => selected.iter().copied().filter(|&p| p < count).collect(),
            TrimPages::Range => {
                let (a, b) = (self.from.min(self.to).max(1), self.from.max(self.to).min(count));
                (a.saturating_sub(1)..b).collect()
            }
        }
    }

    /// The padding, kept within what the engine accepts.
    pub fn padding(&self) -> f64 {
        if self.padding.is_finite() { self.padding.clamp(0.0, MAX_PADDING) } else { 0.0 }
    }
}

impl PdfCraftApp {
    /// Open Trim white margins on the active document.
    pub fn open_trim_margins(&mut self) {
        let Some((i, _)) = self.active_ids() else { return };
        let picked = !self.views[i].selected.is_empty();
        let d = &mut self.trim_draft;
        d.pages = match (picked, d.pages) {
            (true, _) => TrimPages::Selected,
            (false, TrimPages::Selected) => TrimPages::All,
            (false, other) => other,
        };
        self.dialog = Some(crate::Dialog::TrimMargins);
    }

    /// Trim: crop the chosen pages to their ink, as one undo step, and say what was left alone.
    pub(crate) fn trim_with_draft(&mut self) {
        let Some((i, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let view = &self.views[i];
        let selected: Vec<usize> = view.selected.iter().copied().collect();
        let pages = self.trim_draft.pages(doc.info.pages.len(), view.current, &selected);
        if pages.is_empty() {
            return;
        }
        let plan = match self.session.plan_trim_margins(id, &TrimOptions { padding: self.trim_draft.padding(), pages }) {
            Ok(plan) => plan,
            Err(e) => return self.notify_error(e),
        };
        let Some(edit) = plan.edit() else {
            return self.notify_tr("No white margins to remove");
        };
        if !self.apply_edit(edit) {
            return;
        }
        let n = plan.crops.len().to_string();
        if plan.left.is_empty() {
            self.notify_fmt("Pages trimmed: {n}", &[("n", &n)]);
        } else {
            let left: Vec<String> = plan.left.iter().map(|(p, _)| (p + 1).to_string()).collect();
            self.notify_fmt("Pages trimmed: {n}. Left alone: {pages}", &[("n", &n), ("pages", &left.join(", "))]);
        }
    }

    /// Set Page Boxes ▸ Remove white margins: set the dialog's margins to crop page `page`
    /// (0-based) to its ink, as Acrobat's button does; OK then applies them to the chosen pages.
    pub(crate) fn boxes_remove_white_margins(&mut self, page: usize) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(media) = self.session.get(id).and_then(|d| d.page_boxes().get(page).map(|b| b[0])) else { return };
        let plan = match self.session.plan_trim_margins(id, &TrimOptions { padding: 0.0, pages: vec![page] }) {
            Ok(plan) => plan,
            Err(e) => return self.notify_error(e),
        };
        let Some(&(_, c)) = plan.crops.first() else {
            return self.notify_tr("No white margins to remove");
        };
        self.boxes_draft.margins = [c[0] - media[0], c[1] - media[1], media[2] - c[2], media[3] - c[3]].map(|v| (v.max(0.0) * 100.0).floor() / 100.0);
    }
}

/// Draw the dialog; returns (trim, cancel).
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> (bool, bool) {
    let Some((i, id)) = app.active_ids() else { return (false, true) };
    let count = app.session.get(id).map_or(1, |d| d.info.pages.len().max(1));
    let picked = !app.views[i].selected.is_empty();
    let d = &mut app.trim_draft;
    d.to = d.to.clamp(1, count);
    d.from = d.from.clamp(1, d.to);
    ui.label(egui::RichText::new(tl!("Trim white margins…").trim_end_matches('…')).font(theme::semibold(18.0)));
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
        widgets::radio_value(ui, &mut d.pages, TrimPages::All, tl!("All pages"));
        widgets::radio_value(ui, &mut d.pages, TrimPages::Current, tl!("Current page"));
        if picked {
            widgets::radio_value(ui, &mut d.pages, TrimPages::Selected, tl!("Selected pages"));
        }
        ui.horizontal(|ui| {
            widgets::radio_value(ui, &mut d.pages, TrimPages::Range, tl!("From"));
            let on = d.pages == TrimPages::Range;
            ui.add_enabled(on, egui::DragValue::new(&mut d.from).range(1..=count));
            ui.label(tl!("to"));
            ui.add_enabled(on, egui::DragValue::new(&mut d.to).range(1..=count));
        });
    });
    ui.horizontal(|ui| {
        let l = ui.label(tl!("Padding"));
        ui.add(egui::DragValue::new(&mut d.padding).range(0.0..=MAX_PADDING).speed(0.5).max_decimals(1).suffix(" pt")).labelled_by(l.id);
    });
    ui.add_space(12.0);
    let (mut go, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("OK"), true).clicked() {
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
    fn the_pages_follow_the_choice_and_the_padding_stays_in_range() {
        let mut d = TrimDraft::default();
        assert_eq!(d.pages(3, 1, &[]), [0, 1, 2]);
        d.pages = TrimPages::Current;
        assert_eq!(d.pages(3, 7, &[]), [2], "a stale current page is clamped");
        d.pages = TrimPages::Selected;
        assert_eq!(d.pages(3, 0, &[0, 2, 9]), [0, 2]);
        d.pages = TrimPages::Range;
        (d.from, d.to) = (3, 2);
        assert_eq!(d.pages(3, 0, &[]), [1, 2], "a reversed range still counts");
        for (given, used) in [(-4.0, 0.0), (12.5, 12.5), (99.0, MAX_PADDING), (f64::NAN, 0.0)] {
            d.padding = given;
            assert_eq!(d.padding(), used);
        }
    }
}
