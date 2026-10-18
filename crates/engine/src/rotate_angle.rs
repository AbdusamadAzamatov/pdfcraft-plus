//! Organize ▸ Layout ▸ Rotate by angle (execution plan PL2): the preview the dialog turns live.
//! The edit itself is [`crate::Edit::RotateContent`] (`pdfcraft_organize::rotate_content`).

use pdfcraft_render::{PageRenderer, RenderRequest, RenderedPage};

use crate::{Document, guard};

/// The largest preview side, in pixels.
const MAX_PREVIEW_SIDE: u32 = 1024;

impl Document {
    /// Page `page` (0-based) as it is shown, rendered so that its longer side is about
    /// `max_side` pixels (at most 1024, at most 4 px per point): a still image a dialog can turn
    /// and scale for a live preview without rendering again.
    pub fn page_preview(&self, page: usize, max_side: u32) -> Result<RenderedPage, String> {
        let info = self.info.pages.get(page).ok_or_else(|| format!("page {} does not exist", page.saturating_add(1)))?;
        let side = info.width.max(info.height);
        if !side.is_finite() || side <= 0.0 {
            return Err(format!("page {} has no size", page.saturating_add(1)));
        }
        let scale = (max_side.clamp(16, MAX_PREVIEW_SIDE) as f32 / side).min(4.0);
        let bytes = self.display.clone();
        let config = self.config.clone();
        guard(move || {
            let out = PageRenderer::new(bytes, config).render(RenderRequest { page, scale, ..Default::default() });
            match &out.error {
                Some(e) => Err(e.clone()),
                None => Ok(out),
            }
        })?
    }

