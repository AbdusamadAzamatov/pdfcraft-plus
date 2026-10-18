//! Organize pages ▸ Layout ▸ Divide pages (execution plan PL4): split each page into columns ×
//! rows equal parts, read left to right or right to left, on all, the selected or a range of
//! pages, optionally leaving the first and last pages (covers) whole. A preview shows how the
//! first page divided is cut and the order its parts come in.

use egui::{Align, Align2, Color32, CornerRadius, Layout, Rect, Stroke, pos2, vec2};
use pdfcraft_engine::{Edit, MAX_DIVIDE};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    All,
    /// The pages selected when the dialog opened (or the current page).
    Selection,
    /// 1-based, inclusive.
    Pages(usize, usize),
}

/// Dialog state.
#[derive(Clone, Debug, PartialEq)]
pub struct DivideDraft {
    pub cols: u32,
    pub rows: u32,
    pub rtl: bool,
    pub range: Range,
    /// Leave the document's first and last pages whole (book covers).
    pub skip_covers: bool,
    /// The pages (0-based) selected when the dialog opened.
    pub selection: Vec<usize>,
}

impl Default for DivideDraft {
    fn default() -> Self {
        Self { cols: 2, rows: 1, rtl: false, range: Range::All, skip_covers: false, selection: Vec::new() }
    }
}

impl DivideDraft {
    /// The pages (0-based) to divide in a document of `count` pages.
    pub fn pages(&self, count: usize) -> Vec<usize> {
        let mut pages: Vec<usize> = match self.range {
            Range::All => (0..count).collect(),
            Range::Selection => self.selection.iter().copied().filter(|p| *p < count).collect(),
            Range::Pages(a, b) => (a.max(1) - 1..b.min(count)).collect(),
        };
        if self.skip_covers {
            pages.retain(|p| *p != 0 && p.saturating_add(1) != count);
        }
        pages
    }

    /// The edit to apply, or `None` when nothing would be divided.
    pub fn edit(&self, count: usize) -> Option<Edit> {
        let pages = self.pages(count);
        let grid = (1..=MAX_DIVIDE).contains(&self.cols) && (1..=MAX_DIVIDE).contains(&self.rows) && self.cols * self.rows > 1;
        (grid && !pages.is_empty()).then_some(Edit::DividePages { pages, cols: self.cols, rows: self.rows, rtl: self.rtl })
    }
}

