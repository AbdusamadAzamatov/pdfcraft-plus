//! Flatten a curved page (Enhance camera image ▸ Flatten curved page): a book or a folded sheet
//! photographed so its lines of text bend is straightened along its lines.
//!
//! The method follows the text: the picture is cut into vertical strips, and in each strip how
//! much ink each row holds (its projection profile) shows where the lines of text are.
//! Neighbouring strips' profiles are matched to find how far the lines move up or down from one
//! strip to the next, separately in a few horizontal bands (a book page bends differently near
//! its top and its bottom). Smooth curves fitted through those offsets give each column's
//! displacement, which is undone by moving the column's pixels up or down.
//!
//! Only the bending of the lines is undone: the squeezing of the text towards a book's spine is
//! not. A picture without clear lines of text is left as it is. Pure Rust and deterministic.

/// How many vertical strips the lines are followed across.
const STRIPS: usize = 20;
/// How many horizontal bands the bending is measured in, top to bottom.
const BANDS: usize = 4;
/// About how wide, in pixels, the picture is analysed at (rows are summed in groups to match).
const ANALYSIS_WIDTH: usize = 1000;
/// The least share of a strip's window that must be ink for its offset to count.
const MIN_INK: f64 = 0.01;
/// The least normalised correlation for a match between neighbouring strips to count.
const MIN_MATCH: f64 = 0.3;

/// The flattened picture of an RGB8 `width` × `height` photo, or `None` when the buffer doesn't
/// match the size or no clear lines of text bend in it (the caller keeps the photo as it is).
pub fn flatten(rgb: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    if w < STRIPS || h < 4 * BANDS || w.checked_mul(h)?.checked_mul(3)? != rgb.len() {
        return None;
    }
    let field = displacement(rgb, width, height)?;
    Some(remap(rgb, w, h, &field))
}

/// Each band's displacement, in full-resolution pixels, for every column: `field[b][x]` is how
/// far down the content of column x has moved in band b.
fn displacement(rgb: &[u8], width: u32, height: u32) -> Option<Vec<Vec<f64>>> {
    let (w, h) = (width as usize, height as usize);
    // Even out the lighting first, so a shadow doesn't count as ink, then split ink from paper.
    let even = crate::camera_clean::clean(rgb, width, height, 50)?;
    let lum: Vec<u8> =
        even.as_chunks::<3>().0.iter().map(|p| ((u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114) / 1000) as u8).collect();
    let mut hist = [0u64; 256];
    for &v in &lum {
        if let Some(n) = hist.get_mut(usize::from(v)) {
            *n += 1;
        }
    }
    let (threshold, dark, light) = crate::deskew::otsu(&hist)?;
    if light - dark < 60.0 {
        return None;
    }
    // Rows are summed in groups of `step`, so the analysis is about ANALYSIS_WIDTH wide whatever
    // the photo's size.
    let step = w.div_ceil(ANALYSIS_WIDTH).max(1);
    let rows = h / step;
    if rows < 4 * BANDS {
        return None;
    }
    let mut profiles = vec![vec![0.0f64; rows]; STRIPS];
    let mut ink = 0usize;
    for (i, &v) in lum.iter().enumerate() {
        if v <= threshold {
            let (x, y) = (i % w, i / w);
            let strip = (x * STRIPS / w).min(STRIPS - 1);
            if let Some(cell) = profiles.get_mut(strip).and_then(|p| p.get_mut(y / step)) {
                *cell += 1.0;
                ink += 1;
            }
        }
    }
    let share = ink as f64 / (w * h) as f64;
    if !(0.005..=0.4).contains(&share) {
        return None;
    }
    let strip_width = (w / STRIPS) as f64;
    let centre = STRIPS / 2;
    let pitch = line_pitch(profiles.get(centre)?)?;
    // A neighbour's lines move less than half a line: further, they'd match the next line.
    let reach = ((pitch * 2) / 5).max(1);
    let window = rows / BANDS;
    let mut bands: Vec<Option<[f64; 4]>> = Vec::with_capacity(BANDS);
    for b in 0..BANDS {
        // Each band's window reaches halfway into its neighbours.
        let mid = (2 * b + 1) * rows / (2 * BANDS);
        let (y0, y1) = (mid.saturating_sub(window), (mid + window).min(rows));
        let mut offset = [0.0f64; STRIPS];
        let mut known = [false; STRIPS];
        known[centre] = true; // STRIPS / 2 < STRIPS
        let chain = |s: usize, towards: usize| {
            let (a, b) = (profiles.get(s)?, profiles.get(towards)?);
            let inked: f64 = a.get(y0..y1)?.iter().sum();
            if inked < MIN_INK * strip_width * step as f64 * (y1 - y0) as f64 {
                return None;
            }
            best_shift(a, b, y0, y1, reach)
        };
        // From the middle out, each strip against its neighbour towards the middle.
        for s in centre + 1..STRIPS {
            let moved = chain(s, s - 1);
            offset[s] = offset[s - 1] + moved.unwrap_or(0.0);
            known[s] = moved.is_some();
        }
        for s in (0..centre).rev() {
            let moved = chain(s, s + 1);
            offset[s] = offset[s + 1] + moved.unwrap_or(0.0);
            known[s] = moved.is_some();
        }
        let points: Vec<(f64, f64)> = (0..STRIPS).filter(|&s| known[s]).map(|s| (strip_u(s), offset[s])).collect();
        bands.push(if points.len() >= STRIPS / 2 { fit(&points) } else { None });
    }
    // A band without clear lines borrows its nearest neighbour's curve.
    let any = bands.iter().flatten().next().copied()?;
    let filled: Vec<[f64; 4]> = (0..BANDS)
        .map(|b| {
            let near = (0..BANDS).filter_map(|o| bands.get(o).copied().flatten().map(|c| (b.abs_diff(o), c))).min_by_key(|(d, _)| *d);
            near.map_or(any, |(_, c)| c)
        })
        .collect();
    let field: Vec<Vec<f64>> =
        filled.iter().map(|c| (0..w).map(|x| poly(c, (x as f64 + 0.5) / w as f64 * 2.0 - 1.0) * step as f64).collect()).collect();
    // Bent by less than a pixel anywhere: nothing to undo.
    let most = field.iter().flatten().fold(0.0f64, |m, d| m.max(d.abs()));
    (most >= 1.0 && most.is_finite()).then_some(field)
}