    /// Of `pages` (0-based), those that Rotate by angle refuses for an angle that isn't a
    /// multiple of 90° because they have comments or form fields
    /// ([`pdfcraft_organize::page_has_drawn_annotations`]); flattening them first lets them turn
    /// ([`crate::Edit::flatten_and_rotate`]). Pages that don't exist are left out.
    pub fn pages_with_drawn_annotations(&self, pages: &[usize]) -> Vec<usize> {
        let Some(editor) = self.editor.as_ref() else { return Vec::new() };
        let n = self.info.pages.len();
        let pages: Vec<usize> = pages.iter().copied().filter(|p| *p < n).collect();
        pdfcraft_organize::pages_with_drawn_annotations(&editor.cos, &pages).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{DocId, Edit, NewAnnotation, RotateFit, Session, Shape, Style};

    /// A one-page `w`×`h` PDF with a 20 pt red square centred on (cx, cy) and `/Rotate rotate`.
    fn red_square_page(w: u32, h: u32, cx: u32, cy: u32, rotate: i64) -> Vec<u8> {
        let content = format!("1 0 0 rg {} {} 20 20 re f", cx - 10, cy - 10);
        let objs = [
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            format!("<< /Type /Page /Parent 2 0 R /Contents 4 0 R /MediaBox [0 0 {w} {h}] /Rotate {rotate} >>").into_bytes(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes(),
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, obj) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(obj);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
        out
    }

    fn open(bytes: Vec<u8>) -> (Session, DocId) {
        let mut s = Session::new().with_clock(|| 1_700_000_000);
        let id = s.open("square.pdf", None, Arc::new(bytes), None).unwrap();
        (s, id)
    }

    /// The colour shown at user-space point (x, y), rendered at 1 px per point.
    fn shown_at(s: &Session, id: DocId, x: f64, y: f64) -> [u8; 4] {
        let doc = s.get(id).unwrap();
        let info = &doc.info.pages[0];
        let [vx, vy] = info.user_to_view(x as f32, y as f32);
        let out = doc.page_preview(0, info.width.max(info.height).round() as u32).unwrap();
        let k = out.width as f32 / info.width;
        let (px, py) = ((vx * k) as u32, (vy * k) as u32);
        let i = ((py * out.width + px) * 4) as usize;
        out.rgba[i..i + 4].try_into().unwrap()
    }

    fn red(p: [u8; 4]) -> bool {
        p[0] > 200 && p[1] < 60 && p[2] < 60
    }

    fn white(p: [u8; 4]) -> bool {
        p.iter().all(|c| *c > 230)
    }

    /// The square's centre goes where the matrix says, on an upright and on a turned page; on
    /// the upright page that is down and right of the top centre: clockwise, as shown.
    #[test]
    fn a_red_pixel_moves_where_the_maths_says() {
        for rotate in [0, 90] {
            let (mut s, id) = open(red_square_page(200, 200, 100, 170, rotate));
            assert!(red(shown_at(&s, id, 100.0, 170.0)), "the square is drawn before the turn");
            let cos = s.get(id).unwrap().editor.as_ref().unwrap().cos.clone();
            let (m, _) = pdfcraft_organize::rotation_transform(&cos, 0, 45.0, RotateFit::Expand).unwrap();
            s.apply(id, Edit::RotateContent { pages: vec![0], degrees: 45.0, fit: RotateFit::Expand }).unwrap();
            let (x, y) = (m[0] * 100.0 + m[2] * 170.0 + m[4], m[1] * 100.0 + m[3] * 170.0 + m[5]);
            assert!(red(shown_at(&s, id, x, y)), "/Rotate {rotate}: the square moved to ({x:.1}, {y:.1})");
            assert!(white(shown_at(&s, id, 100.0, 170.0)), "/Rotate {rotate}: and left where it was");
            if rotate == 0 {
                assert!(x > 140.0 && y < 160.0, "a clockwise turn as shown: ({x:.1}, {y:.1})");
            }
            let size = s.get(id).unwrap().info.pages[0].width;
            assert!((size - 282.84).abs() < 0.5, "expanded to the turned page: {size}");
        }
    }

    #[test]
    fn a_page_with_a_comment_needs_flatten_and_rotate_which_is_one_undo_step() {
        let (mut s, id) = open(red_square_page(200, 200, 100, 100, 0));
        let comment = NewAnnotation {
            page: 0,
            shape: Shape::Rectangle { rect: [20.0, 20.0, 60.0, 60.0] },
            style: Style { color: [0.0, 0.0, 1.0], opacity: 1.0, width: 2.0, fill: None },
            contents: "Look".into(),
            author: "Reviewer".into(),
        };
        s.apply(id, Edit::AddAnnotation(comment)).unwrap();
        let before = s.get(id).unwrap().bytes.clone();
        let err = s.apply(id, Edit::RotateContent { pages: vec![0], degrees: 10.0, fit: RotateFit::Expand }).unwrap_err();
        assert!(err.to_string().contains("page 1 has comments or form fields; flatten them first or rotate by a multiple of 90°"), "{err}");
        assert_eq!(s.get(id).unwrap().bytes, before, "nothing changed");
        s.apply(id, Edit::flatten_and_rotate(vec![0], 10.0, RotateFit::Expand)).unwrap();
        let doc = s.get(id).unwrap();
        assert_eq!(doc.can_undo(), Some("Flatten comments and rotate"));
        assert!(doc.info.pages[0].width > 200.0, "turned");
        assert!(pdfcraft_annot::summaries(&doc.editor.as_ref().unwrap().cos).is_empty(), "flattened");
        s.undo(id).unwrap();
        assert_eq!(s.get(id).unwrap().bytes, before, "one undo step restores both");
    }

    #[test]
    fn unusable_angles_are_refused_with_the_range() {
        let (mut s, id) = open(red_square_page(200, 200, 100, 100, 0));
        for degrees in [f64::NAN, f64::INFINITY, 200.0] {
            let err = s.apply(id, Edit::RotateContent { pages: vec![0], degrees, fit: RotateFit::Crop }).unwrap_err();
            assert!(err.to_string().contains("-180 to 180"), "{degrees}: {err}");
        }
        // A quarter turn is lossless: only /Rotate changes.
        s.apply(id, Edit::RotateContent { pages: vec![0], degrees: -90.0, fit: RotateFit::Crop }).unwrap();
        assert_eq!(s.get(id).unwrap().info.pages[0].rotation, 270);
        assert_eq!(s.get(id).unwrap().can_undo(), Some("Rotate page"));
    }

    #[test]
    fn previews_refuse_missing_pages_and_cap_their_size() {
        let (s, id) = open(red_square_page(200, 100, 100, 50, 0));
        let doc = s.get(id).unwrap();
        assert!(doc.page_preview(1, 200).is_err());
        let p = doc.page_preview(0, 100_000).unwrap();
        assert!(p.width <= 1024 && p.height <= 512, "{}×{}", p.width, p.height);
    }
}
