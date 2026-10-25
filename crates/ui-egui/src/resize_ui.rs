//! Organize ▸ Layout ▸ Change page size (execution plan PL7).
//!
//! The target size (a paper preset, the size of another page, or a custom width × height in the
//! unit chosen in Preferences), which way round it goes (portrait, landscape, or each page as it
//! is), how the content is placed (scale to fit, scale to fill and crop, or no scaling), and the
//! pages: all, a range, or only those whose size differs from the target.

use egui::{Align, Layout};
use pdfcraft_engine::{Edit, RESIZE_PAPERS, ResizeMode, SizeOrientation, paper_size};

use crate::theme::{self, Tokens};
use crate::units::Unit;
use crate::{PdfCraftApp, widgets};

/// What the pages are resized to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A [`RESIZE_PAPERS`] index.
    Paper(usize),
    /// The shown size of this page (1-based).
    SameAsPage(usize),
    /// [`ResizeDraft::width`] × [`ResizeDraft::height`].
    Custom,
}

/// Which pages change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    /// 1-based, inclusive.
    Range(usize, usize),
    /// Every page whose size differs from the target.
    Differing,
}

/// Dialog state.
pub struct ResizeDraft {
    pub target: Target,
    /// The custom size, in points.
    pub width: f64,
    pub height: f64,
    /// The unit the custom size is typed in.
    pub unit: Unit,
    pub orientation: SizeOrientation,
    pub mode: ResizeMode,
    pub scope: Scope,
    /// The pages of another size, for (document state, width and height bits, orientation).
    differing: Option<(DifferingKey, Vec<usize>)>,
}

type DifferingKey = (usize, u64, u64, SizeOrientation);

impl Default for ResizeDraft {
    fn default() -> Self {
        let (width, height) = paper_size("A4").unwrap_or((595.28, 841.89));
        Self {
            target: Target::Paper(1),
            width,
            height,
            unit: Unit::Millimetres,
            orientation: SizeOrientation::Match,
            mode: ResizeMode::Fit,
            scope: Scope::All,
            differing: None,
        }
    }
}

/// What the dialog asks for when it closes.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    Open,
    Cancel,
    Apply(Box<Edit>),
}

impl ResizeDraft {
    /// Seed for a fresh dialog: the custom size is typed in the Preferences unit.
    pub(crate) fn open(&mut self, unit: Unit) {
        self.unit = unit;
    }

    /// The target size in points, given each page's shown size (`sizes`, 0-based). `None` when
    /// "same as page" names a page that doesn't exist.
    pub fn size(&self, sizes: &[(f32, f32)]) -> Option<(f64, f64)> {
        match self.target {
            Target::Paper(i) => RESIZE_PAPERS.get(i).and_then(|name| paper_size(name)),
            Target::SameAsPage(p) => p.checked_sub(1).and_then(|i| sizes.get(i)).map(|(w, h)| (f64::from(*w), f64::from(*h))),
            Target::Custom => Some((self.width, self.height)),
        }
    }

    /// The pages (0-based) the scope names before "only the pages that differ" is applied.
    pub fn pages(&self, count: usize) -> Vec<usize> {
        match self.scope {
            Scope::All | Scope::Differing => (0..count).collect(),
            Scope::Range(a, b) => (a.max(1) - 1..b.min(count)).collect(),
        }
    }

    pub fn edit(&self, pages: Vec<usize>, (width, height): (f64, f64)) -> Edit {
        Edit::ResizePages { pages, width, height, orientation: self.orientation, mode: self.mode }
    }
}

