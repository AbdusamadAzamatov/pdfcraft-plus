//! Organize ▸ Layout and Scan & OCR ▸ Straighten scans (PL9): the dialog measures each page's tilt
//! ([`pdfcraft_engine::deskew`]) on a worker thread, showing its progress, and lists the tilted
//! pages with their angles, each checked. Unchecking a page leaves it; Straighten turns the
//! checked pages in one undo step. Changing an option measures again.

use std::sync::{Arc, Mutex};

use pdfcraft_engine::deskew::{DEFAULT_MIN_ANGLE, DeskewJob};
use pdfcraft_engine::{DeskewOptions, DeskewPage, DeskewSkip, DocId, Edit, EditError};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// The smallest-angle slider's range, in degrees.
const MAX_MIN_ANGLE: f64 = 5.0;

/// Progress of a measurement: pages done, total, the result once finished, and a cancel request.
#[derive(Default)]
pub struct DeskewProgress {
    pub done: usize,
    pub total: usize,
    pub result: Option<Result<Vec<DeskewPage>, EditError>>,
    pub cancel: bool,
}

/// The dialog's state. The options stay between uses; the results belong to one measurement.
pub struct DeskewDraft {
    pub only_scans: bool,
    /// The smallest tilt to correct, in degrees (0–5).
    pub min_angle: f64,
    /// The document measured.
    pub doc: Option<DocId>,
    /// The measurement under way, if any.
    pub progress: Option<Arc<Mutex<DeskewProgress>>>,
    /// The pages to straighten the last measurement found, each with whether it is checked.
    pub found: Vec<(DeskewPage, bool)>,
    /// Pages left alone because they have comments or form fields.
    pub annotated: usize,
    /// Why the last measurement failed.
    pub error: Option<String>,
}

impl Default for DeskewDraft {
    fn default() -> Self {
        DeskewDraft { only_scans: true, min_angle: DEFAULT_MIN_ANGLE, doc: None, progress: None, found: Vec::new(), annotated: 0, error: None }
    }
}

impl DeskewDraft {
    /// The options these choices make.
    pub fn options(&self) -> DeskewOptions {
        let min_angle = if self.min_angle.is_finite() { self.min_angle.clamp(0.0, MAX_MIN_ANGLE) } else { DEFAULT_MIN_ANGLE };
        DeskewOptions { only_scans: self.only_scans, min_angle, pages: Vec::new() }
    }

    /// The checked pages (0-based, in order) with their angles.
    pub fn checked(&self) -> Vec<(usize, f64)> {
        self.found.iter().filter(|(_, on)| *on).filter_map(|(p, _)| p.angle.map(|a| (p.page, a))).collect()
    }

    /// Ask a running measurement to stop; its result is dropped.
    pub fn cancel(&mut self) {
        if let Some(p) = self.progress.take()
            && let Ok(mut s) = p.lock()
        {
            s.cancel = true;
        }
    }

