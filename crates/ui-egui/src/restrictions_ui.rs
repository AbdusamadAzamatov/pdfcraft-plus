//! Personal-fork-only security removal: explicit consent, never remembered between dialogs.

use crate::theme::{self, Tokens};
use crate::widgets;

/// Draw the confirmation for one named document. Returns (remove, cancel).
pub(crate) fn body(ui: &mut egui::Ui, name: &str, confirmed_rights: &mut bool, t: &Tokens) -> (bool, bool) {
    ui.label(egui::RichText::new(tl!("Remove restrictions")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    ui.label(egui::RichText::new(crate::bidi::visual(name)).strong());
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(tl!("Fork only. Removes security without the permissions password. Saving rewrites the file unencrypted."))
            .color(t.text_muted),
    );
    ui.add_space(12.0);
    ui.add(widgets::checkbox(confirmed_rights, tl!("I have the right to edit this file")));
    ui.add_space(16.0);
    let (mut remove, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            remove = ui.add_enabled_ui(*confirmed_rights, |ui| widgets::pill_button(ui, tl!("Remove restrictions"), true)).inner.clicked();
            cancel = widgets::pill_button(ui, tl!("Cancel"), false).clicked();
        });
    });
    (remove, cancel)
}
