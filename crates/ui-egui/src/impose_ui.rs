//! Organize ▸ Layout ▸ Pages per sheet (N-up), Booklet and Poster (PL1): the print layouts as a
//! page tool. Each dialog reuses the Print dialog's page and layout controls
//! ([`print_ui::pages_controls`], [`print_ui::layout_controls`]) with its handling fixed, adds the
//! paper (by default the size of the first page laid out), and Create opens the sheets as a new,
//! unsaved document ([`pdfcraft_engine::Session::impose_new`]). The document itself is not changed.

use pdfcraft_engine::print::{self, PAPERS};

use crate::print_ui::{self, Handling, PrintDraft, Which};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// Dialog state: the Print dialog's draft (its handling is the dialog's) and the paper.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ImposeDraft {
    pub print: PrintDraft,
    /// A [`PAPERS`] index, or `None` for the size of the first page laid out, upright.
    pub paper: Option<usize>,
}

impl ImposeDraft {
    /// The layout settings for a document with these page sizes (display space) and labels.
    pub fn settings(&self, sizes: &[(f64, f64)], labels: &[String]) -> Result<print::Settings, String> {
        let mut settings = self.print.settings(sizes.len(), labels)?;
        settings.raster = None;
        settings.crops = Vec::new();
        settings.paper = match self.paper {
            Some(i) => PAPERS.get(i).map_or(PAPERS[0].1, |p| p.1),
            None => {
                let first = settings.pages.first().and_then(|&p| sizes.get(p)).copied();
                let (w, h) = first.ok_or_else(|| tl!("there are no pages to print").to_string())?;
                (w.min(h), w.max(h))
            }
        };
        Ok(settings)
    }
}

/// The dialog's title and the label it is opened from (Organize ▸ Layout, the Pages menu).
fn label(handling: Handling) -> &'static str {
    match handling {
        Handling::Booklet => "Booklet…",
        Handling::Poster => "Poster (tile)…",
        _ => "Pages per sheet (N-up)…",
    }
}

impl PdfCraftApp {
    /// Open the N-up, Booklet or Poster dialog on the active document. The choices made last time
    /// stay; the pages start from the pages picked, as in Print.
    pub fn open_impose(&mut self, handling: Handling) {
        let Some((i, _)) = self.active_ids() else { return };
        let current = self.views[i].current;
        let selected: Vec<usize> = self.views[i].selected.iter().copied().collect();
        let keep = std::mem::take(&mut self.impose_draft);
        let which = match (selected.is_empty(), keep.print.which) {
            (false, _) => Which::Selected,
            (true, Which::Selected) => Which::All,
            (true, other) => other,
        };
        self.impose_draft = ImposeDraft {
            print: PrintDraft {
                handling,
                units: self.units,
                current_page: current,
                selected,
                which,
                printer: None,
                duplex: Some(pdfcraft_engine::print::spool::Duplex::Off),
                as_image: false,
                ..keep.print
            },
            paper: keep.paper,
        };
        self.dialog = Some(crate::Dialog::Impose(handling));
    }

    /// Create: lay the pages out and open the sheets as a new document.
    pub(crate) fn impose_with_draft(&mut self) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(doc) = self.session.get(id) else { return };
        let sizes: Vec<(f64, f64)> = doc.info.pages.iter().map(|p| (p.width as f64, p.height as f64)).collect();
        let labels: Vec<String> = doc.info.pages.iter().map(|p| p.label.clone()).collect();
        let name = doc.name.clone();
        let settings = match self.impose_draft.settings(&sizes, &labels) {
            Ok(s) => s,
            Err(e) => return self.notify_fmt("Couldn't create a PDF: {e}", &[("e", &e)]),
        };
        let name = pdfcraft_engine::impose_name(&name, &settings.layout);
        match self.session.impose_new(id, settings) {
            Ok(bytes) => {
                let message = crate::i18n::fmt(tl!("Created {name}"), &[("name", &name)]);
                self.open_created(&name, bytes, &message);
            }
            Err(e) => self.notify_fmt("Couldn't create a PDF: {e}", &[("e", &e.to_string())]),
        }
    }
}

/// Draw the dialog; returns (create, cancel).
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens, handling: Handling) -> (bool, bool) {
    let Some((_, id)) = app.active_ids() else { return (false, true) };
    let Some(doc) = app.session.get(id) else { return (false, true) };
    let sizes: Vec<(f64, f64)> = doc.info.pages.iter().map(|p| (p.width as f64, p.height as f64)).collect();
    let labels: Vec<String> = doc.info.pages.iter().map(|p| p.label.clone()).collect();
    let d = &mut app.impose_draft;
    d.print.handling = handling;
    ui.label(egui::RichText::new(tl!(label(handling)).trim_end_matches('…')).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(tl!("Paper:"));
        let shown = match d.paper.and_then(|i| PAPERS.get(i)) {
            Some((name, _)) => tl!(name),
            None => tl!("Same as first page"),
        };
        craft_ui::ComboBox::from_id_salt("impose-paper").selected_text(shown).width(260.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut d.paper, None, tl!("Same as first page"));
            for (i, (name, _)) in PAPERS.iter().enumerate() {
                ui.selectable_value(&mut d.paper, Some(i), tl!(name));
            }
        });
    });
    ui.add_space(6.0);
    widgets::section_title(ui, tl!("Pages"));
    print_ui::pages_controls(ui, &mut d.print, sizes.len());
    ui.add_space(10.0);
    print_ui::layout_controls(ui, &mut d.print, t);
    ui.add_space(8.0);
    let settings = d.settings(&sizes, &labels);
    let laid_out = settings.as_ref().map_err(Clone::clone).and_then(|s| print::layout(&sizes, s).map_err(|e| e.said(&crate::i18n::words)));
    match &laid_out {
        Ok(_) => ui.label(egui::RichText::new(tl!("Creates a new document; this one is not changed.")).small().color(t.text_muted)),
        Err(e) => ui.label(egui::RichText::new(tl!(e)).small().color(t.text_muted)),
    };
    ui.add_space(12.0);
    let (mut create, mut cancel) = (false, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
    fn the_paper_is_the_first_page_upright_unless_one_is_chosen() {
        let sizes = [(842.0, 595.0), (200.0, 300.0)];
        let labels = ["1".to_string(), "2".to_string()];
        let mut d = ImposeDraft { print: PrintDraft { handling: Handling::Multiple, ..Default::default() }, paper: None };
        let s = d.settings(&sizes, &labels).unwrap();
        assert_eq!(s.paper, (595.0, 842.0));
        assert!(matches!(s.layout, print::Layout::Multiple { per_sheet: 2, .. }));
        d.paper = Some(4);
        assert_eq!(d.settings(&sizes, &labels).unwrap().paper, PAPERS[4].1);
        d.print.as_image = true;
        assert_eq!(d.settings(&sizes, &labels).unwrap().raster, None, "never pictures");
    }
}
