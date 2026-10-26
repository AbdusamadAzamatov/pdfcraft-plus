//! Clean up a straightened camera photo (Enhance ▸ Camera image ▸ enhancement level): even out
//! the lighting so the paper turns white (room-light colour casts, a hand's shadow, darker
//! edges go with it), then raise the contrast and drop isolated specks. Colour ink stays colour.
//! Besides that clean-up the page can be lightened, made vivid, inverted (chalk on a board reads
//! as dark ink on white) or turned into ink-saving black and white.
//!
//! The paper's illumination is estimated per channel on a coarse grid of blocks, from each
//! block's bright end (the paper, not the ink), smoothed into a continuous map and divided out.
//! Pure Rust and deterministic.

/// The strongest a level can be.
pub const MAX_LEVEL: u8 = 100;

/// How an enhanced camera page looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    /// The straightened photo as it was taken.
    Original,
    /// Brighter, colours and lighting otherwise as photographed: the level is how much.
    Lighten,
    /// Cleaned up, colour ink kept.
    #[default]
    Colour,
    /// Cleaned up, with colour ink made more vivid.
    Vivid,
    /// Cleaned up, in shades of grey.
    Greyscale,
    /// Black ink on white paper only, each pixel one or the other.
    BlackWhite,
    /// Black and white that saves ink: large solid areas keep only their outline.
    Eco,
    /// The photo's colours inverted, then cleaned up: chalk on a blackboard or white ink on dark
    /// paper becomes dark ink on white.
    Invert,
}

impl Look {
    pub const ALL: [Look; 8] = [Look::Original, Look::Lighten, Look::Colour, Look::Vivid, Look::Greyscale, Look::BlackWhite, Look::Eco, Look::Invert];

    /// The name tools use.
    pub fn name(self) -> &'static str {
        match self {
            Look::Original => "original",
            Look::Lighten => "lighten",
            Look::Colour => "colour",
            Look::Vivid => "vivid",
            Look::Greyscale => "greyscale",
            Look::BlackWhite => "bw",
            Look::Eco => "eco",
            Look::Invert => "invert",
        }
    }

    pub fn from_name(name: &str) -> Option<Look> {
        Look::ALL.into_iter().find(|l| l.name() == name)
    }

    /// Whether every pixel of this look is black or white, so the page is stored losslessly.
    pub fn is_bilevel(self) -> bool {
        matches!(self, Look::BlackWhite | Look::Eco)
    }
}

/// A page's pixels after [`apply`]: RGB8, or one grey byte per pixel.
#[derive(Clone, Debug, PartialEq)]
pub enum Pixels {
    Rgb(Vec<u8>),
    Grey(Vec<u8>),
}

/// Give a straightened photo (`rgb`, RGB8) the `look` at `level`. For black and white (and eco)
/// the level sets the threshold: higher keeps fainter strokes black. `None` when the buffer
/// doesn't match the size.
pub fn apply(rgb: &[u8], width: u32, height: u32, look: Look, level: u8) -> Option<Pixels> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || w.checked_mul(h)?.checked_mul(3)? != rgb.len() {
        return None;
    }
    let lum = |p: &[u8; 3]| ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8;
    Some(match look {
        Look::Original => Pixels::Rgb(rgb.to_vec()),
        Look::Lighten => Pixels::Rgb(lighten(rgb, level)),
        Look::Colour => Pixels::Rgb(clean(rgb, width, height, level)?),
        Look::Vivid => Pixels::Rgb(vivid(&clean(rgb, width, height, level)?, level)),
        Look::Greyscale => Pixels::Grey(clean(rgb, width, height, level)?.as_chunks::<3>().0.iter().map(lum).collect()),
        Look::BlackWhite | Look::Eco => {
            // The lighting evened out fully, then one threshold for the whole page.
            let even = clean(rgb, width, height, 50)?;
            let t = 0.45 + 0.4 * level.min(MAX_LEVEL) as f32 / MAX_LEVEL as f32;
            let mut grey: Vec<u8> = even.as_chunks::<3>().0.iter().map(|p| if (lum(p) as f32 / 255.0) < t { 0 } else { 255 }).collect();
            despeckle_grey(&mut grey, w, h);
            if look == Look::Eco {
                hollow(&mut grey, w, h);
            }
            Pixels::Grey(grey)
        }
        Look::Invert => {
            let negative: Vec<u8> = rgb.iter().map(|&v| 255 - v).collect();
            Pixels::Rgb(clean(&negative, width, height, level)?)
        }
    })
}

