use super::*;

const RU: fn() -> Lang = || Lang::from_code("ru").expect("ru registered");

/// Skips a test that needs the embedded catalogs when none were embedded (`CRAFT_I18N_DIR`
/// unset): the interface is then English only.
macro_rules! needs_catalogs {
    () => {
        if !needs_catalogs() {
            return;
        }
    };
}

/// The rows of `text` (catalog format) as (context, English source, translation), escapes
/// undone; comments, blank and malformed lines left out.
fn rows(text: &str) -> Vec<(String, String, String)> {
    let unescape = |s: &str| s.replace("\\\\", "\u{0}").replace("\\n", "\n").replace("\\t", "\t").replace('\u{0}', "\\");
    text.lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut c = l.split('\t');
            let (Some(context), Some(source), Some(translation), None) = (c.next(), c.next(), c.next(), c.next()) else { return None };
            Some((unescape(context), unescape(source), unescape(translation)))
        })
        .collect()
}

/// The embedded PdfCraft catalog of `code` (not the shared one), `""` without one.
fn app_catalog(code: &str) -> &'static str {
    CRAFT_I18N.iter().find(|(c, ..)| *c == code).map_or("", |(_, _, app)| *app)
}

#[test]
fn the_macos_bundle_declares_every_interface_language() {
    // AppKit translates its own dialogs (open and save panels) only into the languages the app
    // bundle declares, so `CFBundleLocalizations` must list every entry of `LANGUAGES`.
    let plist = include_str!("../../../../packaging/macos/Info.plist.in");
    let block = plist.split("<key>CFBundleLocalizations</key>").nth(1).expect("the template declares CFBundleLocalizations");
    let block = block.split("</array>").next().expect("the declaration is an array");
    let declared: Vec<String> = block.split("<string>").skip(1).filter_map(|s| s.split("</string>").next()).map(str::to_lowercase).collect();
    let registered: Vec<String> = LANGUAGES.iter().map(|l| l.code.to_lowercase()).collect();
    assert_eq!(declared, registered, "packaging/macos/Info.plist.in and LANGUAGES list different languages");
}

#[test]
fn keyboard_key_names_have_a_separate_context_and_english_fallback() {
    let keys = fixture_lang("x-keys", "key\tDelete\tEntf\n\tDelete\tLöschen\n\tOpen\tÖffnen\n");
    let previous = current();
    set_current(keys);
    assert_eq!(key_name("Delete"), "Entf");
    assert_eq!(t("Delete"), "Löschen");
    assert_eq!(key_name("Open"), "Open", "a missing key name must not borrow a command translation");
    set_current(Lang::EN);
    assert_eq!(key_name("Delete"), "Delete");
    set_current(previous);
}

#[cfg(target_os = "windows")]
#[test]
fn windows_display_language_query_returns_a_locale_tag() {
    // One tag per line, as `first_supported` reads them: a machine with several display
    // languages reports them all, so each line is checked rather than the list as a whole.
    let list = windows_ui_language().expect("Windows should report a display language");
    let tags: Vec<&str> = list.lines().map(str::trim).filter(|t| !t.is_empty()).collect();
    assert!(!tags.is_empty(), "no display language in {list:?}");
    for tag in tags {
        assert!(tag.split('-').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric())), "not a locale tag: {tag:?}");
    }
}

/// A bad line is skipped and reported; the rest of the catalog still loads.
#[test]
fn malformed_lines_are_reported_not_fatal() {
    let l = fixture_lang("x-bad", "\tok\tはい\nno tabs here\n\tonly\n\ta\tb\tc\textra\n\t\tempty source\n\tOpen…\tOpen\n");
    assert_eq!(tr(l, "ok"), "はい");
    assert_eq!(tr(l, "Open…"), "Open…", "a row that drops the ellipsis is skipped");
    assert_eq!(l.i18n().check_all().len(), 5, "{:?}", l.i18n().check_all());
}

#[test]
fn catalogs_handle_escapes_crlf_and_distinct_contexts() {
    let l =
        fixture_lang("x-esc", "# comment\r\n\r\n\tLine\\nTab\\tPath\\\\\tOther\\nTab\\tPath\\\\\r\nweight\tLight\tThin\r\ntheme\tLight\tPale\r\n");
    assert!(l.i18n().check_all().is_empty(), "{:?}", l.i18n().check_all());
    assert_eq!(tr(l, "Line\nTab\tPath\\"), "Other\nTab\tPath\\");
    assert_eq!(tr_ctx(l, "weight", "Light"), "Thin");
    assert_eq!(tr_ctx(l, "theme", "Light"), "Pale");
}

