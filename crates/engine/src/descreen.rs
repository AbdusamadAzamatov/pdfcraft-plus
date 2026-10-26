//! Remove moiré from a photographed screen or a scanned printed picture (Enhance camera image ▸
//! Remove moiré): the fine interference pattern between the screen's (or the halftone's) grid
//! and the camera's is smoothed away, and the edges of text are sharpened again.
//!
//! The standard descreening recipe, in YCbCr: the brightness is low-pass filtered just enough to
//! remove the pattern, then sharpened with an unsharp mask taken from the filtered image (so the
//! pattern can't come back but letters regain their edges); the colour, where a screen's moiré
//! shows most as rainbow fringes, is filtered more strongly. Each Gaussian is approximated by
//! three box blurs computed with running sums, so the cost doesn't grow with the radius.
//! Pure Rust and deterministic.

/// The box radius, in pixels, the brightness is blurred with: 2 (a Gaussian of about 2.5 pixels,
/// which removes patterns finer than about 6 pixels) up to 6 on large photos, where the pattern
/// is coarser and the letters larger.
fn radius(w: usize, h: usize) -> usize {
    (w.min(h) / 1000).clamp(2, 6)
}

/// [`descreen`]'s result for an RGB8 picture `width` × `height`; `None` when the buffer doesn't
/// match the size.
pub fn descreen(rgb: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || w.checked_mul(h)?.checked_mul(3)? != rgb.len() {
        return None;
    }
    let n = w * h;
    let (mut y, mut cb, mut cr) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    for p in rgb.as_chunks::<3>().0 {
        let (r, g, b) = (f32::from(p[0]), f32::from(p[1]), f32::from(p[2]));
        // ITU-R BT.601 full range, as JPEG uses.
        y.push(to_u8(0.299 * r + 0.587 * g + 0.114 * b));
        cb.push(to_u8(128.0 - 0.168_736 * r - 0.331_264 * g + 0.5 * b));
        cr.push(to_u8(128.0 + 0.5 * r - 0.418_688 * g - 0.081_312 * b));
    }
    let r = radius(w, h);
    // Brightness: smooth the pattern away, then an unsharp mask from a wider blur of the smooth
    // image brings the edges of letters back.
    let smooth = gaussian(&y, w, h, r);
    let wide = gaussian(&smooth, w, h, 3 * r);
    let amount = 1.2f32;
    let y: Vec<u8> = smooth.iter().zip(&wide).map(|(&s, &b)| to_u8(f32::from(s) + amount * (f32::from(s) - f32::from(b)))).collect();
    let cb = gaussian(&cb, w, h, 2 * r);
    let cr = gaussian(&cr, w, h, 2 * r);
    let mut out = Vec::with_capacity(rgb.len());
    for ((&y, &cb), &cr) in y.iter().zip(&cb).zip(&cr) {
        let (y, cb, cr) = (f32::from(y), f32::from(cb) - 128.0, f32::from(cr) - 128.0);
        out.push(to_u8(y + 1.402 * cr));
        out.push(to_u8(y - 0.344_136 * cb - 0.714_136 * cr));
        out.push(to_u8(y + 1.772 * cb));
    }
    Some(out)
}

fn to_u8(v: f32) -> u8 {
    // Clamped first, so the cast is exact (NaN becomes 0).
    v.round().clamp(0.0, 255.0) as u8
}

/// About a Gaussian blur of `plane` (`w` × `h`): three box blurs of radius `r`.
fn gaussian(plane: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    let mut a = plane.to_vec();
    for _ in 0..3 {
        a = box_blur(&a, w, h, r);
    }
    a
}

/// A box blur of radius `r` across the rows, then down the columns. The edges are extended (a
/// pixel beyond the picture is the nearest one in it).
fn box_blur(plane: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    if plane.len() != w.saturating_mul(h) || r == 0 {
        return plane.to_vec();
    }
    let span = (2 * r + 1) as u32;
    let mut across = vec![0u8; plane.len()];
    let mut line = Vec::with_capacity(w.max(h));
    for y in 0..h {
        line.clear();
        line.extend((0..w).map(|x| plane.get(y * w + x).copied().unwrap_or(0)));
        for (x, v) in blur_line(&line, r, span).into_iter().enumerate() {
            if let Some(o) = across.get_mut(y * w + x) {
                *o = v;
            }
        }
    }
    let mut out = vec![0u8; plane.len()];
    for x in 0..w {
        line.clear();
        line.extend((0..h).map(|y| across.get(y * w + x).copied().unwrap_or(0)));
        for (y, v) in blur_line(&line, r, span).into_iter().enumerate() {
            if let Some(o) = out.get_mut(y * w + x) {
                *o = v;
            }
        }
    }
    out
}

