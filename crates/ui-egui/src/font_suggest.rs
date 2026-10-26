//! Missing fonts (PdfCraft Plus): when a paragraph being edited uses a font the PDF holds only
//! partly (a subset), the Edit panel says so and, for the families with a free look-alike
//! ([`pdfcraft_fonts::lookalike`]), offers to download it. Once it is installed, letters the
//! subset lacks are written in the look-alike, which has the same widths, instead of a fallback
//! of another design.
//!
//! The desktop app supplies the download ([`FontFetch`]), so this crate has no network code;
//! without it (the web build, tests) the panel points to Load font… instead.

use std::sync::Arc;

use pdfcraft_fonts::lookalike::{self, Lookalike, LookalikeFile};

use crate::PdfCraftApp;
use crate::theme::{self, Tokens};

/// Downloads one catalogue file, checks it and installs it (blocking; it runs on its own thread).
pub type FontFetch = Arc<dyn Fn(&'static LookalikeFile) -> Result<(), String> + Send + Sync>;

/// A family being downloaded, or the last download that failed.
#[derive(Default)]
pub(crate) struct Downloads {
    #[cfg(not(target_arch = "wasm32"))]
    running: Vec<(&'static str, std::sync::mpsc::Receiver<Result<(), String>>)>,
    failed: Vec<(&'static str, String)>,
}

/// How one of the paragraph's fonts stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Status {
    /// A look-alike exists but isn't installed yet.
    Offer(&'static Lookalike),
    /// Its look-alike is installed: new letters (and the editor) use it.
    Matched(&'static Lookalike),
    /// No look-alike: new letters use a fallback.
    NoLookalike,
}

/// Why a font is listed: the PDF holds only the letters it uses (a subset), or doesn't include
/// the font at all (or in a form the editor can't read).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gap {
    Partial,
    Missing,
}

/// The family a PDF font name shows ("ABCDEF+TimesNewRoman,Bold" → "TimesNewRoman").
fn shown_family(base_font: &str) -> String {
    let name = base_font.rsplit('+').next().unwrap_or(base_font);
    name.split([',', '-']).next().unwrap_or(name).to_string()
}

/// Whether a `/BaseFont` names a subset (`ABCDEF+Name`).
fn is_subset(base_font: &str) -> bool {
    base_font.split_once('+').is_some_and(|(tag, _)| tag.len() == 6 && tag.bytes().all(|b| b.is_ascii_uppercase()))
}

/// One of the paragraph's fonts, for [`statuses`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FontUse {
    /// The run's `/BaseFont`.
    pub base_font: String,
    /// The editor has the font's own program (not a stand-in).
    pub own: bool,
    /// The paragraph as typed has a character its own programs can't show.
    pub lacks_typed: bool,
}

/// The paragraph's fonts the PDF holds only partly or not at all, one entry per family, in the
/// order they appear.
pub(crate) fn statuses(fonts: &[FontUse]) -> Vec<(String, Status, Gap)> {
    let mut out: Vec<(String, Status, Gap)> = Vec::new();
    for FontUse { base_font: base, own, lacks_typed } in fonts {
        let gap = if !own {
            Gap::Missing
        } else if is_subset(base) || *lacks_typed {
            Gap::Partial
        } else {
            continue;
        };
        let status = match lookalike::for_font(base) {
            Some((l, bold, italic)) => match l.file(bold, italic) {
                Some(f) if lookalike::is_installed(f) => Status::Matched(l),
                _ => Status::Offer(l),
            },
            None => Status::NoLookalike,
        };
        let family = match status {
            Status::Offer(l) | Status::Matched(l) => l.replaces.first().copied().unwrap_or(l.family).to_string(),
            Status::NoLookalike => shown_family(base),
        };
        match out.iter_mut().find(|(f, _, _)| *f == family) {
            // One style of a family still to download makes the whole family an offer.
            Some(entry) => {
                if matches!(status, Status::Offer(_)) {
                    entry.1 = status;
                }
                if gap == Gap::Missing {
                    entry.2 = gap;
                }
            }
            None => out.push((family, status, gap)),
        }
    }
    out
}

impl PdfCraftApp {
    /// Download and install every file of a look-alike family in the background.
    pub(crate) fn download_lookalike(&mut self, lookalike: &'static Lookalike) {
        let Some(fetch) = self.font_fetch.clone() else { return };
        self.font_downloads.failed.retain(|(f, _)| *f != lookalike.family);
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.font_downloads.running.iter().any(|(f, _)| *f == lookalike.family) {
                return;
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = self.ctx.clone();
            std::thread::spawn(move || {
                let result = lookalike.files.iter().filter(|f| !lookalike::is_installed(f)).try_for_each(|f| fetch(f));
                // The receiver may be gone (the app quit): nothing to report to then.
                let _ = tx.send(result);
                if let Some(ctx) = ctx {
                    ctx.request_repaint();
                }
            });
            self.font_downloads.running.push((lookalike.family, rx));
        }
        #[cfg(target_arch = "wasm32")]
        let _ = fetch;
    }

    pub(crate) fn lookalike_downloading(&self, lookalike: &Lookalike) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.font_downloads.running.iter().any(|(f, _)| *f == lookalike.family);
        #[cfg(target_arch = "wasm32")]
        {
            let _ = lookalike;
            false
        }
    }

    /// Pick up finished downloads (each frame). The open paragraph's editor gets the new face at
    /// once, so the letters typed next show in it.
    pub(crate) fn poll_font_downloads(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut finished = Vec::new();
            self.font_downloads.running.retain(|(family, rx)| match rx.try_recv() {
                Ok(result) => {
                    finished.push((*family, result));
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished.push((*family, Err("the download stopped unexpectedly".into())));
                    false
                }
            });
            for (family, result) in finished {
                match result {
                    Ok(()) => {
                        self.notify_fmt("{family} installed: new letters now match the text's font.", &[("family", family)]);
                        self.refresh_editor_fonts();
                    }
                    Err(e) => self.font_downloads.failed.push((family, e)),
                }
            }
        }
    }

    /// Give the open paragraph editor the faces available now.
    fn refresh_editor_fonts(&mut self) {
        let (Some(ctx), Some((i, id))) = (self.ctx.clone(), self.active_ids()) else { return };
        let Some(doc) = self.session.get(id) else { return };
        if let Some(ed) = self.views.get_mut(i).and_then(|v| v.line_editor.as_mut()) {
            ed.fonts = crate::edit_text_ui::install_source_fonts(&ctx, doc, ed.page);
        }
    }
}