#[test]
fn broken_catalog_falls_back_to_english() {
    let broken = fixture_lang("x-broken", "malformed\n\\");
    assert_eq!(tr(broken, "File"), "File");
    assert_eq!(trn(broken, 2, "{n} page", "{n} pages"), "2 pages");
}

#[test]
fn formatting_reorders_parameters_without_reinterpreting_user_text() {
    let args = [("file", "日本語-{n}.pdf"), ("n", "2")];
    assert_eq!(fmt("{n}: {file}; {unknown}", &args), "2: 日本語-{n}.pdf; {unknown}");
    assert_eq!(fmt("{file} / {file}", &args), "日本語-{n}.pdf / 日本語-{n}.pdf");
    assert_eq!(fmt("text {unfinished", &args), "text {unfinished");
    assert_eq!(fmt("}{n}{", &args), "}2{");
    assert_eq!(fmt("unchanged", &[]), "unchanged");
}

/// Every literal passed to `tl!(…)` or `crate::i18n::t(…)` in the UI source (test modules
/// aside), unescaped, including literals on a line of their own.
fn ui_literals() -> std::collections::BTreeSet<String> {
    let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut literals = std::collections::BTreeSet::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).expect("UI source directory") {
            let path = entry.expect("UI source entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "i18n") {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let source = std::fs::read_to_string(path).expect("UI source file").replace("\r\n", "\n");
                let source = source.replace("crate::i18n::t(", "tl!(");
                let mut rest = source.split("#[cfg(test)]\nmod ").next().unwrap_or_default();
                while let Some((_, after)) = rest.split_once("tl!(") {
                    let after = after.trim_start();
                    let Some(after) = after.strip_prefix('"') else {
                        rest = after;
                        continue;
                    };
                    let mut escaped = false;
                    let end = after
                        .char_indices()
                        .find_map(|(i, c)| {
                            if c == '"' && !escaped {
                                return Some(i);
                            }
                            escaped = c == '\\' && !escaped;
                            None
                        })
                        .expect("closed tl! literal");
                    let (raw, tail) = after.split_at(end);
                    let argument_end = tail.strip_prefix('"').expect("closing quote").trim_start();
                    let literal_call =
                        argument_end.starts_with(')') || argument_end.strip_prefix(',').is_some_and(|rest| rest.trim_start().starts_with(')'));
                    if literal_call {
                        let label: String = serde_json::from_str(&format!("\"{raw}\"")).expect("UI literal escapes");
                        literals.insert(label);
                    }
                    rest = tail.strip_prefix('"').expect("closing quote");
                }
            }
        }
    }
    literals
}

/// Each translated label of a visible signature that runs into its value keeps a separator
/// at its end.
#[test]
fn signature_appearance_labels_keep_their_separator() {
    needs_catalogs!();
    let english = pdfcraft_engine::sign::AppearanceLabels::default();
    for lang in Lang::all().filter(|l| *l != Lang::EN) {
        for (field, label) in english.fields() {
            let said = tr(lang, label);
            if field != "signed_by" && !lang.rtl() {
                assert!(said.ends_with(' ') || said.ends_with('：'), "{}: the {field} label runs into its value: {said:?}", lang.code());
            }
        }
    }
}

/// An `@id` row that drops the English label's trailing `…` or changes its `{placeholders}` is not
/// used, by the menus or by `tr_id`: the label's translation shows instead. craft-i18n checks
/// neither for `@id` rows, whose source is the command id.
#[test]
fn an_id_row_that_changes_the_label_s_shape_is_not_used() {
    let l = fixture_lang(
        "x-shape",
        "@id\tfile.open\tÖffnen\n@id\tfile.close\tSchließen {name}\n@id\tfile.save\tSichern…\n\tOpen…\tÖffnen…\n\tClose\tSchließen\n",
    );
    assert_eq!(tr_id(l, "file.open", "Open…"), "Öffnen…", "the row drops the ellipsis");
    assert_eq!(tr_id(l, "file.close", "Close"), "Schließen", "the row adds a placeholder");
    assert_eq!(tr_id(l, "file.save", "Save…"), "Sichern…", "a row that keeps the shape is used");
    set_current(l);
    assert_eq!(menu_label("file.open", "Open…"), "Öffnen…");
    assert_eq!(menu_label("file.close", "Close"), "Schließen");
    assert_eq!(menu_label("file.save", "Save…"), "Sichern…");
    set_current(Lang::EN);
    assert!(same_shape("{b} und {a}", "{a} and {b}"), "placeholders may be reordered");
    assert!(!same_shape("Öffnen {name…", "Open…"), "a placeholder left open");
    assert!(!same_shape("Öffnen name}…", "Open…"), "a closing brace without its opening one");
    assert!(!same_shape("{name}} öffnen", "Open {name}"), "a second closing brace");
}

