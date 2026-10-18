//! Organize pages ▸ Divide pages in the real shell (egui_kittest).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{Dialog, PdfCraftApp};

/// A 600 × 400 spread between two 300 × 400 covers.
const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 300 400] >> endobj
3 0 obj << /Type /Page /Parent 2 0 R >> endobj
4 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 600 400] >> endobj
5 0 obj << /Type /Page /Parent 2 0 R >> endobj
trailer << /Root 1 0 R >>
%%EOF";

fn harness() -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("doc.pdf", None, FIXTURE.to_vec()).unwrap();
        app.set_option("left", "closed").unwrap();
        app
    });
    h.run_steps(6);
    h
}

fn crops(h: &Harness<'static, PdfCraftApp>) -> Vec<[f32; 4]> {
    let s = h.state();
    s.session.get(s.views[0].id).unwrap().info.pages.iter().map(|p| p.crop).collect()
}

#[test]
fn divide_pages_cuts_the_spread_right_to_left_and_skips_the_covers() {
    let mut h = harness();
    assert!(h.state_mut().execute("page.divide"));
    h.run_steps(3);
    assert_eq!(h.state().dialog, Some(Dialog::DividePages));
    h.get_by_label("Right to left").click();
    h.get_by_label("Skip first and last page").click();
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(4);
    assert_eq!(h.state().dialog, None);
    assert_eq!(
        crops(&h),
        [[0.0, 0.0, 300.0, 400.0], [300.0, 0.0, 600.0, 400.0], [0.0, 0.0, 300.0, 400.0], [0.0, 0.0, 300.0, 400.0]],
        "the spread's right half comes first; the covers stay whole"
    );
    // One undo step puts the spread back.
    assert!(h.state_mut().execute("edit.undo"));
    h.run_steps(3);
    assert_eq!(crops(&h).len(), 3);
    assert_eq!(crops(&h)[1], [0.0, 0.0, 600.0, 400.0]);
}

#[test]
fn divide_pages_cancel_changes_nothing() {
    let mut h = harness();
    assert!(h.state_mut().execute("page.divide"));
    h.run_steps(3);
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    assert_eq!(crops(&h).len(), 3);
}