/// Brighten `rgb` at `level` (0 = unchanged): a curve that lifts the shadows and middle tones
/// and leaves white white, so the ink stays readable.
fn lighten(rgb: &[u8], level: u8) -> Vec<u8> {
    let k = level.min(MAX_LEVEL) as f32 / MAX_LEVEL as f32;
    // 1 - (1 - v)^p: p = 1 is unchanged, larger p is brighter.
    let p = 1.0 + 2.0 * k;
    let table: [u8; 256] = std::array::from_fn(|v| ((1.0 - (1.0 - v as f32 / 255.0).powf(p)) * 255.0).round().clamp(0.0, 255.0) as u8);
    // A u8 always indexes a 256-entry table.
    rgb.iter().map(|&v| table[v as usize]).collect()
}

/// Make the colours of a cleaned page (`rgb`) more saturated at `level` (0 = unchanged): each
/// pixel moves away from its own grey, so white paper and black ink stay as they are.
fn vivid(rgb: &[u8], level: u8) -> Vec<u8> {
    let gain = 1.0 + 0.8 * level.min(MAX_LEVEL) as f32 / MAX_LEVEL as f32;
    let mut out = Vec::with_capacity(rgb.len());
    for p in rgb.as_chunks::<3>().0 {
        let grey = (p[0] as f32 * 0.299 + p[1] as f32 * 0.587 + p[2] as f32 * 0.114).clamp(0.0, 255.0);
        out.extend(p.iter().map(|&v| (grey + (v as f32 - grey) * gain).round().clamp(0.0, 255.0) as u8));
    }
    out
}

/// The side, in pixels, of the square a black pixel must be surrounded by to count as inside a
/// solid area for [`hollow`]: about a hundredth of the page's shorter side, so text strokes keep
/// their ink and headings, bars and photos lose theirs.
fn hollow_radius(w: usize, h: usize) -> usize {
    (w.min(h) / 200).clamp(2, 32)
}

/// Turn the inside of every solid black area of `grey` (0 or 255 per pixel) white, keeping an
/// outline [`hollow_radius`] thick: strokes thinner than twice that stay as they were.
fn hollow(grey: &mut [u8], w: usize, h: usize) {
    if grey.len() != w.saturating_mul(h) {
        return;
    }
    let r = hollow_radius(w, h);
    let span = 2 * r + 1;
    // A pixel is inside when the square of side `span` centred on it is all black: first along
    // each row (runs of black ending at x mark the centre x - r), then down each column of those.
    let mut across = vec![false; grey.len()];
    for y in 0..h {
        let mut run = 0usize;
        for x in 0..w {
            run = if grey.get(y * w + x) == Some(&0) { run + 1 } else { 0 };
            // A run of `span` ending at x means x >= 2r, so x - r doesn't underflow.
            if run >= span
                && let Some(a) = across.get_mut(y * w + x - r)
            {
                *a = true;
            }
        }
    }
    let mut down = vec![0usize; w];
    for y in 0..h {
        for (x, run) in down.iter_mut().enumerate() {
            *run = if across.get(y * w + x) == Some(&true) { *run + 1 } else { 0 };
            // As above: a run of `span` ending at y means y >= 2r.
            if *run >= span
                && let Some(p) = grey.get_mut((y - r) * w + x)
            {
                *p = 255;
            }
        }
    }
}