#[test]
fn current_language_is_per_thread() {
    set_current(fixture_lang("x-thread", "\tFile\tファイル\n"));
    assert_eq!(t("File"), "ファイル");
    std::thread::spawn(|| assert_eq!(current(), Lang::EN)).join().unwrap();
    set_current(Lang::EN);
    assert_eq!(t("File"), "File");
}

#[test]
fn russian_is_registered() {
    needs_catalogs!();
    let ru = Lang::from_code("ru").expect("ru registered");
    assert_eq!(ru.name(), "Русский");
    assert_eq!(normalize_pref("RU"), Some("ru"));
    assert_eq!(lang_from_tag("ru_RU.UTF-8"), Some(ru));
    assert_eq!(lang_from_tag("ru-BY"), Some(ru));
    assert_eq!(first_supported("ru-RU\r\nen-US"), Some(ru));
    assert_eq!(tr(ru, "Rapport de l'utilisateur.pdf"), "Rapport de l'utilisateur.pdf");
    let mut app = crate::PdfCraftApp::default();
    app.set_option("language", "ru").unwrap();
    assert_eq!(app.language, "ru");
    let mut restored = crate::PdfCraftApp::default();
    restored.restore(&app.persist());
    assert_eq!(restored.language, "ru");
}

#[test]
fn slovene_plurals_cover_dual_few_teens_and_large_counts() {
    let form = plural_fixture("sl", 4);
    for (counts, f) in [
        (vec![1, 101, 201, 1001, 1101], 0),
        (vec![2, 102, 202, 1002], 1),
        (vec![3, 4, 103, 104, 1003], 2),
        (vec![0, 5, 10, 11, 12, 13, 14, 15, 20, 21, 22, 23, 24, 25, 31, 100, 111, 112, 121, 122, 1000, u64::MAX], 3),
    ] {
        for n in counts {
            assert_eq!(form(n), f, "{n}");
        }
    }
}

/// A form picker for `code`: a fixture row with `forms` plural forms, and for a count the form
/// index the crate's CLDR rules choose.
fn plural_fixture(code: &'static str, forms: usize) -> impl Fn(u64) -> usize {
    let row = (0..forms).map(|i| format!("F{i} {{n}}")).collect::<Vec<_>>().join("|");
    let lang = fixture_lang(code, Box::leak(format!("@plural\t{{n}} x|{{n}} xs\t{row}\n").into_boxed_str()));
    move |n| {
        let said = trn(lang, n, "{n} x", "{n} xs");
        said.strip_prefix('F')
            .and_then(|s| s.split(' ').next())
            .and_then(|i| i.parse().ok())
            .unwrap_or_else(|| panic!("{code}: no form for {n}: {said:?}"))
    }
}

/// The language list is well-formed. Problems in the embedded catalogs are not failed: craft-i18n
/// checks its rows, a row PdfCraft can't use shows in English, and `translation_gaps_report`
/// lists the `@id` rows PdfCraft doesn't use ([`unused_id_rows`]).
#[test]
fn bundled_catalogs_are_consistent() {
    let mut codes = std::collections::HashSet::new();
    for l in &LANGUAGES {
        assert!(codes.insert(l.code), "duplicate code {}", l.code);
        assert!(l.code == l.code.to_ascii_lowercase() && !l.code.is_empty(), "{}", l.code);
    }
    assert_eq!(LANGUAGES.first().map(|l| l.code), Some("en"), "English first: the source and the fallback");
    if !needs_catalogs() {
        return;
    }
    let problems = I18N.check_all();
    if !problems.is_empty() {
        println!("{} catalog rows are skipped (they show in English):\n{problems:#?}", problems.len());
    }
}

