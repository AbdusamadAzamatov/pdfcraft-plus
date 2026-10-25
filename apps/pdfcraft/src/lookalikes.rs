//! Free look-alike fonts (PdfCraft Plus): downloads a catalogue file when the user asks
//! (Edit a PDF ▸ Fonts), keeps it in the settings folder's `fonts` folder and installs it for
//! editing. Only files whose SHA-256 is in [`pdfcraft_fonts::lookalike::CATALOGUE`] are kept or
//! installed, so neither the network nor that folder can bring in another font.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pdfcraft_fonts::lookalike::{self, LookalikeFile};
use pdfcraft_ui_egui::font_suggest::FontFetch;

/// Install the catalogue files saved in `dir` by earlier downloads; anything else there is left
/// alone and not used.
pub fn install_saved(dir: &Path) {
    for file in lookalike::CATALOGUE.iter().flat_map(|l| l.files) {
        let path = dir.join(file.file);
        let Ok(mut f) = std::fs::File::open(&path) else { continue };
        let mut bytes = Vec::new();
        // A catalogue file is under 1 MB; a larger one there isn't ours.
        if f.by_ref().take(u64::from(file.size) + 1).read_to_end(&mut bytes).is_err() {
            continue;
        }
        if let Err(e) = lookalike::install(bytes) {
            log::warn!("look-alike font {} not used: {e}", path.display());
        }
    }
}

/// Download one catalogue file, check its SHA-256, save it into `dir` (when known) and install it.
pub fn fetcher(dir: Option<PathBuf>) -> FontFetch {
    std::sync::Arc::new(move |file: &'static LookalikeFile| {
        let bytes = download(file)?;
        match lookalike::identify(&bytes) {
            Some((_, found)) if found.file == file.file => {}
            _ => return Err(format!("{} didn't match its checksum; nothing was installed", file.file)),
        }
        if let Some(dir) = &dir {
            // Saving is for the next start; a failure only means downloading again then.
            let saved = std::fs::create_dir_all(dir).and_then(|()| {
                pdfcraft_platform::staging::write_atomically(&dir.join(file.file), &bytes, pdfcraft_platform::staging::StagingName::TagThenSuffix)
            });
            if let Err(e) = saved {
                log::warn!("couldn't keep {} for the next start: {e}", file.file);
            }
        }
        lookalike::install(bytes).map(|_| ())
    })
}

fn download(file: &LookalikeFile) -> Result<Vec<u8>, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(crate::updates::os_roots()?).build())
        .build()
        .new_agent();
    let mut response = agent
        .get(file.url)
        .header("User-Agent", concat!("PdfCraft/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| format!("couldn't download {} ({e})", file.file))?;
    response.body_mut().with_config().limit(u64::from(file.size) + 1).read_to_vec().map_err(|e| format!("couldn't download {} ({e})", file.file))
}