/// Clean `rgb` (RGB8, `width` × `height`) at `level` (0 = unchanged … 100 = strongest).
/// Returns `None` when the buffer doesn't match the size.
pub fn clean(rgb: &[u8], width: u32, height: u32, level: u8) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || w.checked_mul(h)?.checked_mul(3)? != rgb.len() {
        return None;
    }
    let level = level.min(MAX_LEVEL);
    if level == 0 {
        return Some(rgb.to_vec());
    }
    let k = level as f32 / MAX_LEVEL as f32;
    let background = background(rgb, w, h);
    // Lighting is evened out fully from half strength on; contrast keeps growing to the top.
    let flatten = (2.0 * k).min(1.0);
    let white = 1.0 - 0.12 * k; // what reaches white
    let black = 0.30 * k; // what reaches black
    let mut out = Vec::with_capacity(rgb.len());
    for (i, px) in rgb.as_chunks::<3>().0.iter().enumerate() {
        let (x, y) = (i % w, i / w);
        let bg = background.at(x, y);
        for (c, &v) in px.iter().enumerate() {
            let v = v as f32 / 255.0;
            let b = bg.get(c).copied().unwrap_or(1.0).max(0.05);
            let even = (v / b).min(1.0);
            let flat = v + (even - v) * flatten;
            let stretched = ((flat - black) / (white - black).max(0.05)).clamp(0.0, 1.0);
            out.push((stretched * 255.0).round() as u8);
        }
    }
    if level >= 60 {
        despeckle(&mut out, w, h);
    }
    Some(out)
}

/// The paper's illumination: a grid of per-channel values, read back with bilinear
/// interpolation.
struct Background {
    cols: usize,
    rows: usize,
    block: f32,
    cells: Vec<[f32; 3]>,
}

impl Background {
    fn cell(&self, cx: isize, cy: isize) -> [f32; 3] {
        let cx = cx.clamp(0, self.cols as isize - 1) as usize;
        let cy = cy.clamp(0, self.rows as isize - 1) as usize;
        self.cells.get(cy * self.cols + cx).copied().unwrap_or([1.0; 3])
    }

    fn at(&self, x: usize, y: usize) -> [f32; 3] {
        // Cell centres sit at (i + 0.5) × block.
        let fx = (x as f32 + 0.5) / self.block - 0.5;
        let fy = (y as f32 + 0.5) / self.block - 0.5;
        let (x0, y0) = (fx.floor(), fy.floor());
        let (ax, ay) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let (a, b, c, d) = (self.cell(x0, y0), self.cell(x0 + 1, y0), self.cell(x0, y0 + 1), self.cell(x0 + 1, y0 + 1));
        std::array::from_fn(|i| {
            let top = a[i] * (1.0 - ax) + b[i] * ax;
            let bottom = c[i] * (1.0 - ax) + d[i] * ax;
            top * (1.0 - ay) + bottom * ay
        })
    }
}

