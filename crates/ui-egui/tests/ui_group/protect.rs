//! Protect Using Password in the real shell (egui_kittest).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{Dialog, PdfCraftApp};

const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] >> endobj
trailer << /Root 1 0 R >>
%%EOF";

fn harness() -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("doc.pdf", None, FIXTURE.to_vec()).unwrap();
        app
    });
    h.run_steps(4);
    h
}

fn type_into(h: &mut Harness<'static, PdfCraftApp>, label: &str, text: &str) {
    h.get_by_label(label).click();
    h.run_steps(2);
    h.get_by_label(label).type_text(text);
    h.run_steps(2);
}

#[test]
fn protecting_for_viewing_requires_matching_passwords_and_saves_encrypted() {
    let mut h = harness();
    assert!(h.state_mut().execute("protect.password"));
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::Protect));
    h.get_by_label("Viewing");
    type_into(&mut h, "Type Password", "Open-Sesame-42");
    type_into(&mut h, "Re-type Password", "Open-Sesame-4");
    h.get_by_label_contains("don't match");
    assert!(h.query_by(|n| n.label().as_deref() == Some("Apply") && n.is_disabled()).is_some(), "Apply is disabled");
    type_into(&mut h, "Re-type Password", "2");
    h.get_by_label("Strength: Strong");
    h.get_by_label("Apply").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    let id = h.state().views[0].id;
    let doc = h.state().session.get(id).unwrap();
    assert!(doc.info.encrypted && doc.dirty);
    assert!(doc.security_summary().unwrap().pending);
    // What Save writes needs the password.
    let bytes = h.state().session.save_bytes(id).unwrap();
    let mut s = pdfcraft_engine::Session::new();
    assert!(s.open("p.pdf", None, bytes.clone(), None).is_err());
    assert!(s.open("p.pdf", None, bytes, Some("Open-Sesame-42")).is_ok());
    // Remove security is now offered, and undo also takes the protection back.
    assert!(h.state_mut().execute("protect.remove"));
    h.state_mut().undo();
    h.state_mut().undo();
    assert!(!h.state().session.get(id).unwrap().info.encrypted);
}

#[test]
fn editing_restrictions_via_advanced_options() {
    let mut h = harness();
    h.state_mut().execute("protect.password");
    h.run_steps(2);
    h.get_by_label("Editing").click();
    h.run_steps(2);
    h.get_by_label("Advanced Options ⌄").click();
    h.run_steps(2);
    h.get_by_label("Enable copying of text, images, and other content").click();
    h.run_steps(1);
    type_into(&mut h, "Type Password", "boss");
    type_into(&mut h, "Re-type Password", "boss");
    h.get_by_label("Apply").click();
    h.run_steps(3);
    let id = h.state().views[0].id;
    let sec = h.state().session.get(id).unwrap().security_summary().unwrap();
    assert!(sec.permissions.copy() && sec.permissions.print() && !sec.permissions.modify() && !sec.permissions.annotate());
    // Remove security is disabled for nobody here: this session set it.
    assert!(h.state().session.get(id).unwrap().allows_security_change());
}

fn restricted_harness() -> Harness<'static, PdfCraftApp> {
    use pdfcraft_engine::{Edit, Protection, Session};
    let mut author = Session::new();
    let id = author.open("source.pdf", None, std::sync::Arc::new(FIXTURE.to_vec()), None).unwrap();
    author.apply(id, Edit::Protect(Protection { permissions_password: Some("owner".into()), ..Default::default() })).unwrap();
    let bytes = author.save_bytes(id).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("restricted.pdf", None, bytes.as_ref().clone()).unwrap();
        app
    });
    h.run_steps(4);
    h
}

#[test]
fn removing_restrictions_requires_fresh_rights_confirmation_and_is_undoable() {
    let mut h = restricted_harness();
    let id = h.state().active_ids().unwrap().1;
    assert!(h.state_mut().execute("protect.remove_restrictions"));
    h.run_steps(4);
    assert!(h.query_all_by_label("restricted.pdf").count() >= 1);
    h.get_by_label("I have the right to edit this file");
    assert!(h.query_by(|n| n.label().as_deref() == Some("Remove restrictions") && n.is_disabled()).is_some());
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert!(h.state().session.get(id).unwrap().security_summary().is_some());
    assert!(!h.state().session.get(id).unwrap().dirty);
    h.get_by_label("I have the right to edit this file").click();
    h.run_steps(2);
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(!h.state().session.get(id).unwrap().dirty && h.state().dialog.is_none());
    assert!(h.state_mut().execute("protect.remove_restrictions"));
    h.run_steps(3);
    assert!(h.query_by(|n| n.label().as_deref() == Some("Remove restrictions") && n.is_disabled()).is_some(), "consent was not remembered");
    h.get_by_label("I have the right to edit this file").click();
    h.run_steps(2);
    h.get_all_by_label("Remove restrictions").last().unwrap().click();
    h.run_steps(3);
    let doc = h.state().session.get(id).unwrap();
    assert!(doc.dirty && doc.security_summary().is_none() && doc.allows_modification());
    assert_eq!(doc.can_undo(), Some("Remove restrictions"));
    assert!(h.state().dialog.is_none() && h.state().unlock_prompt.is_none());
    let plain = h.state().session.save_bytes(id).unwrap();
    let mut reopened = pdfcraft_engine::Session::new();
    assert!(reopened.open("plain.pdf", None, plain, None).is_ok());
    h.state_mut().undo();
    let doc = h.state().session.get(id).unwrap();
    assert!(doc.security_summary().is_some() && !doc.allows_modification());
    assert!(!doc.security_summary().unwrap().owner);
    assert!(h.state_mut().execute("protect.remove_restrictions"));
    h.run_steps(3);
    assert!(h.query_by(|n| n.label().as_deref() == Some("Remove restrictions") && n.is_disabled()).is_some());
}

#[test]
fn removing_restrictions_confirmation_never_follows_another_active_document() {
    let mut h = restricted_harness();
    let original = h.state().active_ids().unwrap().1;
    assert!(h.state_mut().execute("protect.remove_restrictions"));
    h.run_steps(3);
    h.get_by_label("I have the right to edit this file").click();
    h.run_steps(2);
    h.state_mut().open_bytes("other.pdf", None, FIXTURE.to_vec()).unwrap();
    h.run_steps(4);
    assert!(h.state().dialog.is_none(), "changing documents dismisses the document-bound consent");
    assert!(!h.state().session.get(original).unwrap().dirty);
    assert!(h.state().session.get(original).unwrap().security_summary().is_some());
    let other = h.state().active_ids().unwrap().1;
    assert_ne!(original, other);
    assert!(!h.state().session.get(other).unwrap().dirty);
}

#[test]
fn removing_restrictions_is_disabled_without_a_protected_document() {
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    assert!(pdfcraft_engine::commands::command("protect.remove_restrictions").is_some());
    assert!(!app.execute("protect.remove_restrictions"));
    app.open_bytes("plain.pdf", None, FIXTURE.to_vec()).unwrap();
    assert!(!app.execute("protect.remove_restrictions"));
    assert!(app.dialog.is_none() && !app.session.get(app.active_ids().unwrap().1).unwrap().dirty);
}