/// A strip's centre across the picture, from -1 (left edge) to 1 (right edge).
fn strip_u(s: usize) -> f64 {
    (s as f64 + 0.5) / STRIPS as f64 * 2.0 - 1.0
}

/// The distance, in profile rows, from one line of text to the next: the first strong peak of
/// the profile's autocorrelation. `None` when the profile has no regular lines.
fn line_pitch(profile: &[f64]) -> Option<usize> {
    let n = profile.len();
    let mean = profile.iter().sum::<f64>() / n.max(1) as f64;
    let centred: Vec<f64> = profile.iter().map(|v| v - mean).collect();
    let at = |lag: usize| -> f64 { centred.iter().zip(centred.iter().skip(lag)).map(|(a, b)| a * b).sum() };
    let zero = at(0);
    if zero <= 0.0 {
        return None;
    }
    let corr: Vec<f64> = (0..=n / 3).map(|lag| at(lag) / zero).collect();
    // The first local maximum after the correlation has dipped, and a clear one.
    let dipped = corr.iter().position(|&c| c < 0.0)?;
    (dipped.max(2)..corr.len().saturating_sub(1))
        .find(|&l| corr.get(l).is_some_and(|&c| c > 0.3 && c >= corr[l - 1] && c >= corr[l + 1]))
        .filter(|&l| l >= 3)
}

/// How far down `a` must be read to match `b` over rows `y0..y1`, within ±`reach` rows, to a
/// fraction of a row; `None` when no shift matches well.
fn best_shift(a: &[f64], b: &[f64], y0: usize, y1: usize, reach: usize) -> Option<f64> {
    let score = |t: isize| -> f64 {
        let pairs: Vec<(f64, f64)> = (y0..y1).filter_map(|y| Some((*a.get(usize::try_from(y as isize + t).ok()?)?, *b.get(y)?))).collect();
        if pairs.len() < 8 {
            return f64::NEG_INFINITY;
        }
        let n = pairs.len() as f64;
        let (ma, mb) = (pairs.iter().map(|p| p.0).sum::<f64>() / n, pairs.iter().map(|p| p.1).sum::<f64>() / n);
        let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
        for (x, y) in &pairs {
            ab += (x - ma) * (y - mb);
            aa += (x - ma) * (x - ma);
            bb += (y - mb) * (y - mb);
        }
        if aa <= 0.0 || bb <= 0.0 { f64::NEG_INFINITY } else { ab / (aa * bb).sqrt() }
    };
    let reach = reach as isize;
    let scores: Vec<(isize, f64)> = (-reach..=reach).map(|t| (t, score(t))).collect();
    let &(t, best) = scores.iter().max_by(|x, y| x.1.total_cmp(&y.1))?;
    // NaN (no comparable rows) counts as no match too.
    if best.partial_cmp(&MIN_MATCH).is_none_or(|o| o == std::cmp::Ordering::Less) {
        return None;
    }
    // A parabola through the best score and its neighbours places the peak between rows.
    let around = |d: isize| scores.iter().find(|s| s.0 == t + d).map(|s| s.1).filter(|v| v.is_finite());
    let fine = match (around(-1), around(1)) {
        (Some(l), Some(r)) if l - 2.0 * best + r < 0.0 => 0.5 * (l - r) / (l - 2.0 * best + r),
        _ => 0.0,
    };
    Some(t as f64 + fine.clamp(-0.5, 0.5))
}