/// Estimate the paper's illumination: per block, the 90th percentile of each channel (bright
/// paper, not dark ink); blocks that are mostly ink borrow from their neighbours; then smoothed.
fn background(rgb: &[u8], w: usize, h: usize) -> Background {
    let block = (w.min(h) / 16).clamp(8, 128);
    let (cols, rows) = (w.div_ceil(block), h.div_ceil(block));
    let mut cells = Vec::with_capacity(cols * rows);
    let mut hist = [[0u32; 256]; 3];
    for by in 0..rows {
        for bx in 0..cols {
            hist.iter_mut().for_each(|hh| hh.fill(0));
            let mut n = 0u32;
            for y in by * block..((by + 1) * block).min(h) {
                for x in bx * block..((bx + 1) * block).min(w) {
                    if let Some(p) = rgb.get((y * w + x) * 3..(y * w + x) * 3 + 3) {
                        for (c, &v) in p.iter().enumerate() {
                            if let Some(slot) = hist.get_mut(c).and_then(|hh| hh.get_mut(v as usize)) {
                                *slot += 1;
                            }
                        }
                        n += 1;
                    }
                }
            }
            let want = (n as f32 * 0.9) as u32;
            cells.push(std::array::from_fn(|c| {
                let mut seen = 0;
                let v = hist.get(c).and_then(|hh| {
                    hh.iter().position(|&count| {
                        seen += count;
                        seen > want
                    })
                });
                v.unwrap_or(255) as f32 / 255.0
            }));
        }
    }
    // A block darker than its neighbourhood's paper is mostly ink, a stamp or a picture: take
    // the brighter of itself and the 75th percentile of the 5 × 5 blocks around it (so a
    // coloured area up to about half that neighbourhood isn't taken for paper), then smooth.
    let get = |cells: &Vec<[f32; 3]>, x: isize, y: isize| -> [f32; 3] {
        let x = x.clamp(0, cols as isize - 1) as usize;
        let y = y.clamp(0, rows as isize - 1) as usize;
        cells.get(y * cols + x).copied().unwrap_or([1.0; 3])
    };
    let mut lifted = Vec::with_capacity(cells.len());
    for y in 0..rows as isize {
        for x in 0..cols as isize {
            let own = get(&cells, x, y);
            lifted.push(std::array::from_fn(|c| {
                let mut around: Vec<f32> =
                    (-2..=2).flat_map(|dy| (-2..=2).map(move |dx| (dx, dy))).map(|(dx, dy)| get(&cells, x + dx, y + dy)[c]).collect();
                around.sort_by(f32::total_cmp);
                let paper = around.get(18).copied().unwrap_or(1.0);
                own[c].max(paper)
            }));
        }
    }
    let mut cells = lifted;
    for _ in 0..2 {
        let mut next = Vec::with_capacity(cells.len());
        for y in 0..rows as isize {
            for x in 0..cols as isize {
                let mut sum = [0.0f32; 3];
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let v = get(&cells, x + dx, y + dy);
                        for c in 0..3 {
                            sum[c] += v[c] / 9.0;
                        }
                    }
                }
                next.push(sum);
            }
        }
        cells = next;
    }
    Background { cols, rows, block: block as f32, cells }
}

/// [`despeckle`] for one grey byte per pixel.
fn despeckle_grey(grey: &mut [u8], w: usize, h: usize) {
    let mut rgb: Vec<u8> = grey.iter().flat_map(|&v| [v, v, v]).collect();
    despeckle(&mut rgb, w, h);
    for (g, p) in grey.iter_mut().zip(rgb.as_chunks::<3>().0.iter()) {
        *g = p[0];
    }
}