/// One line blurred with a running sum over `span` = 2r + 1 values, edges extended.
fn blur_line(line: &[u8], r: usize, span: u32) -> Vec<u8> {
    let n = line.len();
    if n == 0 {
        return Vec::new();
    }
    let at = |i: isize| -> u32 { u32::from(line.get(i.clamp(0, n as isize - 1) as usize).copied().unwrap_or(0)) };
    let r = r as isize;
    let mut sum: u32 = (-r..=r).map(at).sum();
    let mut out = Vec::with_capacity(n);
    for i in 0..n as isize {
        // Rounded division: span is at least 3.
        out.push(((sum + span / 2) / span) as u8);
        sum = sum + at(i + r + 1) - at(i - r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A photographed screen: white paper with dark text bars and a coloured interference
    /// pattern of period about 5 pixels over everything.
    fn screen_photo(w: usize, h: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let text = (y / 24) % 3 == 1 && x > w / 10 && x < 9 * w / 10;
                let base = if text { 30.0 } else { 225.0 };
                let phase = std::f32::consts::TAU * (x as f32 * 0.93 + y as f32 * 0.37) / 5.0;
                for (c, shift) in [0.0f32, 2.1, 4.2].into_iter().enumerate() {
                    let v = base + 28.0 * (phase + shift).sin() * if c == 1 { 0.6 } else { 1.0 };
                    out.push(v.round().clamp(0.0, 255.0) as u8);
                }
            }
        }
        out
    }

    fn spread(img: &[u8], w: usize, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>) -> f32 {
        let vals: Vec<f32> =
            ys.flat_map(|y| xs.clone().map(move |x| (y, x))).flat_map(|(y, x)| img[(y * w + x) * 3..][..3].to_vec()).map(f32::from).collect();
        let mean = vals.iter().sum::<f32>() / vals.len() as f32;
        (vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32).sqrt()
    }

    #[test]
    fn the_pattern_goes_and_the_text_stays() {
        let (w, h) = (600, 300);
        let img = screen_photo(w, h);
        let out = descreen(&img, w as u32, h as u32).unwrap();
        // Paper between the text rows: the pattern's spread falls to a small fraction.
        let (before, after) = (spread(&img, w, 100..500, 2..10), spread(&out, w, 100..500, 2..10));
        assert!(after * 5.0 < before, "paper spread {before} → {after}");
        // Text keeps its contrast with the paper: dark in the middle of a bar, light beside it.
        let lum = |img: &[u8], x: usize, y: usize| img[(y * w + x) * 3..][..3].iter().map(|&v| u32::from(v)).sum::<u32>() / 3;
        assert!(lum(&out, 300, 36) < 70, "text {}", lum(&out, 300, 36));
        assert!(lum(&out, 300, 10) > 200, "paper {}", lum(&out, 300, 10));
        // The edge is still sharp: three pixels either side of a bar's top differ by most of the contrast.
        assert!(lum(&out, 300, 21) > lum(&out, 300, 27) + 120, "{} {}", lum(&out, 300, 21), lum(&out, 300, 27));
    }

    #[test]
    fn a_flat_picture_stays_flat() {
        let img = vec![200u8; 50 * 40 * 3];
        let out = descreen(&img, 50, 40).unwrap();
        assert!(out.iter().all(|&v| v.abs_diff(200) <= 1), "only rounding");
    }

    #[test]
    fn small_and_mismatched_pictures_are_safe() {
        assert_eq!(descreen(&[], 0, 0), None);
        assert_eq!(descreen(&[0; 5], 1, 2), None);
        assert_eq!(descreen(&[10, 20, 30], 1, 1).map(|v| v.len()), Some(3));
        let one_row = vec![128u8; 7 * 3];
        assert_eq!(descreen(&one_row, 7, 1).map(|v| v.len()), Some(21));
        assert_eq!(box_blur(&[1, 2, 3], 2, 2, 1), vec![1, 2, 3], "a mismatched plane is returned as it is");
        assert_eq!(box_blur(&[], 0, 5, 2), Vec::<u8>::new(), "no columns");
        assert_eq!(blur_line(&[], 2, 5), Vec::<u8>::new());
    }
}