    /// Take the result of a finished measurement, if one has finished.
    fn poll(&mut self) {
        let Some(p) = self.progress.clone() else { return };
        let result = match p.lock() {
            Ok(mut s) => s.result.take(),
            // The worker panicked holding the lock: it will never report back.
            Err(_) => Some(Err(EditError::Invalid("the measurement stopped unexpectedly".into()))),
        };
        let Some(result) = result else { return };
        self.progress = None;
        match result {
            Ok(found) => {
                self.annotated = found.iter().filter(|p| p.skip == Some(DeskewSkip::Annotated)).count();
                self.found = found.into_iter().filter(DeskewPage::straightens).map(|p| (p, true)).collect();
            }
            Err(EditError::Cancelled) => {}
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}

impl PdfCraftApp {
    /// Open Straighten scans on the active document and start measuring.
    pub fn open_deskew(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        self.dialog = Some(crate::Dialog::Deskew);
        self.measure_skew(id);
    }

    /// Measure document `id` with the dialog's options, replacing any measurement under way.
    fn measure_skew(&mut self, id: DocId) {
        let d = &mut self.deskew_draft;
        d.cancel();
        d.doc = Some(id);
        d.found.clear();
        d.annotated = 0;
        d.error = None;
        let job = match self.session.deskew_job(id, &d.options()) {
            Ok(job) => job,
            Err(e) => {
                d.error = Some(e.to_string());
                return;
            }
        };
        let progress = Arc::new(Mutex::new(DeskewProgress { total: job.len(), ..Default::default() }));
        d.progress = Some(progress.clone());
        run_job(job, progress, self.run_inline);
        self.deskew_draft.poll();
    }

    /// Straighten: turn the checked pages as one undoable edit.
    pub(crate) fn deskew_with_draft(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let pages = self.deskew_draft.checked();
        if pages.is_empty() || self.deskew_draft.doc != Some(id) {
            return;
        }
        if self.apply_edit(Edit::straighten(pages)) {
            self.deskew_draft.found.clear();
        }
    }
}

/// Run the measurement on a worker thread (inline in tests and on the web).
fn run_job(job: DeskewJob, progress: Arc<Mutex<DeskewProgress>>, inline: bool) {
    let p = progress.clone();
    let work = move || {
        let result = job.run(|done, total| {
            let Ok(mut s) = p.lock() else { return false };
            s.done = done;
            s.total = total;
            !s.cancel
        });
        if let Ok(mut s) = p.lock() {
            s.result = Some(result);
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    if inline {
        work();
    } else if let Err(e) = std::thread::Builder::new().name("pdfcraft-deskew".into()).spawn(work) {
        // No worker: report the failure instead of waiting for a result that never comes.
        if let Ok(mut s) = progress.lock() {
            s.result = Some(Err(EditError::Invalid(e.to_string())));
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (inline, progress);
        work();
    }
}

/// Draw the dialog; returns (straighten, cancel).
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> (bool, bool) {
    let Some((_, id)) = app.active_ids() else { return (false, true) };
    if app.session.get(id).is_none() {
        return (false, true);
    }
    app.deskew_draft.poll();
    let mut again = app.deskew_draft.doc != Some(id);
    let d = &mut app.deskew_draft;
    ui.label(egui::RichText::new(tl!("Straighten scans…").trim_end_matches('…')).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    again |= ui.add(widgets::checkbox(&mut d.only_scans, tl!("Only pages that are scans"))).changed();
    ui.horizontal(|ui| {
        ui.label(tl!("Minimum angle"));
        let slider = ui.add(egui::Slider::new(&mut d.min_angle, 0.0..=MAX_MIN_ANGLE).step_by(0.1).fixed_decimals(1).suffix("°"));
        // Measure again once the slider is let go (or nudged with the keyboard).
        again |= slider.drag_stopped() || (slider.changed() && !slider.dragged());
    });
    ui.add_space(8.0);
    let running = d.progress.as_ref().and_then(|p| p.lock().ok().map(|s| (s.done, s.total)));
    if let Some((done, total)) = running {
        let label =
            crate::i18n::fmt(tl!("Checking page {d} of {t}…"), &[("d", &(done + 1).min(total.max(1)).to_string()), ("t", &total.max(1).to_string())]);
        ui.label(label);
        ui.add(egui::ProgressBar::new(if total == 0 { 0.0 } else { done as f32 / total as f32 }).desired_height(6.0));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    } else if let Some(e) = &d.error {
        ui.label(egui::RichText::new(e).color(t.text_muted));
    } else if d.found.is_empty() {
        ui.label(egui::RichText::new(tl!("No tilted pages found")).color(t.text_muted));
    } else {
        egui::ScrollArea::vertical().max_height(280.0).auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("deskew-pages").num_columns(2).spacing(egui::vec2(24.0, 6.0)).show(ui, |ui| {
                for (p, on) in d.found.iter_mut() {
                    let label = crate::i18n::fmt(tl!("Page {label}"), &[("label", &(p.page + 1).to_string())]);
                    ui.add(widgets::checkbox(on, label));
                    // Positive tilts rise to the right; the page turns clockwise by that much.
                    ui.label(format!("{:+.2}°", p.angle.unwrap_or(0.0)));
                    ui.end_row();
                }
            });
        });
    }
    if running.is_none() && d.annotated > 0 {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tl!("Pages with comments or form fields are left as they are")).small().color(t.text_muted));
    }
    let checked = d.found.iter().filter(|(_, on)| *on).count();
    ui.add_space(12.0);
    let (mut go, mut cancel) = (false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let can = running.is_none() && checked > 0;
        if ui.add_enabled_ui(can, |ui| widgets::pill_button(ui, tl!("Straighten"), true)).inner.clicked() {
            go = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            cancel = true;
        }
    });
    if again && !go && !cancel {
        app.measure_skew(id);
    }
    (go, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_clamp_the_angle_and_checked_lists_angles() {
        let mut d = DeskewDraft { min_angle: f64::NAN, ..Default::default() };
        assert_eq!(d.options().min_angle, DEFAULT_MIN_ANGLE);
        d.min_angle = 40.0;
        assert_eq!(d.options().min_angle, MAX_MIN_ANGLE);
        let page = |page, angle| DeskewPage { page, angle: Some(angle), confidence: 0.9, skip: None };
        d.found = vec![(page(0, 1.5), true), (page(2, -0.7), false), (page(4, 3.0), true)];
        assert_eq!(d.checked(), [(0, 1.5), (4, 3.0)]);
    }
}