/// The `@id` rows of the embedded PdfCraft catalogs that are never shown, for the translation gaps
/// report: a row for a command PdfCraft doesn't have, and one that doesn't keep its label's
/// placeholders or trailing `…` ([`same_shape`]), whose label's translation shows instead. One
/// line each under a heading; empty when there are none.
fn unused_id_rows() -> String {
    let mut out = Vec::new();
    for lang in Lang::all().filter(|l| *l != Lang::EN) {
        for (_, id, translation) in rows(app_catalog(lang.code())).into_iter().filter(|(c, ..)| c == "@id") {
            if id == "language.name" {
                continue;
            }
            let Some(command) = pdfcraft_engine::commands::command(&id) else {
                out.push(format!("{}: @id {id:?} names a command PdfCraft doesn't have", lang.code()));
                continue;
            };
            if !same_shape(&translation, command.label) {
                let label = command.label;
                out.push(format!("{}: @id {id:?} doesn't keep the placeholders or ellipsis of {label:?}: {translation:?}", lang.code()));
            }
        }
    }
    if out.is_empty() {
        return String::new();
    }
    format!("{} @id rows not used (their label's translation shows):\n{}", out.len(), out.iter().map(|row| format!("  {row}\n")).collect::<String>())
}

/// The (context, English source) keys PdfCraft looks up: the UI source's literals, the commands,
/// All tools, the engine's own wordings, and every key of every embedded PdfCraft catalog (which
/// covers contextual, plural and dynamically looked-up strings the scan can't see).
fn english_keys() -> std::collections::BTreeSet<(String, String)> {
    use std::collections::BTreeSet;
    let mut plain: BTreeSet<String> = ui_literals();
    let mut add = |s: &str| {
        plain.insert(s.to_string());
    };
    for command in pdfcraft_engine::commands::COMMANDS {
        add(command.label);
        command.menu.into_iter().for_each(&mut add);
    }
    for group in pdfcraft_engine::catalog::TOOL_GROUPS {
        add(group.label);
        for section in group.sections {
            add(section.title);
            section.items.iter().for_each(|item| add(item.label));
        }
    }
    pdfcraft_engine::catalog::TOOL_CATEGORIES.iter().for_each(|category| add(category.label));
    let engine: [&[&str]; 9] = [
        pdfcraft_engine::a11y::WORDINGS,
        pdfcraft_engine::a11y::REPORT_WORDS,
        pdfcraft_engine::pdfa::WORDINGS,
        pdfcraft_engine::sign::pdf::WORDINGS,
        pdfcraft_engine::sign::pdf::kind::ALL,
        pdfcraft_engine::print::WORDINGS,
        pdfcraft_engine::EDIT_LABELS,
        pdfcraft_engine::BATCH_LABELS,
        pdfcraft_engine::field_table::WORDINGS,
    ];
    engine.into_iter().flatten().for_each(|s| add(s));
    pdfcraft_engine::sign::AppearanceLabels::default().fields().into_iter().for_each(|(_, label)| add(label));
    crate::credits::NameMode::ALL.iter().for_each(|mode| add(mode.label()));
    for key in crate::credits::SortKey::ALL {
        let (label, header) = key.label();
        add(label);
        add(header);
    }
    crate::credits::MODEL_COLUMNS.iter().for_each(|s| add(s));
    // (context, source) keys; `@id` rows only reword a command label, which is in `plain`.
    let mut keys: BTreeSet<(String, String)> = plain.into_iter().map(|s| (String::new(), s)).collect();
    for (_, _, app) in CRAFT_I18N {
        keys.extend(rows(app).into_iter().filter(|(context, ..)| context != "@id").map(|(context, source, _)| (context, source)));
    }
    keys
}