/// Draw the dialog body; returns (apply, cancel).
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> (bool, bool) {
    let Some((_, id)) = app.active_ids() else { return (false, true) };
    let Some(doc) = app.session.get(id) else { return (false, true) };
    let sizes: Vec<(f32, f32)> = doc.info.pages.iter().map(|p| (p.width, p.height)).collect();
    let count = sizes.len().max(1);
    let d = &mut app.divide_draft;
    ui.label(egui::RichText::new(tl!("Divide pages")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            egui::Grid::new("divide-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                let l = ui.label(tl!("Columns"));
                ui.add(egui::DragValue::new(&mut d.cols).range(1..=MAX_DIVIDE)).labelled_by(l.id);
                ui.end_row();
                let l = ui.label(tl!("Rows"));
                ui.add(egui::DragValue::new(&mut d.rows).range(1..=MAX_DIVIDE)).labelled_by(l.id);
                ui.end_row();
                ui.label(tl!("Reading order"));
                ui.vertical(|ui| {
                    widgets::radio_value(ui, &mut d.rtl, false, tl!("Left to right"));
                    widgets::radio_value(ui, &mut d.rtl, true, tl!("Right to left"));
                });
                ui.end_row();
                ui.label(tl!("Pages"));
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
                        let on = matches!(d.range, Range::Pages(..));
                        if ui.add(widgets::radio(on, tl!("From"))).clicked() {
                            d.range = Range::Pages(a, b);
                        }
                        let ra = ui.add(egui::DragValue::new(&mut a).range(1..=count));
                        ui.label(tl!("to"));
                        let rb = ui.add(egui::DragValue::new(&mut b).range(1..=count));
                        if ra.changed() || rb.changed() {
                            d.range = Range::Pages(a.min(b), b.max(a));
                        }
                    });
                    ui.add(widgets::checkbox(&mut d.skip_covers, tl!("Skip first and last page")));
                });
                ui.end_row();
            });
        });
        ui.add_space(16.0);
        // Preview: the first page divided, as displayed, with its parts numbered in order.
        let first = d.pages(sizes.len()).first().copied();
        if let Some((w, h)) = first.and_then(|p| sizes.get(p)).copied() {
            let (w, h) = (w.max(1.0), h.max(1.0));
            let s = 180.0 / w.max(h);
            let (r, _) = ui.allocate_exact_size(vec2(200.0, 200.0), egui::Sense::hover());
            let page = Rect::from_center_size(r.center(), vec2(w * s, h * s));
            let p = ui.painter();
            p.rect_filled(page, CornerRadius::ZERO, Color32::WHITE);
            p.rect_stroke(page, CornerRadius::ZERO, Stroke::new(1.0, t.border), egui::StrokeKind::Outside);
            let (cols, rows) = (d.cols.clamp(1, MAX_DIVIDE), d.rows.clamp(1, MAX_DIVIDE));
            let (cw, ch) = (page.width() / cols as f32, page.height() / rows as f32);
            let line = Stroke::new(1.5, t.accent);
            for c in 1..cols {
                let x = page.left() + cw * c as f32;
                p.line_segment([pos2(x, page.top()), pos2(x, page.bottom())], line);
            }
            for row in 1..rows {
                let y = page.top() + ch * row as f32;
                p.line_segment([pos2(page.left(), y), pos2(page.right(), y)], line);
            }
            let mut n = 0u32;
            for row in 0..rows {
                for k in 0..cols {
                    n += 1;
                    let c = if d.rtl { cols - 1 - k } else { k };
                    let center = pos2(page.left() + cw * (c as f32 + 0.5), page.top() + ch * (row as f32 + 0.5));
                    p.text(center, Align2::CENTER_CENTER, n.to_string(), theme::semibold(12.0), t.text_faint);
                }
            }
            if let Some(first) = first {
                p.text(
                    page.center_bottom() + vec2(0.0, 6.0),
                    Align2::CENTER_TOP,
                    crate::i18n::fmt(tl!("Page {label}"), &[("label", &(first + 1).to_string())]),
                    theme::regular(11.0),
                    t.text_faint,
                );
            }
        }
    });
    ui.add_space(12.0);
    let ready = d.edit(sizes.len()).is_some();
    let (mut apply, mut cancel) = (false, false);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui.add_enabled_ui(ready, |ui| widgets::pill_button(ui, tl!("OK"), true)).inner.clicked() {
            apply = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            cancel = true;
        }
    });
    (apply, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_pick_pages_skip_covers_and_refuse_a_one_part_grid() {
        let mut d = DivideDraft::default();
        assert_eq!(d.pages(5), [0, 1, 2, 3, 4]);
        assert_eq!(d.edit(5), Some(Edit::DividePages { pages: vec![0, 1, 2, 3, 4], cols: 2, rows: 1, rtl: false }));
        d.skip_covers = true;
        assert_eq!(d.pages(5), [1, 2, 3]);
        d.range = Range::Pages(4, 9);
        assert_eq!(d.pages(5), [3]);
        d.range = Range::Selection;
        d.selection = vec![0, 4, 7];
        assert_eq!(d.pages(5), Vec::<usize>::new(), "covers skipped and a page past the end dropped");
        assert_eq!(d.edit(5), None);
        d.skip_covers = false;
        d.cols = 1;
        assert_eq!(d.edit(5), None, "1 × 1 changes nothing");
        d.rows = 3;
        d.rtl = true;
        assert_eq!(d.edit(5), Some(Edit::DividePages { pages: vec![0, 4], cols: 1, rows: 3, rtl: true }));
    }
}
