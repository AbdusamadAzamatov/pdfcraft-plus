//! Free look-alike faces for common proprietary fonts (PdfCraft Plus).
//!
//! Most PDFs embed only the glyphs they use (a subset), so a letter the text didn't have can't
//! be drawn in the paragraph's own face. For the fonts office documents use most, an openly
//! licensed face exists with the same advance widths: Tinos for Times New Roman, Arimo for Arial
//! and Helvetica, Cousine for Courier New, Carlito for Calibri and Caladea for Cambria. Once
//! one is installed, new letters in such a paragraph are drawn with it instead of a fallback of
//! another design, and the paragraph keeps its widths.
//!
//! Nothing is fetched here: this crate has no network code. The catalogue says where each file
//! is (a GitHub URL pinned to a commit of google/fonts) and its SHA-256, the app downloads it
//! when the user asks, and [`install`] accepts only bytes whose digest is in the catalogue. An
//! installed face is used for editing only ([`crate::embedding::editing_faces`]): it never
//! replaces a font for display.

use std::sync::{Mutex, OnceLock, PoisonError};

use sha2::{Digest, Sha256};

use crate::CraftFont;

/// One downloadable file of a look-alike family.
#[derive(Debug, PartialEq, Eq)]
pub struct LookalikeFile {
    /// `Regular`, `Bold`, `Italic` or `BoldItalic`.
    pub style: &'static str,
    /// The file name it is saved under, such as `Tinos-Bold.ttf`.
    pub file: &'static str,
    /// Where to download it.
    pub url: &'static str,
    /// Its SHA-256, lowercase hex.
    pub sha256: &'static str,
    /// Its size in bytes.
    pub size: u32,
}

/// A look-alike family and the proprietary families it stands in for.
#[derive(Debug, PartialEq, Eq)]
pub struct Lookalike {
    pub family: &'static str,
    /// The families it matches, as shown to the user.
    pub replaces: &'static [&'static str],
    /// Prefixes of [`crate::embedding::face_key`] that name those families (longest first).
    keys: &'static [&'static str],
    /// SPDX identifier.
    pub licence: &'static str,
    pub files: &'static [LookalikeFile],
}

impl Lookalike {
    /// The file for a weight and slant.
    pub fn file(&self, bold: bool, italic: bool) -> Option<&'static LookalikeFile> {
        let style = match (bold, italic) {
            (false, false) => "Regular",
            (true, false) => "Bold",
            (false, true) => "Italic",
            (true, true) => "BoldItalic",
        };
        self.files.iter().find(|f| f.style == style)
    }

    /// The download size of every file of the family.
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| u64::from(f.size)).sum()
    }
}

/// A file of google/fonts at the commit the catalogue was checked against.
macro_rules! google_fonts {
    ($path:literal) => {
        concat!("https://raw.githubusercontent.com/google/fonts/e7db6ca603fd4862010383ff17ffd6cc4ffac1c0/", $path)
    };
}

/// Arimo's last static (Apache-2.0) release in google/fonts; later commits ship only a variable
/// font, which can't be embedded for editing.
macro_rules! arimo_static {
    ($file:literal) => {
        concat!("https://raw.githubusercontent.com/google/fonts/7252eca1fac7ed118f81c4ba5d520f6f40c22860/apache/arimo/static/", $file)
    };
}

