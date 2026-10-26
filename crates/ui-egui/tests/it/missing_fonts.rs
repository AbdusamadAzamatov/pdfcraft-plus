//! PdfCraft Plus: Edit a PDF ▸ Fonts names a paragraph's partly embedded fonts and offers the
//! free look-alike for download (see `font_suggest`).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::PdfCraftApp;

/// One page with a line in a Times New Roman subset (`ABCDEF+TimesNewRoman`) at (72, 700); the
/// font isn't embedded, so the PDF doesn't include it at all.
fn subset_times_pdf() -> Vec<u8> {
    let body = "BT /F1 24 Tf 72 700 Td (Hello) Tj ET";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()),
        "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+TimesNewRoman /Encoding /WinAnsiEncoding >>".to_string(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn harness(setup: impl FnOnce(&mut PdfCraftApp) + 'static) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 1000.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("doc.pdf", None, subset_times_pdf()).unwrap();
        setup(&mut app);
        app
    });
    h.run_steps(4);
    h
}

/// Edit text & images, then a click on "Hello" opens it in the paragraph editor.
fn open_the_line(h: &mut Harness<'static, PdfCraftApp>) {
    assert!(h.state_mut().execute("edit.edit_text"));
    h.run_steps(3);
    let r = h.state().views[0].page_screen_rect(0).expect("on screen");
    let at = egui::pos2(r.left() + 90.0 / 612.0 * r.width(), r.top() + (792.0 - 708.0) / 792.0 * r.height());
    h.hover_at(at);
    h.run_steps(1);
    h.drag_at(at);
    h.run_steps(1);
    h.drop_at(at);
    h.run_steps(3);
    assert_eq!(h.state().views[0].line_editor.as_ref().expect("the line opens").text, "Hello");
}

#[test]
fn a_partly_embedded_font_offers_its_free_look_alike_and_reports_a_failed_download() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let mut h = harness(move |app| {
        app.font_fetch = Some(std::sync::Arc::new(move |_file| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("offline".to_string())
        }));
    });
    open_the_line(&mut h);
    h.get_by_label("Times New Roman");
    h.get_by_label_contains("doesn't include this font, so edited text would show in another font");
    h.get_by_label_contains("Download Tinos").click();
    // The download runs on its own thread; give it a moment.
    for _ in 0..50 {
        h.run_steps(1);
        if h.query_by_label_contains("The download failed").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    h.get_by_label_contains("The download failed: offline");
    assert!(calls.load(std::sync::atomic::Ordering::SeqCst) >= 1, "the app's download ran");
    assert!(h.query_by_label_contains("Download Tinos").is_some(), "offered again after a failure");
}

#[test]
fn without_a_download_the_panel_points_to_load_font() {
    let mut h = harness(|_| {});
    open_the_line(&mut h);
    h.get_by_label_contains("Load the free look-alike Tinos with Load font");
    assert!(h.query_by_label_contains("Download Tinos").is_none());
}