/// Lighten dark pixels with at most one dark neighbour (specks), in place.
fn despeckle(rgb: &mut [u8], w: usize, h: usize) {
    let lum = |p: &[u8; 3]| (p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000;
    let dark: Vec<bool> = rgb.as_chunks::<3>().0.iter().map(|p| lum(p) < 128).collect();
    let is_dark = |x: isize, y: isize| {
        x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && dark.get(y as usize * w + x as usize).copied().unwrap_or(false)
    };
    for y in 0..h as isize {
        for x in 0..w as isize {
            if !is_dark(x, y) {
                continue;
            }
            let neighbours =
                (-1..=1).flat_map(|dy| (-1..=1).map(move |dx| (dx, dy))).filter(|&(dx, dy)| (dx, dy) != (0, 0) && is_dark(x + dx, y + dy)).count();
            if neighbours <= 1
                && let Some(p) = rgb.get_mut((y as usize * w + x as usize) * 3..(y as usize * w + x as usize) * 3 + 3)
            {
                p.fill(255);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic photo of a page: paper lit by `light(x, y)` (per channel, 0–1), with dark
    /// text bars, a blue stamp and a few specks.
    fn photo(w: usize, h: usize, light: impl Fn(usize, usize) -> [f32; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let text = (y / 12) % 3 == 1 && x > w / 10 && x < 9 * w / 10 && (x / 9) % 6 != 0;
                let stamp = (x as isize - (3 * w / 4) as isize).pow(2) + (y as isize - (3 * h / 4) as isize).pow(2) < (h as isize / 12).pow(2);
                let speck = (x * 7919 + y * 104729) % 997 == 0;
                let ink: [f32; 3] = if text || speck {
                    [0.12, 0.12, 0.12]
                } else if stamp {
                    [0.15, 0.25, 0.75]
                } else {
                    [0.97, 0.97, 0.95]
                };
                let l = light(x, y);
                for c in 0..3 {
                    out.push((ink[c] * l[c] * 255.0).round().clamp(0.0, 255.0) as u8);
                }
            }
        }
        out
    }

    fn px(img: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
        let i = (y * w + x) * 3;
        [img[i], img[i + 1], img[i + 2]]
    }

    /// A paper point (between text rows, away from the stamp).
    fn paper_points(w: usize, h: usize) -> Vec<(usize, usize)> {
        [(0.05, 0.03), (0.5, 0.03), (0.95, 0.03), (0.05, 0.53), (0.5, 0.53), (0.97, 0.98), (0.03, 0.98)]
            .iter()
            .map(|&(fx, fy)| ((fx * w as f64) as usize, (fy * h as f64) as usize))
            .map(|(x, y)| (x.min(w - 1), (y - y % 36 + 4).min(h - 1)))
            .collect()
    }

    #[test]
    fn a_shadow_and_a_yellow_cast_become_white_paper() {
        let (w, h) = (480, 640);
        // Warm room light, falling off to a third towards the bottom right (a hand's shadow).
        let light = |x: usize, y: usize| {
            let f = 1.0 - 0.66 * ((x + y) as f32 / (w + h) as f32);
            [0.95 * f, 0.85 * f, 0.6 * f]
        };
        let img = photo(w, h, light);
        let out = clean(&img, w as u32, h as u32, 60).unwrap();
        for (x, y) in paper_points(w, h) {
            let before = px(&img, w, x, y);
            let after = px(&out, w, x, y);
            assert!(after.iter().all(|&v| v >= 235), "({x}, {y}) {before:?} → {after:?}");
        }
        // The text stays dark everywhere, also in the shadow.
        for (x, y) in [(w / 2, 13), (w / 2, 3 * 12 + 13), (w * 8 / 10, h - 40 - (h - 40) % 36 + 13)] {
            let after = px(&out, w, x, y);
            if (x / 9) % 6 != 0 {
                assert!(after.iter().all(|&v| v <= 90), "text at ({x}, {y}): {after:?}");
            }
        }
    }

    #[test]
    fn a_blue_stamp_stays_blue() {
        let (w, h) = (480, 640);
        let img = photo(w, h, |_, _| [0.9, 0.85, 0.7]);
        let out = clean(&img, w as u32, h as u32, 100).unwrap();
        // The stamp's centre, on a row without text.
        let [r, g, b] = px(&out, w, 3 * w / 4, 3 * h / 4 + 12);
        assert!(b as i32 - r as i32 > 60 && b as i32 - g as i32 > 40, "{:?}", [r, g, b]);
    }

    #[test]
    fn level_zero_changes_nothing_and_strength_grows_with_the_level() {
        let (w, h) = (240, 320);
        let img = photo(w, h, |x, _| [0.6 + 0.3 * x as f32 / w as f32; 3]);
        assert_eq!(clean(&img, w as u32, h as u32, 0).unwrap(), img);
        let mean = |v: &[u8]| v.iter().map(|&c| c as f64).sum::<f64>() / v.len() as f64;
        let (a, b, c) = (mean(&img), mean(&clean(&img, w as u32, h as u32, 30).unwrap()), mean(&clean(&img, w as u32, h as u32, 100).unwrap()));
        assert!(a < b && b < c, "{a} {b} {c}");
    }

    #[test]
    fn specks_go_at_high_levels_but_text_stays() {
        let (w, h) = (240, 320);
        let img = photo(w, h, |_, _| [1.0; 3]);
        let out = clean(&img, w as u32, h as u32, 100).unwrap();
        let specks: Vec<(usize, usize)> = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            // Specks away from the text rows (one touching a letter isn't isolated).
            .filter(|&(x, y)| (x * 7919 + y * 104729) % 997 == 0 && (y / 12) % 3 == 0 && (2..10).contains(&(y % 12)))
            .collect();
        assert!(!specks.is_empty());
        for (x, y) in specks {
            assert!(px(&out, w, x, y).iter().all(|&v| v > 200), "speck at ({x}, {y})");
        }
        assert!(px(&out, w, w / 2 + 1, 13).iter().all(|&v| v < 60), "text");
    }

    #[test]
    fn the_looks() {
        let (w, h) = (240, 320);
        let img = photo(w, h, |x, _| [0.95 - 0.4 * x as f32 / w as f32, 0.85 - 0.4 * x as f32 / w as f32, 0.65]);
        assert_eq!(apply(&img, w as u32, h as u32, Look::Original, 80), Some(Pixels::Rgb(img.clone())));
        let Some(Pixels::Rgb(colour)) = apply(&img, w as u32, h as u32, Look::Colour, 80) else { panic!("colour") };
        assert_eq!(Some(colour), clean(&img, w as u32, h as u32, 80));
        let Some(Pixels::Grey(grey)) = apply(&img, w as u32, h as u32, Look::Greyscale, 80) else { panic!("grey") };
        assert_eq!(grey.len(), w * h);
        let Some(Pixels::Grey(bw)) = apply(&img, w as u32, h as u32, Look::BlackWhite, 50) else { panic!("bw") };
        assert!(bw.iter().all(|&v| v == 0 || v == 255), "only black and white");
        // Paper (between rows, across the shadow) is white; text is black.
        for x in [5, w / 2, w - 6] {
            assert_eq!(bw[3 * w + x], 255, "paper at x {x}");
        }
        assert_eq!(bw[13 * w + w / 2 + 1], 0, "text");
        // The level moves the threshold: more black at the top than at the bottom.
        let black = |level| match apply(&img, w as u32, h as u32, Look::BlackWhite, level) {
            Some(Pixels::Grey(g)) => g.iter().filter(|&&v| v == 0).count(),
            _ => 0,
        };
        assert!(black(0) <= black(50) && black(50) <= black(100), "{} {} {}", black(0), black(50), black(100));
        for look in Look::ALL {
            assert_eq!(Look::from_name(look.name()), Some(look));
        }
        assert_eq!(apply(&[0; 5], 2, 1, Look::BlackWhite, 50), None);
    }

    #[test]
    fn lighten_brightens_the_ink_and_keeps_white_white() {
        let (w, h) = (120, 120);
        let img = photo(w, h, |_, _| [0.8, 0.8, 0.8]);
        let Some(Pixels::Rgb(light)) = apply(&img, w as u32, h as u32, Look::Lighten, 80) else { panic!("lighten") };
        let text = (w / 2 + 1, 13);
        assert!(px(&light, w, text.0, text.1)[0] > px(&img, w, text.0, text.1)[0] + 20, "ink is lighter");
        assert!(light.iter().zip(&img).all(|(a, b)| a >= b), "nothing darker");
        assert_eq!(lighten(&[255, 0, 128], 100)[..2], [255, 0], "white and black stay");
        assert_eq!(apply(&img, w as u32, h as u32, Look::Lighten, 0), Some(Pixels::Rgb(img.clone())), "level 0 is unchanged");
    }

    #[test]
    fn vivid_saturates_the_stamp_and_keeps_paper_and_ink_neutral() {
        let (w, h) = (240, 320);
        let img = photo(w, h, |_, _| [0.9, 0.9, 0.9]);
        let Some(Pixels::Rgb(colour)) = apply(&img, w as u32, h as u32, Look::Colour, 60) else { panic!("colour") };
        let Some(Pixels::Rgb(vivid)) = apply(&img, w as u32, h as u32, Look::Vivid, 60) else { panic!("vivid") };
        let chroma = |p: [u8; 3]| p.iter().max().unwrap() - p.iter().min().unwrap();
        let stamp = (3 * w / 4, 3 * h / 4);
        assert!(chroma(px(&vivid, w, stamp.0, stamp.1)) > chroma(px(&colour, w, stamp.0, stamp.1)) + 10, "the stamp is bluer");
        for (x, y) in paper_points(w, h) {
            assert_eq!(px(&vivid, w, x, y), px(&colour, w, x, y), "grey paper at ({x}, {y}) is not tinted");
        }
    }

    #[test]
    fn eco_hollows_solid_areas_and_keeps_text_strokes() {
        // A thin stroke and a large solid block on white.
        let (w, h) = (400, 400);
        let mut img = vec![245u8; w * h * 3];
        let mut paint = |x0: usize, y0: usize, x1: usize, y1: usize| {
            for y in y0..y1 {
                for x in x0..x1 {
                    img[(y * w + x) * 3..][..3].fill(20);
                }
            }
        };
        paint(20, 20, 380, 23); // a 3-pixel stroke
        // An 80-pixel block: larger, and the lighting estimate would take its middle for paper.
        paint(160, 160, 240, 240);
        let Some(Pixels::Grey(bw)) = apply(&img, w as u32, h as u32, Look::BlackWhite, 50) else { panic!("bw") };
        let Some(Pixels::Grey(eco)) = apply(&img, w as u32, h as u32, Look::Eco, 50) else { panic!("eco") };
        assert!(eco.iter().all(|&v| v == 0 || v == 255), "only black and white");
        assert!(Look::Eco.is_bilevel() && Look::BlackWhite.is_bilevel() && !Look::Greyscale.is_bilevel());
        let at = |g: &[u8], x: usize, y: usize| g[y * w + x];
        assert_eq!((at(&bw, 200, 21), at(&eco, 200, 21)), (0, 0), "the stroke keeps its ink");
        assert_eq!((at(&bw, 200, 200), at(&eco, 200, 200)), (0, 255), "the block's inside is white");
        assert_eq!(at(&eco, 161, 200), 0, "the block keeps its outline");
        let ink = |g: &[u8]| g.iter().filter(|&&v| v == 0).count();
        assert!(ink(&eco) * 3 < ink(&bw), "most of the ink is saved: {} of {}", ink(&eco), ink(&bw));
    }

    #[test]
    fn invert_turns_a_blackboard_into_dark_writing_on_white() {
        // Light chalk strokes on a dark, unevenly lit board.
        let (w, h) = (240, 240);
        let mut img = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let chalk = (y / 12) % 3 == 1 && x > 20 && x < 220;
                let board = 40.0 + 30.0 * x as f32 / w as f32;
                let v = if chalk { 230.0 } else { board };
                img.extend([v as u8, (v + 10.0) as u8, v as u8]);
            }
        }
        let Some(Pixels::Rgb(out)) = apply(&img, w as u32, h as u32, Look::Invert, 60) else { panic!("invert") };
        let lum = |p: [u8; 3]| p.iter().map(|&v| v as u32).sum::<u32>() / 3;
        for x in [10, w / 2, w - 10] {
            assert!(lum(px(&out, w, x, 4)) > 225, "the board is white at x {x}: {:?}", px(&out, w, x, 4));
        }
        assert!(lum(px(&out, w, w / 2, 13)) < 60, "the chalk is dark");
        assert_eq!(apply(&[0, 255, 10], 1, 1, Look::Invert, 0), Some(Pixels::Rgb(vec![255, 0, 245])), "level 0 only inverts");
    }

    #[test]
    fn hollow_leaves_small_and_mismatched_rasters_alone() {
        let mut tiny = vec![0u8; 4];
        hollow(&mut tiny, 2, 2);
        assert_eq!(tiny, vec![0; 4], "smaller than the square");
        let mut wrong = vec![0u8; 3];
        hollow(&mut wrong, 2, 2);
        assert_eq!(wrong, vec![0; 3]);
        hollow(&mut [], 0, 0);
    }

    #[test]
    fn bad_sizes_are_none_not_a_panic() {
        assert_eq!(clean(&[], 0, 0, 50), None);
        assert_eq!(clean(&[0; 12], 2, 3, 50), None);
        assert_eq!(clean(&[9; 3], 1, 1, 255).map(|v| v.len()), Some(3));
    }
}
