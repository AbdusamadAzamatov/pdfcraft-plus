# Interface language

PdfCraft Plus ships two interface languages: **English** and **Русский** (Russian). Upstream
PdfCraft carries 33 other catalogs; this fork leaves them out (the owner's decision), so its
settings accept only `auto`, `en` and `ru`.

Choose **Menu > Edit > Preferences…** (Command-comma on macOS, Ctrl-comma elsewhere) **> Interface
language** and select **Auto**, **English** or **Русский**. The change applies immediately and
persists between launches. Command ids, document contents and file names are unchanged.

**Auto** (the default) follows the system language: `LC_ALL`, `LC_MESSAGES` or `LANG`, then the
preferred-languages list on macOS (and the display languages on Windows). Any Russian locale
(`ru`, `ru_RU`, `ru_BY`, `ru_KZ`, …) uses the Russian catalog; any other system language shows
English. Displayed keyboard shortcuts follow the interface language; Russian keeps the Latin key
names `Ctrl` and `Shift`, and macOS keeps its modifier symbols (`⌘`, `⇧`, `⌃`).

The control channel exposes the setting through `ui.set`:

```json
{"method":"ui.set","params":{"key":"language","value":"ru"}}
```

The value is `auto`, `en` or `ru`, in any case. Unknown values return an error without changing
the current setting. Untranslated labels use English, and error details from the engine or the
operating system are shown as they are inside a translated frame. Russian needs only the bundled
Latin and Cyrillic faces (Inter, JetBrains Mono; tested in `tests/it/fonts.rs`).

Dates written into PDFs (Fill & Sign, stamps) keep upstream's wider list of date languages, which
always includes every interface language.

## How translations work

PdfCraft reads its translations from [craft-i18n](https://github.com/storytold/craft-i18n), shared by the Crafting Apps, through the `craft-i18n` crate (`crates/ui-egui/src/i18n.rs` is a thin wrapper):

- Strings in the code stay in English and serve as lookup keys. At draw time, `tl!("Save")` returns the text in the current language, and falls back to English when there is no entry.
- Each language has two catalogs in craft-i18n: `locales/<code>/common.tsv` (rows every app shares) and `locales/<code>/pdfcraft.tsv` (PdfCraft's own rows, which win over shared ones). One entry per line: `context<TAB>English<TAB>translation`. An empty context marks a plain string. `@id` keys an entry by command id (for example `file.saveAs`); PdfCraft reads `@id` rows only from `pdfcraft.tsv`. `@plural` holds plural forms, chosen by CLDR rules. Any other context disambiguates an English word with several meanings (`tl_ctx!` in UI code). craft-i18n's `docs/integration.md` documents the format and escapes.
- PdfCraft Plus embeds only the English and Russian catalogs, and appends `crates/ui-egui/src/i18n/ru.tsv` to the Russian one: the Russian rows for the strings only this fork has (`crates/ui-egui/build.rs`). A string the fork adds gets its Russian row there, not in craft-i18n.
- The build embeds the catalogs from `CRAFT_I18N_DIR`, a craft-i18n checkout at the commit in `craft-i18n.rev` (CI and the flatpak manifests check it out there). Unset, the build is English only; `CRAFT_I18N_REQUIRED=1` (release builds) makes that an error. Tests that need the catalogs say they are skipped.
- Placeholders (`{name}`) must appear in both columns. A trailing `…` must be kept. `i18n::fmt` fills placeholders in one pass, so a value such as a file name containing `{n}` is inserted as written.
- A missing entry never fails a build or a test: that string shows in English. `cargo xtask i18n-report`, a step of `cargo xtask ci`, prints for each language how many strings it has no translation for and the first few. It counts every command, All tools label, literal passed to `tl!` or `i18n::t` in the UI source, engine wording (below) and key of the embedded PdfCraft catalogs.
- craft-i18n validates rows strictly (unknown escapes, unknown `@` contexts, placeholder or ellipsis mismatches, wrong plural form counts, duplicates). At run time a bad row is skipped (and logged) and that string shows in English; `bundled_catalogs_are_consistent` prints them.
- `ATTRIBUTION.toml` records the translations as one `[[build_input]]`; craft-i18n's `ATTRIBUTION.md` credits the translators.
- Translations are clean-room: write them from the meaning of the English text, never from another product's string tables.

## Sentences the engine words

Some sentences are not the interface's own. The accessibility checker, the PDF/A verifier, the signature reader, the page-range parser and the edit history all word their own findings, and matching that wording in the UI with a `tl!` literal would break silently whenever the engine changed it. So those layers hand the wording over instead, apart from the document's own words:

- Each of them has a `WORDINGS` constant listing every sentence it can say, as `&'static str` with holes in it (`{n}`, `{p}`, `{}`). The sentence is written once, in the layer that means it.
- `said(&words)` says one of them: the layer looks its own wording up through the closure it is given, then fills the holes with the document's values — a page number, a font name, what the person typed — in one pass, so a value containing `{n}` is inserted as written.
- The UI passes `&crate::i18n::words`, which is a catalog lookup, so the sentence reads in the interface language. `in_english` is the same thing in English, and a wording with no entry comes back as it stands, which is the English — a catalog missing one shows an English sentence rather than a gap.
- The lists are `a11y::{WORDINGS, REPORT_WORDS}`, `preflight::WORDINGS`, `sign::pdf::{WORDINGS, kind::ALL}`, `print::WORDINGS` and the engine's `{EDIT_LABELS, BATCH_LABELS}`. `a11y::report_html` takes a language tag and a `words` closure, so a saved report reads the way the panel read, down to its `lang` attribute.
- The translation gaps report (`cargo xtask i18n-report`) counts every wording in those lists, so a catalog that lacks one shows there; until it is added, that sentence shows in English. Catalog validation requires each translation to keep the same holes.

Error details that come from the operating system, and faults rather than mistakes (a file that cannot be read, a lock that was poisoned), still carry their own text and are shown as they are.

## Adding a language

1. In craft-i18n, add `locales/xx/pdfcraft.tsv` with its `@id language.name` row, and translate entries.
2. Add one row to `LANGUAGES` in `crates/ui-egui/src/i18n.rs` (code and whether it is right-to-left), one to `english_name`, and one to `DATE_LANGUAGES` in `crates/engine/src/dates.rs`.
3. Bump `craft-i18n.rev` (and the `commit:` of the craft-i18n source in both flatpak manifests) to a craft-i18n commit with the catalog.

The Preferences dropdown, the system-language match and the translation gaps report then pick it up.

The egui 0.37 migration pins craft-fonts `b076417b5880bd4154bd22f00695b1d28c6bea83`. That manifest supplies Arabic, Hebrew, Thai and Devanagari faces selected for the web payload, but contains no Telugu face. The build code accepts a `Telu` row when a future input provides one; it does not establish Telugu glyph coverage with the current pin.
