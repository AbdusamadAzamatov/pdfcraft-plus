//! Organize ▸ Layout ▸ Change page size (execution plan PL7): the paper presets it offers and
//! which pages already have the target size. The edit itself is [`crate::Edit::ResizePages`]
//! (`pdfcraft_organize::resize_pages`).
//!
//! The paper sizes come from [`pdfcraft_print::PAPERS`]: `organize` can't depend on `print`
//! (both are L4), so the engine looks a preset up and passes plain points down.

use pdfcraft_organize::SizeOrientation;
use pdfcraft_print::PAPERS;

use crate::Document;

/// The [`PAPERS`] names Change page size offers, in the order it lists them.
pub const RESIZE_PAPERS: [&str; 6] = ["A3", "A4", "A5", "US Letter", "US Legal", "Tabloid"];

/// A paper preset's portrait size in points, by its [`PAPERS`] name or short name, ignoring
/// case: "A4", "Letter" or "US Letter", "Legal", "Tabloid", "ARCH C" …
pub fn paper_size(name: &str) -> Option<(f64, f64)> {
    let name = name.trim();
    PAPERS
        .iter()
        .find(|(n, _)| {
            n.eq_ignore_ascii_case(name)
                || n.strip_prefix("US ").is_some_and(|short| short.eq_ignore_ascii_case(name))
                || n.split(" (").next().is_some_and(|short| short.eq_ignore_ascii_case(name))
        })
        .map(|(_, size)| *size)
}

impl Document {
    /// Of `pages` (0-based), in the order given, those whose shown size differs from
    /// `width` × `height` points (turned as `orientation` says) by more than half a point on a
    /// side: what "only pages that differ" resizes. Pages that don't exist are left out; an
    /// unusable size is an error that says what is allowed.
    pub fn pages_of_other_size(&self, pages: &[usize], width: f64, height: f64, orientation: SizeOrientation) -> Result<Vec<usize>, String> {
        let editor = self.editor.as_ref().ok_or_else(|| "this document can't be edited".to_string())?;
        let n = self.info.pages.len();
        let pages: Vec<usize> = pages.iter().copied().filter(|p| *p < n).collect();
        pdfcraft_organize::pages_of_other_size(&editor.cos, &pages, width, height, orientation).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{DocId, Edit, ResizeMode, Session, SizeOrientation};

    use super::*;

    /// A one-page PDF of `w`×`h` pt with blocks of colour across it (and `/Rotate rotate`).
    fn blocks_page(w: u32, h: u32, rotate: i64) -> Vec<u8> {
        let content = format!(
            "1 0 0 rg 0 0 {hw} {hh} re f 0 0 1 rg {hw} {hh} {hw} {hh} re f 0 0.6 0 rg 60 60 120 80 re f 0 0 0 rg {qx} {qy} 40 200 re f",
            hw = w / 2,
            hh = h / 2,
            qx = w * 3 / 4,
            qy = h / 8
        );
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
        let id = s.open("blocks.pdf", None, Arc::new(bytes), None).unwrap();
        (s, id)
    }

    /// The page as shown, at 1 px per point: (width, height, rgba).
    fn render(s: &Session, id: DocId) -> (u32, u32, Vec<u8>) {
        let doc = s.get(id).unwrap();
        let info = &doc.info.pages[0];
        let out = doc.page_preview(0, info.width.max(info.height).round() as u32).unwrap();
        (out.width, out.height, out.rgba.to_vec())
    }

    fn pixel(img: &(u32, u32, Vec<u8>), x: f64, y: f64) -> Option<[u8; 3]> {
        let (w, h, rgba) = img;
        if x < 0.0 || y < 0.0 || x >= f64::from(*w) || y >= f64::from(*h) {
            return None;
        }
        let i = ((y as u32 * w + x as u32) * 4) as usize;
        Some([rgba[i], rgba[i + 1], rgba[i + 2]])
    }

    fn near(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 40)
    }