/// Every look-alike family PdfCraft Plus offers.
pub static CATALOGUE: &[Lookalike] = &[
    Lookalike {
        family: "Tinos",
        replaces: &["Times New Roman", "Times"],
        keys: &["timesnewromanps", "timesnewroman", "timesroman", "times", "liberationserif", "tinos"],
        licence: "OFL-1.1",
        files: &[
            LookalikeFile {
                style: "Regular",
                file: "Tinos-Regular.ttf",
                url: google_fonts!("ofl/tinos/Tinos-Regular.ttf"),
                sha256: "60a0e8ef0c04dd5dd69ffe91025fa2ae5836cbd35600a82ba031977557e2cb61",
                size: 521588,
            },
            LookalikeFile {
                style: "Bold",
                file: "Tinos-Bold.ttf",
                url: google_fonts!("ofl/tinos/Tinos-Bold.ttf"),
                sha256: "393269dbab8899f938db19783eca5eac92eb431f7ae0ab45b8349ca895f1a06b",
                size: 597880,
            },
            LookalikeFile {
                style: "Italic",
                file: "Tinos-Italic.ttf",
                url: google_fonts!("ofl/tinos/Tinos-Italic.ttf"),
                sha256: "5942266ed398b155d7dc23e36833e7ec6be988f2439bdbeb8ef1bede808eaa91",
                size: 565016,
            },
            LookalikeFile {
                style: "BoldItalic",
                file: "Tinos-BoldItalic.ttf",
                url: google_fonts!("ofl/tinos/Tinos-BoldItalic.ttf"),
                sha256: "a5de79f0fe863ea0954757acb3d47b3ccd0a930ce3dd5b97230cd3866790a06e",
                size: 578976,
            },
        ],
    },
    Lookalike {
        family: "Arimo",
        replaces: &["Arial", "Helvetica"],
        keys: &["arialmt", "arial", "helvetica", "liberationsans", "arimo"],
        licence: "Apache-2.0",
        files: &[
            LookalikeFile {
                style: "Regular",
                file: "Arimo-Regular.ttf",
                url: arimo_static!("Arimo-Regular.ttf"),
                sha256: "f405056d25ddc3ad89a852db367f1dc075d8c7212fddcf7c488e212efeb8b176",
                size: 315784,
            },
            LookalikeFile {
                style: "Bold",
                file: "Arimo-Bold.ttf",
                url: arimo_static!("Arimo-Bold.ttf"),
                sha256: "6982e33e0370c907e364816a943ee5ce19468293aa21b9cbcb4d58bc1c230971",
                size: 317148,
            },
            LookalikeFile {
                style: "Italic",
                file: "Arimo-Italic.ttf",
                url: arimo_static!("Arimo-Italic.ttf"),
                sha256: "9fb05a040729cef085041deeb627e488fa7fa2f126f798f3832f3eee9c1ec4ae",
                size: 338188,
            },
            LookalikeFile {
                style: "BoldItalic",
                file: "Arimo-BoldItalic.ttf",
                url: arimo_static!("Arimo-BoldItalic.ttf"),
                sha256: "200c028da787741c888efee8f0416e3052160b21bce165a7607eb504a759f9fa",
                size: 340112,
            },
        ],
    },
    Lookalike {
        family: "Cousine",
        replaces: &["Courier New", "Courier"],
        keys: &["couriernewps", "couriernew", "courier", "liberationmono", "cousine"],
        licence: "OFL-1.1",
        files: &[
            LookalikeFile {
                style: "Regular",
                file: "Cousine-Regular.ttf",
                url: google_fonts!("ofl/cousine/Cousine-Regular.ttf"),
                sha256: "1da22250675fc4c42fcf3a9736c44bc0570516105331443b663fd5cfbd1412fe",
                size: 296856,
            },
            LookalikeFile {
                style: "Bold",
                file: "Cousine-Bold.ttf",
                url: google_fonts!("ofl/cousine/Cousine-Bold.ttf"),
                sha256: "17c8a7245156d2253531c9e529474937b09d9f641c5ae7695c5e33f22822eef4",
                size: 298048,
            },
            LookalikeFile {
                style: "Italic",
                file: "Cousine-Italic.ttf",
                url: google_fonts!("ofl/cousine/Cousine-Italic.ttf"),
                sha256: "ea2a76ae3d0ece9cd59f0d30fdc08dd70e8f5f457beee5b0852a7b50c2286c7c",
                size: 306880,
            },
            LookalikeFile {
                style: "BoldItalic",
                file: "Cousine-BoldItalic.ttf",
                url: google_fonts!("ofl/cousine/Cousine-BoldItalic.ttf"),
                sha256: "848e858726fee0ae27b754e4cd6a2755209bf1428a8c91f747696d58c33906c3",
                size: 308292,
            },
        ],
    },
    Lookalike {
        family: "Carlito",
        replaces: &["Calibri"],
        keys: &["calibri", "carlito"],
        licence: "OFL-1.1",
        files: &[
            LookalikeFile {
                style: "Regular",
                file: "Carlito-Regular.ttf",
                url: google_fonts!("ofl/carlito/Carlito-Regular.ttf"),
                sha256: "f6418f708baede9789daef5d458c0f53d2a888af9820e8062934e504fedc6595",
                size: 628032,
            },
            LookalikeFile {
                style: "Bold",
                file: "Carlito-Bold.ttf",
                url: google_fonts!("ofl/carlito/Carlito-Bold.ttf"),
                sha256: "bb5d20f79b82599ec72983597437373a80f2d2085fa91fc144fd74e876a594db",
                size: 682468,
            },
            LookalikeFile {
                style: "Italic",
                file: "Carlito-Italic.ttf",
                url: google_fonts!("ofl/carlito/Carlito-Italic.ttf"),
                sha256: "0b019225e58d702bfedcbd35c21696769f8ee115cb6343f84c2f240312450d1c",
                size: 615236,
            },
            LookalikeFile {
                style: "BoldItalic",
                file: "Carlito-BoldItalic.ttf",
                url: google_fonts!("ofl/carlito/Carlito-BoldItalic.ttf"),
                sha256: "b32928186c119599e03ca6a1ffc680fdcb7fac95772f4b95d989cf6cd3861517",
                size: 808508,
            },
        ],
    },
    Lookalike {
        family: "Caladea",
        replaces: &["Cambria"],
        keys: &["cambria", "caladea"],
        licence: "OFL-1.1",
        files: &[
            LookalikeFile {
                style: "Regular",
                file: "Caladea-Regular.ttf",
                url: google_fonts!("ofl/caladea/Caladea-Regular.ttf"),
                sha256: "f1e899278b7b4491aba5b6a8253c4b04c050cc59b21865be5c37559a775153cd",
                size: 81600,
            },
            LookalikeFile {
                style: "Bold",
                file: "Caladea-Bold.ttf",
                url: google_fonts!("ofl/caladea/Caladea-Bold.ttf"),
                sha256: "ae3cb2dcbc925809dd29d2a44e9802211cab66be541bacbfc9c08c74b27c3742",
                size: 84492,
            },
            LookalikeFile {
                style: "Italic",
                file: "Caladea-Italic.ttf",
                url: google_fonts!("ofl/caladea/Caladea-Italic.ttf"),
                sha256: "4359a8e24f748b6447b1ff6d7a174febe70961d29f8bb8634b56dacd740a3deb",
                size: 83780,
            },
            LookalikeFile {
                style: "BoldItalic",
                file: "Caladea-BoldItalic.ttf",
                url: google_fonts!("ofl/caladea/Caladea-BoldItalic.ttf"),
                sha256: "ccabaa7b7e2fdf253d2b1a5fa699dd8a3df8d835a9eb285ad82631a677eb76c0",
                size: 83356,
            },
        ],
    },
];

