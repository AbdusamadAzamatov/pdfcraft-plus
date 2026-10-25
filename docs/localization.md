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

PdfCraft uses the same system as PhotoCraft (`crates/ui-egui/src/i18n/`):

- Strings in the code stay in English and serve as lookup keys. At draw time, `tl!("Save")` returns the text in the current language, and falls back to English when there is no entry.
- Each language has a catalog, `crates/ui-egui/src/i18n/<code>.tsv`, with one entry per line: `context<TAB>English<TAB>translation`. An empty context marks a plain string. `@id` keys an entry by command id (for example `file.saveAs`), so one menu item can read differently from another with the same English label. `@plural` holds plural forms. Any other context disambiguates an English word with several meanings (`tl_ctx!` in UI code, for example the signature pad's "Type" button against the "Type" column). The header of `ru.tsv` documents the format and escapes.
- Placeholders (`{name}`) must appear in both columns. A trailing `…` must be kept. `i18n::fmt` fills placeholders in one pass, so a value such as a file name containing `{n}` is inserted as written.
- A missing entry never fails a build or a test: that string shows in English. `cargo xtask i18n-report`, a step of `cargo xtask ci`, prints for each language how many strings it has no translation for and the first few. It counts every command, All tools label, literal passed to `tl!` or `i18n::t` in the UI source, engine wording (below) and key of the bundled catalogs, so a new English string shows up there until each catalog has it.
- Catalogs are validated strictly: unknown escapes, unknown `@` contexts, placeholder or ellipsis mismatches, wrong plural form counts and duplicates are errors. The tests require the bundled catalogs to have none; at run time a bad line is skipped (and logged) and that string shows in English.
- Each `.tsv` has an `ATTRIBUTION.toml` entry with `kind = "translation"` (title, author, source, licence), checked by `cargo xtask assets`. Catalogs are project files under the project licence, so the entry has no `sha256` and editing a catalog doesn't touch `ATTRIBUTION.toml`; git history records their provenance.
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

1. Copy the header of `ru.tsv` into `xx.tsv` and translate entries.
2. Add one row to `LANGUAGES` in `crates/ui-egui/src/i18n/mod.rs`: code, native name, catalog and plural rule.
3. Add an `ATTRIBUTION.toml` entry for the catalog (`kind = "translation"`, no `sha256`), then run `cargo xtask assets --write` (it regenerates `ATTRIBUTION.md` only) and `cargo xtask assets`.

The Preferences dropdown, the system-language match, the catalog tests (format, duplicates, placeholders, plural forms, command ids) and the translation gaps report then pick it up.