/// A cubic (or, with few points, quadratic) least-squares fit through `points` (u, offset):
/// coefficients of 1, u, u², u³.
fn fit(points: &[(f64, f64)]) -> Option<[f64; 4]> {
    let terms = if points.len() >= 6 { 4 } else { 3 };
    // The normal equations, solved by elimination with partial pivoting.
    let mut m = [[0.0f64; 5]; 4];
    for &(u, v) in points {
        let p = [1.0, u, u * u, u * u * u];
        for i in 0..terms {
            for j in 0..terms {
                m[i][j] += p[i] * p[j];
            }
            m[i][4] += p[i] * v;
        }
    }
    for col in 0..terms {
        let pivot = (col..terms).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        m.swap(col, pivot);
        let d = m[col][col];
        if d.abs() < 1e-9 {
            return None;
        }
        for row in 0..terms {
            if row != col {
                let k = m[row][col] / d;
                let pivot_row = m[col];
                for (v, p) in m[row].iter_mut().zip(pivot_row).skip(col) {
                    *v -= k * p;
                }
            }
        }
    }
    let mut out = [0.0; 4];
    for (i, o) in out.iter_mut().enumerate().take(terms) {
        *o = m[i][4] / m[i][i];
    }
    out.iter().all(|c| c.is_finite()).then_some(out)
}

fn poly(c: &[f64; 4], u: f64) -> f64 {
    c[0] + u * (c[1] + u * (c[2] + u * c[3]))
}