    /// Letter made A4 (Fit), on an upright and on a turned page: the page after, scaled back
    /// by the fit factor and offset by the margins, is the same picture as before.
    #[test]
    fn renders_before_and_after_scaled_are_the_same_picture() {
        for rotate in [0, 90] {
            let (w, h) = if rotate == 0 { (612, 792) } else { (792, 612) };
            let (mut s, id) = open(blocks_page(w, h, rotate));
            let before = render(&s, id);
            let (aw, ah) = paper_size("A4").unwrap();
            s.apply(id, Edit::ResizePages { pages: vec![0], width: aw, height: ah, orientation: SizeOrientation::Match, mode: ResizeMode::Fit })
                .unwrap();
            let info = s.get(id).unwrap().info.pages[0].clone();
            assert!((info.width as f64 - aw).abs() < 0.01 && (info.height as f64 - ah).abs() < 0.01, "/Rotate {rotate}: shown A4 portrait");
            let after = render(&s, id);
            // As shown, Letter fits A4 by its width: the scale is 595.28 / 612, centred vertically.
            let k = aw / 612.0;
            let top = (ah - 792.0 * k) / 2.0;
            let (mut same, mut total) = (0, 0);
            for y in (2..790).step_by(4) {
                for x in (2..610).step_by(4) {
                    let a = pixel(&before, f64::from(x), f64::from(y)).unwrap();
                    let b = pixel(&after, f64::from(x) * k, f64::from(y) * k + top).unwrap();
                    total += 1;
                    same += usize::from(near(a, b));
                }
            }
            assert!(same * 100 >= total * 98, "/Rotate {rotate}: {same} of {total} samples match");
            // The margins above and below are blank paper.
            assert!(near(pixel(&after, 300.0, top / 2.0).unwrap(), [255, 255, 255]), "/Rotate {rotate}: top margin");
            assert!(near(pixel(&after, 300.0, ah - top / 2.0).unwrap(), [255, 255, 255]), "/Rotate {rotate}: bottom margin");
            assert_eq!(s.get(id).unwrap().can_undo(), Some("Change page size"));
        }
    }

    #[test]
    fn presets_come_from_the_print_papers() {
        assert_eq!(paper_size("A4"), Some((595.28, 841.89)));
        assert_eq!(paper_size("letter"), Some((612.0, 792.0)));
        assert_eq!(paper_size("US Legal"), Some((612.0, 1008.0)));
        assert_eq!(paper_size(" Tabloid "), Some((792.0, 1224.0)));
        assert_eq!(paper_size("ARCH C"), Some((1296.0, 1728.0)));
        assert_eq!(paper_size("B5"), None);
        assert!(RESIZE_PAPERS.iter().all(|p| paper_size(p).is_some()), "every offered preset exists");
    }

    #[test]
    fn only_pages_of_another_size_and_refusals() {
        let (mut s, id) = open(blocks_page(612, 792, 0));
        let doc = s.get(id).unwrap();
        assert_eq!(doc.pages_of_other_size(&[0, 5], 595.28, 841.89, SizeOrientation::Match), Ok(vec![0]), "missing pages are left out");
        assert_eq!(doc.pages_of_other_size(&[0], 792.0, 612.0, SizeOrientation::Match), Ok(vec![]), "Letter matches Letter either way round");
        assert!(doc.pages_of_other_size(&[0], f64::NAN, 100.0, SizeOrientation::Match).unwrap_err().contains("from 1 to 14400 pt"));
        let before = doc.bytes.clone();
        let err = s
            .apply(id, Edit::ResizePages { pages: vec![0], width: 0.0, height: 100.0, orientation: SizeOrientation::Match, mode: ResizeMode::Fill })
            .unwrap_err();
        assert!(err.to_string().contains("from 1 to 14400 pt"), "{err}");
        assert_eq!(s.get(id).unwrap().bytes, before, "nothing changed");
        // Undo restores the page.
        s.apply(
            id,
            Edit::ResizePages { pages: vec![0], width: 300.0, height: 300.0, orientation: SizeOrientation::Match, mode: ResizeMode::NoScale },
        )
        .unwrap();
        assert_eq!(s.get(id).unwrap().info.pages[0].width, 300.0);
        s.undo(id).unwrap();
        assert_eq!(s.get(id).unwrap().bytes, before);
    }
}
