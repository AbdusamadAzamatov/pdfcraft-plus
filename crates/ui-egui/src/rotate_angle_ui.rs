//! Organize ▸ Layout ▸ Rotate by angle (execution plan PL2).
//!
//! An angle from −180° to 180° in 0.1° steps, how the turned page fits (expand the page, keep
//! its size and scale the content down, or keep its size and crop), and the pages. The preview
//! turns a still render of the current page live, so dragging the angle never renders again.
//! When a page to turn has comments or form fields, which can't turn by such an angle, the
//! dialog says so and offers Flatten comments and rotate (one undo step) instead of OK.

use egui::{Align, Color32, CornerRadius, Layout, Pos2, Rect, Stroke, pos2, vec2};
use pdfcraft_engine::{DocId, Edit, RotateFit};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Which pages turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    /// The selected pages (or the current page when none is selected).
    Selection,
    /// 1-based, inclusive.
    Range(usize, usize),
}

/// The rendered page the preview turns: which document state and page it shows.
struct Preview {
    key: (DocId, usize, usize),
    texture: egui::TextureHandle,
}

/// Which pages have comments or form fields, for (document state, pages to turn).
struct Drawn {
    key: (usize, Vec<usize>),
    pages: Vec<usize>,
}

/// Dialog state.
pub struct RotateAngleDraft {
    pub degrees: f64,
    pub fit: RotateFit,
    pub scope: Scope,
    preview: Option<Preview>,
    drawn: Option<Drawn>,
}

impl Default for RotateAngleDraft {
    fn default() -> Self {
        Self { degrees: 0.0, fit: RotateFit::Expand, scope: Scope::All, preview: None, drawn: None }
    }
}

/// What the dialog asks for when it closes.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    Open,
    Cancel,
    Apply(Box<Edit>),
}

/// A whole multiple of 90° only changes `/Rotate`: comments turn with the page.
fn quarter_turn(degrees: f64) -> bool {
    let q = (degrees / 90.0).round();
    (degrees - q * 90.0).abs() < 1e-9
}

impl RotateAngleDraft {
    /// Seed for a fresh dialog: the selection when there is one, else every page.
    pub(crate) fn open(&mut self, has_selection: bool) {
        self.scope = if has_selection { Scope::Selection } else { Scope::All };
        self.drawn = None;
    }

    /// The pages (0-based) the dialog turns. `selection` is the view's target pages.
    pub fn pages(&self, selection: &[usize], count: usize) -> Vec<usize> {
        match self.scope {
            Scope::All => (0..count).collect(),
            Scope::Selection => selection.iter().copied().filter(|p| *p < count).collect(),
            Scope::Range(a, b) => (a.max(1) - 1..b.min(count)).collect(),
        }
    }

    /// The edit: with `flatten`, comments and form fields are flattened first, in the same step.
    pub fn edit(&self, pages: Vec<usize>, flatten: bool) -> Edit {
        let degrees = (self.degrees.clamp(-180.0, 180.0) * 10.0).round() / 10.0;
        if flatten { Edit::flatten_and_rotate(pages, degrees, self.fit) } else { Edit::RotateContent { pages, degrees, fit: self.fit } }
    }
}

/// The shown size of the frame and the scale of the turned page in it, in page units, for a page
/// shown `w`×`h` turned by `degrees` with `fit` (a quarter turn always grows like Expand).
fn layout(w: f32, h: f32, degrees: f64, fit: RotateFit) -> (f32, f32, f32) {
    let (s, c) = (degrees as f32).to_radians().sin_cos();
    let (tw, th) = (w * c.abs() + h * s.abs(), w * s.abs() + h * c.abs());
    match fit {
        _ if quarter_turn(degrees) => (tw, th, 1.0),
        RotateFit::Expand => (tw, th, 1.0),
        RotateFit::KeepSize => (w, h, (w / tw.max(1e-3)).min(h / th.max(1e-3)).min(1.0)),
        RotateFit::Crop => (w, h, 1.0),
    }
}

/// Draw the preview: the result page as a white frame and the rendered page turned and scaled
/// inside it, clipped to the frame.
fn preview(ui: &mut egui::Ui, t: &Tokens, texture: Option<&egui::TextureHandle>, size: (f32, f32), degrees: f64, fit: RotateFit) {
    let (area, _) = ui.allocate_exact_size(vec2(220.0, 220.0), egui::Sense::hover());
    let (w, h) = (size.0.max(1.0), size.1.max(1.0));
    let (fw, fh, scale) = layout(w, h, degrees, fit);
    let k = 200.0 / fw.max(fh).max(1.0);
    let frame = Rect::from_center_size(area.center(), vec2(fw * k, fh * k));
    let painter = ui.painter().with_clip_rect(frame.intersect(area));
    ui.painter().rect_filled(frame, CornerRadius::ZERO, Color32::WHITE);
    // Clockwise on screen (y down): x' = x·cos − y·sin, y' = x·sin + y·cos.
    let (s, c) = (degrees as f32).to_radians().sin_cos();
    let (hw, hh) = (w * scale * k / 2.0, h * scale * k / 2.0);
    let centre = frame.center();
    let corner = |x: f32, y: f32| -> Pos2 { pos2(centre.x + x * c - y * s, centre.y + x * s + y * c) };
    let quad = [corner(-hw, -hh), corner(hw, -hh), corner(hw, hh), corner(-hw, hh)];
    match texture {
        Some(tex) => {
            let mut mesh = egui::Mesh::with_texture(tex.id());
            let uv = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)];
            for (p, uv) in quad.iter().zip(uv) {
                mesh.vertices.push(egui::epaint::Vertex { pos: *p, uv, color: Color32::WHITE });
            }
            mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
            painter.add(egui::Shape::mesh(mesh));
        }
        None => {
            painter.add(egui::Shape::convex_polygon(quad.to_vec(), t.field, Stroke::new(1.0, t.text_faint)));
        }
    }
    painter.add(egui::Shape::closed_line(quad.to_vec(), Stroke::new(1.0, t.text_faint)));
    ui.painter().rect_stroke(frame, CornerRadius::ZERO, Stroke::new(1.5, t.accent), egui::StrokeKind::Outside);
}

