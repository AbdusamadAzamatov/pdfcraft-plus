//! Organize ▸ Layout ▸ Remove blank pages (PL5): the dialog finds the pages that look blank
//! ([`pdfcraft_engine::blank`]) on a worker thread, showing its progress, and lists them as
//! thumbnails, each checked for removal. Unchecking a page keeps it; Remove deletes the checked
//! pages in one undo step. Changing the sensitivity or the comments option searches again.

use std::sync::{Arc, Mutex};

use pdfcraft_engine::blank::{BlankJob, DEFAULT_SENSITIVITY};
use pdfcraft_engine::{BlankOptions, BlankPage, DocId, Edit, EditError};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// The sensitivity slider's range, in percent.
const MAX_PERCENT: f32 = 2.0;
/// A thumbnail cell's picture box, in points.
const THUMB: egui::Vec2 = egui::vec2(84.0, 108.0);

/// Progress of a search: pages done, total, the result once finished, and a cancel request.
#[derive(Default)]
pub struct BlankProgress {
    pub done: usize,
    pub total: usize,
    pub result: Option<Result<Vec<BlankPage>, EditError>>,
    pub cancel: bool,
}

/// The dialog's state. The options stay between uses; the results belong to one search.
pub struct BlankDraft {
    /// The largest share of ink, in percent (0–2).
    pub percent: f32,
    pub ignore_annotated: bool,
    /// The document searched.
    pub doc: Option<DocId>,
    /// The search under way, if any.
    pub progress: Option<Arc<Mutex<BlankProgress>>>,
    /// What the last search found, each with whether it is checked for removal.
    pub found: Vec<(BlankPage, bool)>,
    /// Why the last search failed.
    pub error: Option<String>,
}

impl Default for BlankDraft {
    fn default() -> Self {
        BlankDraft { percent: DEFAULT_SENSITIVITY * 100.0, ignore_annotated: true, doc: None, progress: None, found: Vec::new(), error: None }
    }
}

impl BlankDraft {
    /// The search options these choices make.
    pub fn options(&self) -> BlankOptions {
        let percent = if self.percent.is_finite() { self.percent.clamp(0.0, MAX_PERCENT) } else { DEFAULT_SENSITIVITY * 100.0 };
        BlankOptions { sensitivity: percent / 100.0, ignore_annotated: self.ignore_annotated, pages: Vec::new() }
    }

    /// The pages checked for removal (0-based, in order).
    pub fn checked(&self) -> Vec<usize> {
        self.found.iter().filter(|(_, on)| *on).map(|(b, _)| b.page).collect()
    }

    /// Ask a running search to stop; its result is dropped.
    pub fn cancel(&mut self) {
        if let Some(p) = self.progress.take()
            && let Ok(mut s) = p.lock()
        {
            s.cancel = true;
        }
    }