/// Name parts that mean another design of the family, with other widths: Arial Narrow is not
/// Arial, Helvetica Neue is not Helvetica.
const OTHER_DESIGNS: &[&str] = &["narrow", "condensed", "compressed", "neue", "light", "thin", "black", "heavy", "medium", "rounded", "cond"];

/// The look-alike for a PDF font name (`ABCDEF+TimesNewRomanPS-BoldMT`, `Arial,BoldItalic`), with
/// the weight and slant the name says. `None` for any other family or design.
pub fn for_font(base_font: &str) -> Option<(&'static Lookalike, bool, bool)> {
    let key = crate::embedding::face_key(base_font);
    for l in CATALOGUE {
        if let Some(rest) = l.keys.iter().find_map(|k| key.strip_prefix(k)) {
            if OTHER_DESIGNS.iter().any(|d| rest.contains(d)) {
                return None;
            }
            return Some((l, rest.contains("bold"), rest.contains("italic") || rest.contains("oblique")));
        }
    }
    None
}

/// The catalogue file whose SHA-256 these bytes have.
pub fn identify(bytes: &[u8]) -> Option<(&'static Lookalike, &'static LookalikeFile)> {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    CATALOGUE.iter().find_map(|l| l.files.iter().find(|f| f.sha256 == hex).map(|f| (l, f)))
}

fn installed_faces() -> &'static Mutex<Vec<(&'static str, &'static CraftFont)>> {
    static FACES: OnceLock<Mutex<Vec<(&'static str, &'static CraftFont)>>> = OnceLock::new();
    FACES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Make a downloaded file available for editing. Only a catalogue file (by SHA-256) that is a