/// Move each column's pixels up by its displacement (interpolated between the bands' centres),
/// reading between rows. What would come from beyond the picture's top or bottom repeats its edge
/// row, so the paper keeps its colour there instead of turning into white crescents.
fn remap(rgb: &[u8], w: usize, h: usize, field: &[Vec<f64>]) -> Vec<u8> {
    let bands = field.len().max(1);
    let mut out = vec![255u8; rgb.len()];
    for y in 0..h {
        // Where this row sits between the bands' centres.
        let fb = ((y as f64 + 0.5) / h as f64 * bands as f64 - 0.5).clamp(0.0, (bands - 1) as f64);
        let (b0, a) = (fb.floor() as usize, fb - fb.floor());
        let b1 = (b0 + 1).min(bands - 1);
        for x in 0..w {
            let d0 = field.get(b0).and_then(|f| f.get(x)).copied().unwrap_or(0.0);
            let d1 = field.get(b1).and_then(|f| f.get(x)).copied().unwrap_or(0.0);
            let sy = y as f64 + d0 * (1.0 - a) + d1 * a;
            if !sy.is_finite() {
                continue;
            }
            let sy = sy.clamp(0.0, (h - 1) as f64);
            let (r0, k) = (sy.floor() as usize, sy - sy.floor());
            let r1 = (r0 + 1).min(h - 1);
            let (p, q) = ((r0 * w + x) * 3, (r1 * w + x) * 3);
            for c in 0..3 {
                let (top, bottom) = (rgb.get(p + c).copied().unwrap_or(255), rgb.get(q + c).copied().unwrap_or(255));
                if let Some(o) = out.get_mut((y * w + x) * 3 + c) {
                    *o = (f64::from(top) * (1.0 - k) + f64::from(bottom) * k).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page of straight lines of words (dark bars with gaps), 40 pixels apart, 14 tall, read
    /// through `bend(x, y)`: the pixel at (x, y) shows the flat page at (x, y - bend).
    fn page(w: usize, h: usize, bend: impl Fn(f64, f64) -> f64) -> Vec<u8> {
        let flat = |x: f64, y: f64| -> bool {
            if !(40.0..h as f64 - 40.0).contains(&y) || !(30.0..w as f64 - 30.0).contains(&x) {
                return false;
            }
            let line = (y / 40.0).floor();
            let inside = y - line * 40.0;
            // Words of varying length, different on every line.
            let word = ((x + line * 37.0) / 23.0).floor() as i64;
            inside < 14.0 && (word * 7919 + line as i64 * 104_729) % 5 != 0
        };
        let mut out = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
                let v = if flat(fx, fy - bend(fx, fy)) { 25 } else { 235 };
                out.extend([v, v, v]);
            }
        }
        out
    }

    /// The middle row of ink of one line of text near column `x`, within ±18 rows of `around`.
    fn line_at(img: &[u8], w: usize, x: usize, around: usize) -> f64 {
        let (mut sum, mut n) = (0.0, 0.0);
        for y in around - 18..around + 18 {
            for xx in x..x + 40 {
                if img[(y * w + xx) * 3] < 128 {
                    sum += y as f64;
                    n += 1.0;
                }
            }
        }
        sum / n
    }

    #[test]
    fn a_bent_page_comes_out_with_straight_lines() {
        let (w, h) = (800, 1000);
        // Bent like a book page: most at the outer edges, more near the top, and a slight tilt.
        let bend = |x: f64, y: f64| {
            let u = x / w as f64 * 2.0 - 1.0;
            25.0 * u * u * (1.0 - 0.6 * y / h as f64) + 8.0 * u
        };
        let bent = page(w, h, bend);
        let flat = flatten(&bent, w as u32, h as u32).expect("lines were found");
        // The line drawn at rows 200–214 on the flat page, left, middle and right.
        let spread = |img: &[u8]| {
            let ys: Vec<f64> = [60, 380, 700].iter().map(|&x| line_at(img, w, x, 207)).collect();
            ys.iter().fold(f64::MIN, |a, &b| a.max(b)) - ys.iter().fold(f64::MAX, |a, &b| a.min(b))
        };
        assert!(spread(&bent) > 8.0, "the test page is bent: {}", spread(&bent));
        assert!(spread(&flat) < 3.0, "the line is straight again: {}", spread(&flat));
        // Lower on the page, too.
        let low = |img: &[u8]| (line_at(img, w, 60, 807) - line_at(img, w, 700, 807)).abs();
        assert!(low(&flat) < 3.0, "{} → {}", low(&bent), low(&flat));
    }

    /// Bent edges pull rows from beyond the picture: they repeat the edge row (the paper's colour),
    /// so no white crescents appear at the top and bottom corners of a grey or cream page.
    #[test]
    fn the_corners_keep_the_paper_colour() {
        let (w, h) = (800, 1000);
        let bend = |x: f64, _: f64| {
            let u = x / w as f64 * 2.0 - 1.0;
            30.0 * u * u
        };
        let flat = flatten(&page(w, h, bend), w as u32, h as u32).expect("lines were found");
        for y in [0, 1, 2, h - 3, h - 2, h - 1] {
            for x in (0..w).step_by(7) {
                let v = flat[(y * w + x) * 3];
                assert!(v < 250, "row {y}, column {x} turned white ({v}); the paper is 235");
            }
        }
    }

    #[test]
    fn straight_blank_and_photo_like_pictures_are_left_alone() {
        let (w, h) = (400, 500);
        assert_eq!(flatten(&page(w, h, |_, _| 0.0), w as u32, h as u32), None, "nothing bends");
        assert_eq!(flatten(&vec![240; w * h * 3], w as u32, h as u32), None, "blank");
        // Random specks, no lines (a xorshift generator, so the test is repeatable).
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let noise: Vec<u8> = (0..w * h)
            .flat_map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let v = if seed.is_multiple_of(3) { 20 } else { 230 };
                [v, v, v]
            })
            .collect();
        assert_eq!(flatten(&noise, w as u32, h as u32), None, "no lines of text");
    }

    #[test]
    fn small_and_mismatched_pictures_are_safe() {
        assert_eq!(flatten(&[], 0, 0), None);
        assert_eq!(flatten(&[0; 30], 10, 1), None);
        assert_eq!(flatten(&vec![0; 300 * 3], 30, 10), None, "too short to have bands");
        assert_eq!(flatten(&[0; 5], 100, 100), None);
        assert_eq!(fit(&[(0.0, 1.0), (0.0, 2.0), (0.0, 3.0)]), None, "all at one place: no curve");
        assert_eq!(line_pitch(&[1.0; 50]), None);
        assert_eq!(line_pitch(&[]), None);
    }
}