/// The translation gaps report (`cargo xtask i18n-report`, a step of `cargo xtask ci`): for
/// each language, how many English strings it has no translation for, and the first few, and how
/// many are kept in English (a row whose translation is the English itself). It never fails: a
/// missing string shows in English, and translators fill it in craft-i18n. Without embedded
/// catalogs it says so.
///
/// The English strings are the UI source's literals, the commands, All tools, the engine's
/// own wordings, and every key of every embedded PdfCraft catalog (which covers contextual,
/// plural and dynamically looked-up strings the scan can't see).
#[test]
fn translation_gaps_report() {
    const SHOWN: usize = 3;
    if !catalogs_embedded() {
        println!("{REPORT_START}\nTranslation gaps: skipped, built without craft-i18n (set CRAFT_I18N_DIR to a craft-i18n checkout)\n{REPORT_END}");
        return;
    }
    let keys = english_keys();
    let problems = I18N.check_all();
    let mut report = format!("Translation gaps: {} English strings; {} catalog rows skipped (they show in English)\n", keys.len(), problems.len());
    for lang in Lang::all().filter(|l| *l != Lang::EN) {
        let found = |context: &str, source: &str| lang.i18n().get(lang.lang, context, source).map(str::to_owned);
        let missing: Vec<&(String, String)> = keys
            .iter()
            .filter(|(context, source)| match context.as_str() {
                "" => found("", source).is_none(),
                // A contextual lookup falls back to the plain translation.
                context => found(context, source).is_none() && (context == "@plural" || found("", source).is_none()),
            })
            .collect();
        let kept = keys.iter().filter(|(context, source)| context.is_empty() && found("", source).as_ref() == Some(source)).count();
        let mut line = format!("  {:<8} {:>5} missing, {kept:>4} kept in English", lang.code(), missing.len());
        for (i, (context, source)) in missing.iter().take(SHOWN).enumerate() {
            let mut shown: String = source.chars().take(48).collect();
            if shown.len() < source.len() {
                shown.push('…');
            }
            let key = if context.is_empty() { format!("{shown:?}") } else { format!("{context} / {shown:?}") };
            line.push_str(if i == 0 { ": " } else { ", " });
            line.push_str(&key);
        }
        if missing.len() > SHOWN {
            line.push_str(", …");
        }
        report.push_str(&line);
        report.push('\n');
    }
    report.push_str(&unused_id_rows());
    println!("{REPORT_START}\n{report}{REPORT_END}");
}

/// The lines `cargo xtask i18n-report` prints from the test output lie between these.
const REPORT_START: &str = "=== translation gaps ===";
const REPORT_END: &str = "=== end of translation gaps ===";

/// PdfCraft Plus offers fewer interface languages than upstream; dates written into PDFs keep
/// upstream's full list, which must still include every interface language, under the
/// interface's name for it when the catalogs are embedded.
#[test]
fn every_interface_language_has_its_date_language() {
    let dates: Vec<&str> = pdfcraft_engine::dates::DATE_LANGUAGES.iter().map(|l| l.code).collect();
    for l in &LANGUAGES {
        assert!(dates.contains(&l.code), "{} has no date language", l.code);
    }
    needs_catalogs!();
    let dates: Vec<(&str, &str)> = pdfcraft_engine::dates::DATE_LANGUAGES.iter().map(|l| (l.code, l.name)).collect();
    for l in Lang::all() {
        assert!(dates.contains(&(l.code(), l.name())), "{} has no date language", l.code());
    }
}

/// The stale-draft notice remains discoverable after the UI modules are reorganized.
#[test]
fn properties_stale_notice_is_in_translation_scan() {
    assert!(ui_literals().contains("The document changed. Reopen Document Properties to edit the current version."));
}

/// A report, not a check: the notifications pasting an image and pinning a tool show, in each
/// language that has no translation for them yet (they show in English).
#[test]
fn image_paste_and_home_notifications_are_translated() {
    for language in Lang::all().filter(|l| *l != Lang::EN) {
        for source in ["Search…", "Image pasted", "No image on clipboard to paste", "Pinned {tool} to Home", "Unpinned {tool} from Home"] {
            if !has(language, source) {
                println!("{}: no translation for {source:?} (shows in English)", language.code());
            }
        }
    }
}

/// Russian comes from craft-i18n, and labels without a row fall back to English.
#[test]
fn russian_lookups_fall_back_to_english() {
    needs_catalogs!();
    assert_eq!(tr(RU(), "no such label"), "no such label");
    assert_eq!(tr(Lang::EN, "File"), "File");
    assert_eq!(tr(RU(), "File"), "Файл");
    assert_eq!(tr_ctx(RU(), "no such context", "File"), "Файл");
    assert!(has(RU(), "Save") && !has(Lang::EN, "Save"));
    assert_eq!(Lang::from_pref("RU"), RU());
    assert_eq!(Lang::from_pref("en"), Lang::EN);
    // No catalog has this plural entry: Russian falls back to the English forms.
    assert_eq!(trn(RU(), 7, "{n} test item", "{n} test items"), "7 test items");
}