/// usable, embeddable face is accepted; installing it again changes nothing.
pub fn install(bytes: Vec<u8>) -> Result<&'static LookalikeFile, String> {
    let (lookalike, file) = identify(&bytes).ok_or("this file is not one of PdfCraft Plus's look-alike fonts (its checksum doesn't match)")?;
    crate::embedding::validate(&bytes)?;
    let mut faces = installed_faces().lock().unwrap_or_else(PoisonError::into_inner);
    if faces.iter().any(|(name, _)| *name == file.file) {
        return Ok(file);
    }
    // Leaked once per catalogue file (at most 20, about 7 MB in all), so the face can be handed
    // out as `&'static` like the compiled-in craft-fonts faces.
    let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    let face: &'static CraftFont =
        Box::leak(Box::new(CraftFont { family: lookalike.family, style: file.style, scripts: &["Latn", "Grek", "Cyrl"], bytes }));
    faces.push((file.file, face));
    Ok(file)
}

/// Whether a catalogue file has been installed in this session.
pub fn is_installed(file: &LookalikeFile) -> bool {
    installed_faces().lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(name, _)| *name == file.file)
}

/// The installed look-alike face for a PDF font name, if any.
pub fn face_for(base_font: &str) -> Option<&'static CraftFont> {
    let (lookalike, bold, italic) = for_font(base_font)?;
    let file = lookalike.file(bold, italic)?;
    installed_faces().lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(name, _)| *name == file.file).map(|(_, face)| *face)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_find_their_look_alike_weight_and_slant() {
        let family = |n: &str| for_font(n).map(|(l, b, i)| (l.family, b, i));
        assert_eq!(family("DEVEXP+TimesNewRoman"), Some(("Tinos", false, false)));
        assert_eq!(family("ABCDEF+TimesNewRoman,BoldItalic"), Some(("Tinos", true, true)));
        assert_eq!(family("TimesNewRomanPS-BoldMT"), Some(("Tinos", true, false)));
        assert_eq!(family("Times-Roman"), Some(("Tinos", false, false)));
        assert_eq!(family("Arial-ItalicMT"), Some(("Arimo", false, true)));
        assert_eq!(family("Helvetica-BoldOblique"), Some(("Arimo", true, true)));
        assert_eq!(family("CourierNewPSMT"), Some(("Cousine", false, false)));
        assert_eq!(family("Calibri-Bold"), Some(("Carlito", true, false)));
        assert_eq!(family("Cambria"), Some(("Caladea", false, false)));
        assert_eq!(family("ArialNarrow"), None, "another design, other widths");
        assert_eq!(family("HelveticaNeue-Bold"), None);
        assert_eq!(family("Arial Black"), None);
        assert_eq!(family("Georgia"), None);
        assert_eq!(family(""), None);
    }

    #[test]
    fn the_catalogue_is_complete_and_pinned() {
        for l in CATALOGUE {
            for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
                let f = l.file(bold, italic).unwrap_or_else(|| panic!("{} lacks a style", l.family));
                assert!(f.url.starts_with("https://raw.githubusercontent.com/google/fonts/"), "{}", f.url);
                assert!(f.url.ends_with(f.file), "{}", f.url);
                assert_eq!(f.sha256.len(), 64);
                assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
                assert!(f.file.starts_with(l.family));
            }
            assert!(["OFL-1.1", "Apache-2.0"].contains(&l.licence), "an open licence on the allowlist");
        }
        assert!(identify(b"not a font").is_none());
    }

    #[test]
    fn only_catalogue_files_install() {
        assert!(install(b"not a font".to_vec()).is_err());
        assert!(install(include_bytes!("../../../assets/fonts/Inter-Regular.ttf").to_vec()).is_err(), "a real font, but not a look-alike");
    }

    /// With the real files (`PDFCRAFT_LOOKALIKE_DIR` holding the downloaded `.ttf`s), every one
    /// passes the checksum and embedding checks and becomes the face for its family's names.
    #[test]
    fn downloaded_files_install_and_match_their_families() {
        let Some(dir) = std::env::var_os("PDFCRAFT_LOOKALIKE_DIR") else { return };
        let dir = std::path::PathBuf::from(dir);
        for l in CATALOGUE {
            for f in l.files {
                let bytes = std::fs::read(dir.join(f.file)).unwrap();
                assert_eq!(bytes.len() as u32, f.size, "{}", f.file);
                assert_eq!(install(bytes).unwrap().file, f.file);
                assert!(is_installed(f));
            }
        }
        assert_eq!(face_for("DEVEXP+TimesNewRoman,Bold").map(|f| (f.family, f.style)), Some(("Tinos", "Bold")));
        assert_eq!(face_for("Calibri-Italic").map(|f| (f.family, f.style)), Some(("Carlito", "Italic")));
    }
}