/// The Edit panel's Fonts section for the paragraph being edited: nothing when all its fonts are
/// complete.
pub(crate) fn section(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, fonts: &[FontUse]) {
    let statuses = statuses(fonts);
    if statuses.is_empty() {
        return;
    }
    crate::widgets::section_title(ui, tl!("Fonts"));
    let mut download = None;
    for (family, status, gap) in statuses {
        egui::Frame::NONE.fill(t.pressed).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(&family).font(theme::semibold(13.0)));
            match status {
                Status::Matched(l) => {
                    ui.label(crate::i18n::fmt(
                        tl!("New letters use {lookalike}, a free font with the same letter widths."),
                        &[("lookalike", l.family)],
                    ));
                }
                Status::NoLookalike if gap == Gap::Missing => {
                    ui.label(tl!("This PDF doesn't include this font, so edited text shows in another font; Load font… picks a matching one."));
                }
                Status::NoLookalike => {
                    ui.label(tl!("This PDF holds only the letters it uses. New letters will use another font; Load font… picks a matching one."));
                }
                Status::Offer(l) => {
                    ui.label(if gap == Gap::Missing {
                        tl!("This PDF doesn't include this font, so edited text would show in another font.")
                    } else {
                        tl!("This PDF holds only the letters it uses, so new letters would use another font.")
                    });
                    if app.font_fetch.is_none() {
                        ui.label(crate::i18n::fmt(tl!("Load the free look-alike {lookalike} with Load font…."), &[("lookalike", l.family)]));
                    } else if app.lookalike_downloading(l) {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(crate::i18n::fmt(tl!("Downloading {lookalike}…"), &[("lookalike", l.family)]));
                        });
                    } else {
                        let size = format!("{:.1} MB", l.size() as f64 / 1_000_000.0);
                        let label = crate::i18n::fmt(tl!("Download {lookalike} ({size})"), &[("lookalike", l.family), ("size", &size)]);
                        if ui
                            .button(label)
                            .on_hover_text(crate::i18n::fmt(
                                tl!("{lookalike} is free ({licence}) and has the same letter widths, so the text keeps its layout."),
                                &[("lookalike", l.family), ("licence", l.licence)],
                            ))
                            .clicked()
                        {
                            download = Some(l);
                        }
                        if let Some((_, e)) = app.font_downloads.failed.iter().find(|(f, _)| *f == l.family) {
                            ui.colored_label(ui.visuals().error_fg_color, crate::i18n::fmt(tl!("The download failed: {e}"), &[("e", e)]));
                        }
                    }
                }
            }
        });
        ui.add_space(4.0);
    }
    if let Some(l) = download {
        app.download_lookalike(l);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_and_missing_fonts_are_listed_once_per_family() {
        let own =
            |names: &[(&str, bool)]| names.iter().map(|(n, o)| FontUse { base_font: n.to_string(), own: *o, lacks_typed: false }).collect::<Vec<_>>();
        let fonts = own(&[
            ("DEVEXP+TimesNewRoman", true),
            ("DEVEXP+TimesNewRoman,Bold", true),
            ("LiberationSerif", true),
            ("ABCDEF+Georgia-Italic", true),
            ("Helvetica", false),
        ]);
        let s = statuses(&fonts);
        assert_eq!(s.len(), 3, "{s:?}");
        assert_eq!((s[0].0.as_str(), s[0].2), ("Times New Roman", Gap::Partial));
        assert!(matches!(s[0].1, Status::Offer(l) if l.family == "Tinos"));
        assert_eq!(s[1], ("Georgia".to_string(), Status::NoLookalike, Gap::Partial));
        assert_eq!((s[2].0.as_str(), s[2].2), ("Arial", Gap::Missing), "Helvetica isn't in the PDF; Arimo stands in for it");
        assert!(statuses(&own(&[("LiberationSerif", true), ("abcdef+Arial", true)])).is_empty(), "complete fonts and non-subset tags");
        // A complete font is listed once the text typed has a letter it can't show.
        let typed = [FontUse { base_font: "LiberationSerif".into(), own: true, lacks_typed: true }];
        assert!(matches!(statuses(&typed).as_slice(), [(f, Status::Offer(l), Gap::Partial)] if f == "Times New Roman" && l.family == "Tinos"));
    }
}