    /// Take the result of a finished search, if one has finished.
    fn poll(&mut self) {
        let Some(p) = self.progress.clone() else { return };
        let result = match p.lock() {
            Ok(mut s) => s.result.take(),
            // The worker panicked holding the lock: it will never report back.
            Err(_) => Some(Err(EditError::Invalid("the search stopped unexpectedly".into()))),
        };
        let Some(result) = result else { return };
        self.progress = None;
        match result {
            Ok(found) => self.found = found.into_iter().map(|b| (b, true)).collect(),
            Err(EditError::Cancelled) => {}
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}

impl PdfCraftApp {
    /// Open Remove blank pages on the active document and start looking.
    pub fn open_remove_blank(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        self.dialog = Some(crate::Dialog::RemoveBlank);
        self.find_blank(id);
    }

    /// Search document `id` with the dialog's options, replacing any search under way.
    fn find_blank(&mut self, id: DocId) {
        let d = &mut self.blank_draft;
        d.cancel();
        d.doc = Some(id);
        d.found.clear();
        d.error = None;
        let job = match self.session.blank_job(id, &d.options()) {
            Ok(job) => job,
            Err(e) => {
                d.error = Some(e.to_string());
                return;
            }
        };
        let progress = Arc::new(Mutex::new(BlankProgress { total: job.len(), ..Default::default() }));
        d.progress = Some(progress.clone());
        run_job(job, progress, self.run_inline);
        self.blank_draft.poll();
    }

    /// Remove: delete the checked pages as one undoable edit. Every page is refused (the
    /// document must keep one), saying why.
    pub(crate) fn remove_blank_with_draft(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let pages = self.blank_draft.checked();
        let count = self.session.get(id).map_or(0, |d| d.info.pages.len());
        if pages.is_empty() || self.blank_draft.doc != Some(id) {
            return;
        }
        if pages.len() >= count {
            self.notify_tr("A document must keep at least one page: uncheck a page to keep it.");
            return;
        }
        if self.apply_edit(Edit::DeletePages { pages }) {
            self.blank_draft.found.clear();
        }
    }
}

/// Run the search on a worker thread (inline in tests and on the web).
fn run_job(job: BlankJob, progress: Arc<Mutex<BlankProgress>>, inline: bool) {
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
    } else if let Err(e) = std::thread::Builder::new().name("pdfcraft-blank".into()).spawn(work) {
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

/// Draw the dialog; returns (remove, cancel).
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> (bool, bool) {
    let Some((vi, id)) = app.active_ids() else { return (false, true) };
    let Some(doc) = app.session.get(id) else { return (false, true) };
    let count = doc.info.pages.len();
    let sizes: Vec<(f32, f32)> = doc.info.pages.iter().map(|p| (p.width, p.height)).collect();
    app.blank_draft.poll();
    let mut again = app.blank_draft.doc != Some(id);
    let d = &mut app.blank_draft;
    ui.label(egui::RichText::new(tl!("Remove blank pages…").trim_end_matches('…')).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(tl!("Sensitivity"));
        let slider = ui
            .add(egui::Slider::new(&mut d.percent, 0.0..=MAX_PERCENT).step_by(0.05).fixed_decimals(2).suffix("%"))
            .on_hover_text(tl!("Pages with at most this share of dark pixels count as blank"));
        // Search again once the slider is let go (or nudged with the keyboard).
        again |= slider.drag_stopped() || (slider.changed() && !slider.dragged());
    });
    again |= ui.add(widgets::checkbox(&mut d.ignore_annotated, tl!("Ignore pages with comments or form fields"))).changed();
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
        ui.label(egui::RichText::new(tl!("No blank pages found")).color(t.text_muted));
    } else {
        let view = &mut app.views[vi];
        egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
                for (b, on) in d.found.iter_mut() {
                    view.need_thumbnail(b.page, true);
                    let (w, h) = sizes.get(b.page).copied().unwrap_or((THUMB.x, THUMB.y));
                    let fit = (THUMB.x / w.max(1.0)).min(THUMB.y / h.max(1.0));
                    let size = egui::vec2((w * fit).max(4.0), (h * fit).max(4.0));
                    ui.vertical(|ui| {
                        ui.set_width(THUMB.x);
                        let (rect, resp) = ui.allocate_exact_size(THUMB, egui::Sense::click());
                        let page = egui::Rect::from_center_size(rect.center(), size);
                        match view.thumb_id(b.page) {
                            Some(tex) => {
                                ui.painter().image(
                                    tex,
                                    page,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    egui::Color32::WHITE,
                                );
                            }
                            None => {
                                ui.painter().rect_filled(page, 0.0, egui::Color32::WHITE);
                            }
                        }
                        let stroke = if *on { egui::Stroke::new(2.0, t.accent) } else { egui::Stroke::new(1.0, t.border) };
                        ui.painter().rect_stroke(page, 0.0, stroke, egui::StrokeKind::Inside);
                        if resp.clicked() {
                            *on = !*on;
                        }
                        let label = crate::i18n::fmt(tl!("Page {label}"), &[("label", &(b.page + 1).to_string())]);
                        ui.add(widgets::checkbox(on, label));
                    });
                }
            });
        });
    }
    let checked = d.found.iter().filter(|(_, on)| *on).count();
    let all = checked > 0 && checked >= count;
    if all {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tl!("A document must keep at least one page: uncheck a page to keep it.")).small().color(t.text_muted));
    }
    ui.add_space(12.0);
    let (mut remove, mut cancel) = (false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let can = running.is_none() && checked > 0 && !all;
        if ui.add_enabled_ui(can, |ui| widgets::pill_button(ui, tl!("Remove"), true)).inner.clicked() {
            remove = true;
        }
        if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
            cancel = true;
        }
    });
    if again && !remove && !cancel {
        app.find_blank(id);
    }
    (remove, cancel)
}