/// The strings only PdfCraft Plus has are translated by its own rows (`ru.tsv`), which the build
/// appends to craft-i18n's Russian catalog.
#[test]
fn russian_has_the_rows_only_pdfcraft_plus_adds() {
    needs_catalogs!();
    assert_eq!(tr(RU(), "Welcome to PdfCraft Plus"), "Добро пожаловать в PdfCraft Plus");
    assert_eq!(tr(RU(), "About PdfCraft Plus"), "О программе PdfCraft Plus");
    for (context, source, translation) in rows(include_str!("ru.tsv")) {
        let shown = if context.is_empty() { tr(RU(), &source).to_owned() } else { tr_ctx(RU(), &context, &source).to_owned() };
        if !source.contains('|') && !context.starts_with('@') {
            assert_eq!(shown, translation, "{context:?} {source:?}");
        }
    }
}

/// Only English and Russian are offered, whatever craft-i18n holds.
#[test]
fn only_english_and_russian_are_interface_languages() {
    assert_eq!(LANGUAGES.iter().map(|l| l.code).collect::<Vec<_>>(), ["en", "ru"]);
    assert_eq!(normalize_pref("de"), None);
    assert_eq!(Lang::from_pref("ja"), Lang::EN);
}

#[test]
fn windows_display_language_resolves_chinese_scripts() {
    for tag in ["zh-CN", "zh-SG", "zh-Hans", "zh-Hans-CN"] {
        assert_eq!(first_supported(&format!("{tag}\r\n")), Lang::from_code("zh-hans"));
    }
    for tag in ["zh-TW", "zh-HK", "zh-MO", "zh-Hant", "zh-Hant-HK"] {
        assert_eq!(first_supported(&format!("{tag}\r\n")), Lang::from_code("zh-hant"));
    }
    assert_eq!(first_supported("en-US\r\n"), Some(Lang::EN));
    assert_eq!(first_supported("fr-FR\r\n"), Lang::from_code("fr"));
    assert_eq!(first_supported("lo-LA\r\n"), Lang::from_code("lo"));
    assert_eq!(first_supported("\r\n"), None);
}

#[cfg(target_os = "windows")]
#[test]
fn neutral_locale_does_not_override_display_language() {
    // Russian: PdfCraft Plus ships English and Russian only.
    let ru = RU();
    for tag in ["C", "C.UTF-8", "POSIX", "posix.UTF-8", "", "  C.UTF-8  ", "unsupported"] {
        assert_eq!(language_override(tag), None, "{tag}");
        assert_eq!(language_override(tag).or_else(|| first_supported("unsupported-XY\nru-RU\nen-US")), Some(ru));
    }
    assert_eq!(["C.UTF-8", "POSIX", "ru_RU.UTF-8"].into_iter().find_map(language_override), Some(ru));
    assert_eq!(language_override("ru_RU.UTF-8"), Some(ru));
    assert_eq!(language_override("en_US.UTF-8"), Some(Lang::EN));
    assert_eq!(language_override("fr_FR.UTF-8"), None, "a language without a catalog doesn't override");
}

#[test]
fn language_persists_and_invalid_input_keeps_current_language() {
    let mut app = crate::PdfCraftApp::default();
    assert_eq!(app.language, AUTO);
    app.set_option("language", "RU").unwrap();
    assert_eq!(app.language, "ru");
    assert!(app.set_option("language", "xx").is_err());
    assert!(app.set_option("language", "ja").is_err(), "upstream languages the fork dropped are refused");
    assert_eq!(app.language, "ru");
    let mut restored = crate::PdfCraftApp::default();
    restored.restore(&app.persist());
    assert_eq!(restored.language, "ru");
    restored.restore(r#"{"language":"xx"}"#);
    assert_eq!(restored.language, "ru");
    app.set_option("language", AUTO).unwrap();
    assert_eq!(app.language, AUTO);
    // Settings written before the system-locale default (no key) follow the system.
    let mut legacy = crate::PdfCraftApp::default();
    legacy.restore("{}");
    assert_eq!(legacy.language, AUTO);
}