/// Draw the dialog body.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> Outcome {
    let Some((i, id)) = app.active_ids() else { return Outcome::Cancel };
    let current = app.views[i].current;
    let selection = app.views[i].target_pages();
    let Some(doc) = app.session.get(id) else { return Outcome::Cancel };
    let count = doc.info.pages.len().max(1);
    let shown = doc.info.pages.get(current).map_or((612.0, 792.0), |p| (p.width, p.height));
    let state = std::sync::Arc::as_ptr(&doc.display) as usize;
    let d = &mut app.rotate_angle_draft;
    // A still render of the current page, made once per page and document state.
    let key = (id, current, state);
    if d.preview.as_ref().is_none_or(|p| p.key != key) {
        d.preview = doc.page_preview(current, 400).ok().map(|out| {
            let img = egui::ColorImage::from_rgba_premultiplied([out.width as usize, out.height as usize], &out.rgba);
            Preview { key, texture: ui.ctx().load_texture("rotate-angle-preview", img, egui::TextureOptions::LINEAR) }
        });
    }
    let pages = d.pages(&selection, count);
    let check = (state, pages.clone());
    if d.drawn.as_ref().is_none_or(|c| c.key != check) {
        d.drawn = Some(Drawn { pages: doc.pages_with_drawn_annotations(&pages), key: check });
    }
    let needs_flatten = !quarter_turn(d.degrees) && d.drawn.as_ref().is_some_and(|c| !c.pages.is_empty());

    ui.label(egui::RichText::new(tl!("Rotate by angle")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            egui::Grid::new("rotate-angle").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                let l = ui.label(tl!("Angle:"));
                ui.spacing_mut().slider_width = 180.0;
                ui.add(egui::Slider::new(&mut d.degrees, -180.0..=180.0).step_by(0.1).max_decimals(1).suffix("°")).labelled_by(l.id);
                ui.end_row();
                ui.label(tl!("Fit:"));
                ui.vertical(|ui| {
                    widgets::radio_value(ui, &mut d.fit, RotateFit::Expand, tl!("Expand page"));
                    widgets::radio_value(ui, &mut d.fit, RotateFit::KeepSize, tl!("Keep page size"));
                    widgets::radio_value(ui, &mut d.fit, RotateFit::Crop, tl!("Keep page size, crop"));
                });
                ui.end_row();
                ui.label(tl!("Pages:"));
                ui.vertical(|ui| {
                    widgets::radio_value(ui, &mut d.scope, Scope::All, tl!("All"));
                    widgets::radio_value(ui, &mut d.scope, Scope::Selection, tl!("Selection"));
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
                });
                ui.end_row();
            });
        });
        ui.add_space(16.0);
        preview(ui, t, d.preview.as_ref().map(|p| &p.texture), shown, d.degrees, d.fit);
    });
    if needs_flatten {
        ui.add_space(8.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(tl!(
                    "Comments and form fields can't turn by this angle. To rotate these pages, every comment and form field in the document is flattened first."
                ))
                .color(t.text_muted),
            )
            .wrap(),
        );
    }
    ui.add_space(12.0);
    let mut out = Outcome::Open;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let go = if needs_flatten { tl!("Flatten comments and rotate") } else { tl!("OK") };
        if widgets::pill_button(ui, go, true).clicked() && !pages.is_empty() {
            out = Outcome::Apply(Box::new(d.edit(pages.clone(), needs_flatten)));
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
    fn drafts_pick_pages_round_the_angle_and_flatten_on_request() {
        let mut d = RotateAngleDraft::default();
        assert_eq!(d.pages(&[1], 4), [0, 1, 2, 3]);
        d.open(true);
        assert_eq!(d.pages(&[1, 9], 4), [1]);
        d.scope = Scope::Range(2, 9);
        assert_eq!(d.pages(&[], 4), [1, 2, 3]);
        d.degrees = 12.345;
        assert_eq!(d.edit(vec![0], false), Edit::RotateContent { pages: vec![0], degrees: 12.3, fit: RotateFit::Expand });
        assert!(matches!(d.edit(vec![0], true), Edit::Batch { label, .. } if label == "Flatten comments and rotate"));
        assert!(quarter_turn(-90.0) && quarter_turn(180.0) && !quarter_turn(90.1));
    }

    #[test]
    fn the_preview_frame_follows_the_fit() {
        let (w, h, s) = layout(100.0, 100.0, 45.0, RotateFit::Expand);
        assert!((w - 141.42).abs() < 0.01 && (h - 141.42).abs() < 0.01 && s == 1.0);
        let (w, h, s) = layout(100.0, 100.0, 45.0, RotateFit::KeepSize);
        assert!(w == 100.0 && h == 100.0 && (s - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.001);
        assert_eq!(layout(100.0, 200.0, 30.0, RotateFit::Crop), (100.0, 200.0, 1.0));
        let (w, h, _) = layout(100.0, 200.0, 90.0, RotateFit::Crop);
        assert!((w - 200.0).abs() < 0.01 && (h - 100.0).abs() < 0.01, "a quarter turn turns the page");
    }
}