/// Draw the dialog body.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> Outcome {
    let Some((_, id)) = app.active_ids() else { return Outcome::Cancel };
    let Some(doc) = app.session.get(id) else { return Outcome::Cancel };
    let sizes: Vec<(f32, f32)> = doc.info.pages.iter().map(|p| (p.width, p.height)).collect();
    let count = sizes.len().max(1);
    let state = std::sync::Arc::as_ptr(&doc.display) as usize;
    let d = &mut app.resize_draft;

    ui.label(egui::RichText::new(tl!("Change page size")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    egui::Grid::new("resize-pages").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        let l = ui.label(tl!("Page size:"));
        ui.vertical(|ui| {
            let shown = match d.target {
                Target::Paper(i) => tl!(RESIZE_PAPERS.get(i).copied().unwrap_or("A4")).to_string(),
                Target::SameAsPage(_) => tl!("Same as page").to_string(),
                Target::Custom => tl!("Custom").to_string(),
            };
            egui::ComboBox::from_id_salt("resize-target")
                .selected_text(shown)
                .height(400.0)
                .show_ui(ui, |ui| {
                    for (i, name) in RESIZE_PAPERS.iter().enumerate() {
                        ui.selectable_value(&mut d.target, Target::Paper(i), tl!(name));
                    }
                    let same = if let Target::SameAsPage(p) = d.target { p } else { 1 };
                    ui.selectable_value(&mut d.target, Target::SameAsPage(same), tl!("Same as page"));
                    ui.selectable_value(&mut d.target, Target::Custom, tl!("Custom"));
                })
                .response
                .labelled_by(l.id);
            match d.target {
                Target::SameAsPage(mut p) => {
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::DragValue::new(&mut p).range(1..=count));
                        ui.label(crate::i18n::fmt(tl!("of {name}"), &[("name", &count.to_string())]));
                        if r.changed() {
                            d.target = Target::SameAsPage(p);
                        }
                    });
                }
                Target::Custom => {
                    ui.horizontal(|ui| {
                        let k = d.unit.per_point();
                        let max = pdfcraft_engine::MAX_PAGE_SIDE * k;
                        for (label, v) in [(tl!("Width:"), &mut d.width), (tl!("Height:"), &mut d.height)] {
                            let l = ui.label(label);
                            let mut shown = *v * k;
                            let r = ui
                                .add(egui::DragValue::new(&mut shown).speed(d.unit.speed()).range(k..=max).fixed_decimals(d.unit.decimals()))
                                .labelled_by(l.id);
                            if r.changed() {
                                *v = (shown / k).clamp(1.0, pdfcraft_engine::MAX_PAGE_SIDE);
                            }
                        }
                        egui::ComboBox::from_id_salt("resize-unit").width(56.0).selected_text(d.unit.label()).show_ui(ui, |ui| {
                            for u in Unit::ALL {
                                ui.selectable_value(&mut d.unit, u, u.label());
                            }
                        });
                    });
                }
                Target::Paper(_) => {}
            }
        });
        ui.end_row();
        ui.label(tl!("Orientation:"));
        ui.vertical(|ui| {
            widgets::radio_value(ui, &mut d.orientation, SizeOrientation::Portrait, tl!("Portrait"));
            widgets::radio_value(ui, &mut d.orientation, SizeOrientation::Landscape, tl!("Landscape"));
            widgets::radio_value(ui, &mut d.orientation, SizeOrientation::Match, tl!("Match each page"));
        });
        ui.end_row();
        ui.label(tl!("Fit:"));
        ui.vertical(|ui| {
            widgets::radio_value(ui, &mut d.mode, ResizeMode::Fit, tl!("Scale to fit"));
            widgets::radio_value(ui, &mut d.mode, ResizeMode::Fill, tl!("Scale to fill, crop"));
            widgets::radio_value(ui, &mut d.mode, ResizeMode::NoScale, tl!("No scaling"));
        });
        ui.end_row();
        ui.label(tl!("Pages:"));
        ui.vertical(|ui| {
            widgets::radio_value(ui, &mut d.scope, Scope::All, tl!("All"));
            let (mut a, mut b) = match d.scope {
                Scope::Range(a, b) => (a, b),
                _ => (1, count),
            };
            ui.horizontal(|ui| {
                if ui.add(widgets::radio(matches!(d.scope, Scope::Range(..)), tl!("From"))).clicked() {
                    d.scope = Scope::Range(a, b);
                }
                let ra = ui.add(egui::DragValue::new(&mut a).range(1..=count));
                ui.label(tl!("to"));
                let rb = ui.add(egui::DragValue::new(&mut b).range(1..=count));
                ui.label(crate::i18n::fmt(tl!("of {name}"), &[("name", &count.to_string())]));
                if ra.changed() || rb.changed() {
                    d.scope = Scope::Range(a.min(b), b.max(a));
                }
            });
            widgets::radio_value(ui, &mut d.scope, Scope::Differing, tl!("Only pages of another size"));
        });
        ui.end_row();
    });

    // The pages that will change: for "only pages of another size", those that differ.
    let size = d.size(&sizes);
    let mut pages = d.pages(sizes.len());
    if d.scope == Scope::Differing
        && let Some((w, h)) = size
    {
        // Read once per document state and target, not every frame.
        let key = (state, w.to_bits(), h.to_bits(), d.orientation);
        if d.differing.as_ref().is_none_or(|(k, _)| *k != key) {
            d.differing = Some((key, doc.pages_of_other_size(&pages, w, h, d.orientation).unwrap_or_default()));
        }
        pages = d.differing.as_ref().map(|(_, p)| p.clone()).unwrap_or_default();
    }
    let ready = size.is_some() && !pages.is_empty();
    if size.is_some() && pages.is_empty() && d.scope == Scope::Differing {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(tl!("Every page already has this size.")).color(t.text_muted));
    }
    ui.add_space(12.0);
    let mut out = Outcome::Open;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let ok = ui.add_enabled_ui(ready, |ui| widgets::pill_button(ui, tl!("OK"), true)).inner;
        if ok.clicked()
            && ready
            && let Some(size) = size
        {
            out = Outcome::Apply(Box::new(d.edit(pages.clone(), size)));
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            out = Outcome::Cancel;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_pick_the_target_and_pages() {
        let mut d = ResizeDraft::default();
        let sizes = [(612.0, 792.0), (200.0, 300.0)];
        assert_eq!(d.size(&sizes), Some((595.28, 841.89)), "A4 by default");
        d.target = Target::Paper(3);
        assert_eq!(d.size(&sizes), Some((612.0, 792.0)), "US Letter");
        d.target = Target::SameAsPage(2);
        assert_eq!(d.size(&sizes), Some((200.0, 300.0)));
        d.target = Target::SameAsPage(3);
        assert_eq!(d.size(&sizes), None, "no page 3");
        d.target = Target::Custom;
        (d.width, d.height) = (100.0, 50.0);
        assert_eq!(d.size(&sizes), Some((100.0, 50.0)));
        assert_eq!(d.pages(3), [0, 1, 2]);
        d.scope = Scope::Range(2, 9);
        assert_eq!(d.pages(3), [1, 2]);
        d.scope = Scope::Differing;
        assert_eq!(d.pages(2), [0, 1]);
        d.mode = ResizeMode::Fill;
        assert_eq!(
            d.edit(vec![1], (100.0, 50.0)),
            Edit::ResizePages { pages: vec![1], width: 100.0, height: 50.0, orientation: SizeOrientation::Match, mode: ResizeMode::Fill }
        );
        assert!(RESIZE_PAPERS.len() > 3, "the presets the combo box lists");
    }
}
