use super::catalog::{Entry, parse_entries, placeholders};
use super::*;

const RU: fn() -> Lang = || Lang::from_code("ru").expect("ru registered");

#[test]
fn keyboard_key_names_have_a_separate_context_and_english_fallback() {
    static KEYS: LangInfo = LangInfo {
        code: "keys",
        name: "Key name test catalog",
        source: "key\tDelete\tEntf\n\tDelete\tLöschen\n\tOpen\tÖffnen\n",
        plural: plural_one_other,
        rtl: false,
        catalog: OnceLock::new(),
    };
    let previous = current();
    set_current(Lang(&KEYS));
    assert_eq!(key_name("Delete"), "Entf");
    assert_eq!(t("Delete"), "Löschen");
    assert_eq!(key_name("Open"), "Open", "a missing key name must not borrow a command translation");
    set_current(Lang::EN);
    assert_eq!(key_name("Delete"), "Delete");
    set_current(previous);
}

/// Regression test: a language picker uses its native name only when a face can draw it, otherwise its
/// English name.
#[test]
fn picker_names_fall_back_to_english_when_their_script_has_no_face() {
    let ru = RU();
    assert_eq!(ru.picker_name(|_| true), "Русский", "the native name is kept when it can be drawn");
    assert_eq!(ru.picker_name(|_| false), "Russian", "no Cyrillic face: an English name, not an unrenderable label");
    assert_eq!(Lang::EN.picker_name(|_| false), "English");
    for language in Lang::all() {
        assert!(!language.english_name().is_empty(), "{}", language.code());
        assert_ne!(language.english_name(), language.code(), "{} needs an English picker fallback", language.code());
    }
}

#[test]
fn tags_map_to_languages() {
    assert_eq!(lang_from_tag("ru_RU.UTF-8"), Some(RU()));
    assert_eq!(lang_from_tag("ru-KZ"), Some(RU()));
    assert_eq!(lang_from_tag("en_US.UTF-8"), Some(Lang::EN));
    assert_eq!(lang_from_tag("C"), Some(Lang::EN));
    assert_eq!(lang_from_tag("POSIX"), Some(Lang::EN));
    assert_eq!(lang_from_tag("fr_FR"), Lang::from_code("fr"));
    assert_eq!(lang_from_tag("fr_CA.UTF-8"), Lang::from_code("fr"));
    assert_eq!(lang_from_tag("fr-BE"), Lang::from_code("fr"));
    assert_eq!(lang_from_tag("zh-TW"), Lang::from_code("zh-hant"));
    assert_eq!(lang_from_tag("zh_CN.UTF-8"), Lang::from_code("zh-hans"));
    assert_eq!(lang_from_tag("zh"), Lang::from_code("zh-hans"));
    assert_eq!(lang_from_tag("zh_HK.UTF-8"), Lang::from_code("zh-hant"));
    assert_eq!(lang_from_tag("zh-Hant-MO"), Lang::from_code("zh-hant"));
    assert_eq!(lang_from_tag("lo_LA.UTF-8"), Lang::from_code("lo"));
    assert_eq!(lang_from_tag("lo-LA"), Lang::from_code("lo"));
    assert_eq!(lang_from_tag("lo"), Lang::from_code("lo"));
    assert_eq!(lang_from_tag(""), None);
    assert_eq!(lang_from_tag("_"), None);
}

#[test]
fn candidates_walk_from_specific_to_general() {
    assert_eq!(candidates("pt_BR.UTF-8"), ["pt-br", "pt"]);
    assert_eq!(candidates("pt_PT.UTF-8"), ["pt-pt", "pt-br", "pt"]);
    assert_eq!(candidates("pt"), ["pt-br", "pt"]);
    assert_eq!(candidates("zh_TW"), ["zh-tw", "zh-hant", "zh"]);
    assert_eq!(candidates("zh-CN"), ["zh-cn", "zh-hans", "zh"]);
    assert_eq!(candidates("zh-Hant-HK"), ["zh-hant-hk", "zh-hant", "zh"]);
    assert_eq!(candidates("lo_LA.UTF-8"), ["lo-la", "lo"]);
    assert_eq!(candidates("nb_NO.UTF-8"), ["nb-no", "nb"]);
    assert_eq!(candidates("no_NO.UTF-8"), ["no-no", "no", "nb"]);
    assert_eq!(candidates("nn-NO"), ["nn-no", "nn", "nb"]);
}

#[test]
fn macos_language_list_is_parsed() {
    assert_eq!(first_supported("(\n    \"ru-RU\",\n    \"en-US\"\n)\n"), Some(RU()));
    assert_eq!(first_supported("(\n    \"fr-FR\",\n    \"en-US\"\n)\n"), Some(Lang::EN), "French has no catalog, so English is next");
    assert_eq!(first_supported("("), None);
}

/// #2954: the web build's Auto language follows the browser's preferred languages in order,
/// skipping ones without a catalog, and is English only when none is supported.
#[test]
fn browser_language_list_is_followed_in_order() {
    assert_eq!(first_supported_tag(["it-IT", "en-US"]), Some(Lang::EN), "a language without a catalog is skipped");
    assert_eq!(first_supported_tag(["xx-YY", "ru", "fr-FR"]), Some(RU()));
    assert_eq!(first_supported_tag(["zh-TW"]), Lang::from_code("zh-hant"));
    assert_eq!(first_supported_tag(["en-GB", "fr-FR"]), Some(Lang::EN));
    assert_eq!(first_supported_tag([" ru-BY "]), Some(RU()));
    assert_eq!(first_supported_tag(["", "xx"]), None);
    assert_eq!(first_supported_tag(Vec::<String>::new()), None);
    let long = std::iter::repeat_n("xx", 10_000).chain(["fr"]);
    assert_eq!(first_supported_tag(long), None, "only the first tags are read");
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

#[test]
fn preferences_resolve_with_fallback() {
    assert_eq!(Lang::from_pref("ru"), RU());
    assert_eq!(Lang::from_pref("RU"), RU());
    assert_eq!(Lang::from_pref("en"), Lang::EN);
    // `auto` and unknown codes follow the system (English under test).
    assert_eq!(Lang::from_pref(AUTO), Lang::EN);
    assert_eq!(Lang::from_pref("xx-unknown"), Lang::EN);
    assert_eq!(normalize_pref("RU"), Some("ru"));
    assert_eq!(normalize_pref("Auto"), Some(AUTO));
    assert_eq!(normalize_pref("xx"), None);
}

#[test]
fn lookups_fall_back_to_english() {
    assert_eq!(tr(RU(), "no such label"), "no such label");
    assert_eq!(tr(Lang::EN, "File"), "File");
    assert_eq!(tr(RU(), "File"), "Файл");
    assert_eq!(tr(RU(), "Русский документ.pdf"), "Русский документ.pdf");
    assert_eq!(tr_id(RU(), "no.such.id", "File"), "Файл");
    assert_eq!(tr_ctx(RU(), "no such context", "File"), "Файл");
    assert!(has(RU(), "Save") && !has(Lang::EN, "Save"));
}

#[test]
fn catalog_kinds_are_parsed_and_looked_up() {
    let (c, errors) =
        Catalog::parse("# c\n\tHello\tこんにちは\n@id\tfile.save\t保存する\nmenu\tWindows\tウィンドウ群\n@plural\t{n} file|{n} files\t{n} 個\n\n", 1);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(c.plain("Hello"), Some("こんにちは"));
    assert_eq!(c.id("file.save"), Some("保存する"));
    assert_eq!(c.contextual("menu", "Windows"), Some("ウィンドウ群"));
    assert_eq!(c.contextual("other", "Windows"), None);
    assert_eq!(c.plural("{n} file", "{n} files", 0), Some("{n} 個"));
    assert_eq!(c.plural("{n} file", "{n} files", 5), Some("{n} 個"), "an index past the forms clamps");
}

#[test]
fn malformed_lines_are_reported_not_fatal() {
    let (entries, errors) = parse_entries("\tok\tはい\nno tabs here\n\tonly\n\ta\tb\tc\textra\n\t\tempty source\n", 1);
    assert_eq!(entries.len(), 1);
    assert_eq!(errors.len(), 4, "{errors:?}");
    let (entries, errors) = parse_entries("\ta\\tb\tx\\ny\\\\z\n", 1);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(entries, [Entry { context: String::new(), source: "a\tb".into(), translation: "x\ny\\z".into() }]);
}

#[test]
fn strict_validation_skips_only_the_bad_line() {
    for (text, forms, error) in [
        ("\tFile", 1, "context<TAB>"),
        ("\tFile\t", 1, "nonempty"),
        ("\tFile\\q\tFiles", 1, "unknown escape"),
        ("\tFile\tFiles\\", 1, "trailing backslash"),
        ("\tOpen…\tOpen", 1, "ellipsis"),
        ("\tOpen {file}\tOpen {name}", 1, "placeholders"),
        ("@plural\tpage\tPages", 1, "one|other"),
        ("@plural\t{n} page|{n} pages\t{n} pages", 2, "2 nonempty"),
        ("@plural\t{n} page|{n} pages\tPages", 1, "placeholders"),
        ("@unknown\tFile\tFiles", 1, "reserved context"),
    ] {
        // The bad line comes first; the good line after it still loads.
        let (catalog, errors) = Catalog::parse(&format!("{text}\n\tSave\t保存\n"), forms);
        assert_eq!(errors.len(), 1, "{text:?}: {errors:?}");
        assert!(errors[0].starts_with("line 1: ") && errors[0].contains(error), "{errors:?}");
        assert_eq!(catalog.plain("Save"), Some("保存"), "{text:?}");
    }
    let (catalog, errors) = Catalog::parse("\tFile\tファイル\n\tFile\t別\n", 1);
    assert!(errors.len() == 1 && errors[0].contains("line 2: duplicate"), "{errors:?}");
    assert_eq!(catalog.plain("File"), Some("ファイル"), "the first entry wins");
}

#[test]
fn catalogs_handle_escapes_crlf_and_distinct_contexts() {
    let (catalog, errors) =
        Catalog::parse("# comment\r\n\r\n\tLine\\nTab\\tPath\\\\\tOther\\nTab\\tPath\\\\\r\nweight\tLight\tThin\r\ntheme\tLight\tPale\r\n", 1);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(catalog.plain("Line\nTab\tPath\\"), Some("Other\nTab\tPath\\"));
    assert_eq!(catalog.contextual("weight", "Light"), Some("Thin"));
    assert_eq!(catalog.contextual("theme", "Light"), Some("Pale"));
}

#[test]
fn broken_catalog_falls_back_to_english() {
    static BROKEN: LangInfo =
        LangInfo { code: "broken", name: "Broken test catalog", source: "malformed\n\\", plural: |_| 0, rtl: false, catalog: OnceLock::new() };
    assert_eq!(tr(Lang(&BROKEN), "File"), "File");
    assert_eq!(trn(Lang(&BROKEN), 2, "{n} page", "{n} pages"), "2 pages");
}

#[test]
fn contextual_id_and_plural_lookups_have_english_fallbacks() {
    static TEST: LangInfo = LangInfo {
        code: "test",
        name: "Test catalog",
        source: "\tLight\tPlain light\nweight\tLight\tThin\n@id\tfile.open\tOpen dialog…\n@plural\t{n} page|{n} pages\tOne page: {n}|Many pages: {n}\n",
        plural: plural_one_other,
        rtl: false,
        catalog: OnceLock::new(),
    };
    let l = Lang(&TEST);
    assert_eq!(tr_ctx(l, "weight", "Light"), "Thin");
    assert_eq!(tr_ctx(l, "theme", "Light"), "Plain light");
    assert_eq!(tr_ctx(l, "theme", "Unknown"), "Unknown");
    assert_eq!(tr_id(l, "file.open", "Reworded English…"), "Open dialog…");
    assert_eq!(tr_id(l, "unknown", "Light"), "Plain light");
    assert_eq!(tr_id(l, "unknown", "Unknown"), "Unknown");
    assert_eq!(trn(l, 1, "{n} page", "{n} pages"), "One page: 1");
    assert_eq!(trn(l, 0, "{n} page", "{n} pages"), "Many pages: 0");
    assert_eq!(trn(l, u64::MAX, "{n} page", "{n} pages"), format!("Many pages: {}", u64::MAX));
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
    let english = pdfcraft_engine::sign::AppearanceLabels::default();
    for language in LANGUAGES.iter().filter(|language| !language.source.is_empty()) {
        let lang = Lang::from_code(language.code).expect("registered language");
        for (field, label) in english.fields() {
            let said = tr(lang, label);
            if field != "signed_by" && !lang.rtl() {
                assert!(said.ends_with(' ') || said.ends_with('：'), "{}: the {field} label runs into its value: {said:?}", language.code);
            }
        }
    }
}

#[test]
fn history_labels_keep_captured_names() {
    set_current(RU());
    assert_eq!(action_label("Insert pages from Save {e}.pdf"), "Вставить страницы из Save {e}.pdf");
    assert_eq!(command_label("Undo Insert pages from Save {e}.pdf"), "Отменить Вставить страницы из Save {e}.pdf");
    assert_eq!(action_label("Untranslated custom action"), "Untranslated custom action");
    assert_eq!(menu_label("file.open", "Open…"), "Открыть…");
    set_current(Lang::EN);
    assert_eq!(command_label("Undo Fill in Save {e}"), "Undo Fill in Save {e}");
}

#[test]
fn current_language_is_per_thread() {
    set_current(RU());
    assert_eq!(t("File"), "Файл");
    std::thread::spawn(|| assert_eq!(current(), Lang::EN)).join().unwrap();
    set_current(Lang::EN);
    assert_eq!(t("File"), "File");
}

#[test]
fn plurals_and_placeholders() {
    assert_eq!(trn(Lang::EN, 1, "{n} item", "{n} items"), "1 item");
    assert_eq!(trn(Lang::EN, 0, "{n} item", "{n} items"), "0 items");
    assert_eq!(trn(Lang::EN, 7, "{n} item", "{n} items"), "7 items");
    // No catalog has this `@plural` entry: Russian falls back to the English forms.
    assert_eq!(trn(RU(), 7, "{n} test item", "{n} test items"), "7 test items");
    assert_eq!(fmt("{b} before {a}", &[("a", "x"), ("b", "y"), ("c", "z")]), "y before x");
    assert_eq!(fmt("{missing}", &[]), "{missing}");
    assert_eq!(placeholders("a {x} b {y} {"), ["x", "y"]);
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

/// A catalog without a whole-phrase "Undo {action}" entry (Russian) keeps the verb in front of
/// the action; English is unchanged.
#[test]
fn undo_and_redo_labels_put_the_verb_in_front_without_a_catalog_phrase() {
    let ru = RU();
    set_current(ru);
    assert_eq!(command_label("Undo"), "Отменить");
    assert_eq!(command_label("Undo Untranslated custom action"), format!("{} Untranslated custom action", tr(ru, "Undo")));
    set_current(Lang::EN);
    assert_eq!(command_label("Undo Untranslated custom action"), "Undo Untranslated custom action");
}

/// A label that reads differently by use has its own contextual entry where a language needs
/// one; a language without it falls back to the plain translation.
#[test]
fn contextual_labels_fall_back_to_the_plain_translation() {
    // A context no catalog defines, so the fallback stays tested as catalogs fill their gaps.
    assert_eq!(tr_ctx(RU(), "no such context", "Type"), tr(RU(), "Type"));
    assert_eq!(tr(RU(), "Type"), "Тип");
}

/// The code joins some labels with a space at either end, so these catalogs keep exactly the
/// source's, and write … rather than three dots.
#[test]
fn catalog_rows_keep_the_source_spacing_and_ellipsis() {
    let edges = |s: &str| (s.len() - s.trim_start().len(), s.len() - s.trim_end().len());
    {
        let code = "ru";
        let lang = Lang::from_code(code).expect("registered");
        let (entries, errors) = parse_entries(lang.0.source, lang.0.plural_forms());
        assert!(errors.is_empty(), "{code}: {errors:?}");
        for e in &entries {
            assert!(!e.translation.contains("..."), "{code}: use … rather than three dots: {:?}", e.translation);
            assert_eq!(edges(&e.translation), edges(&e.source), "{code}: stray whitespace: {:?}", e.translation);
        }
    }
}

#[test]
fn tool_names_inside_sentences_are_lowercased() {
    set_current(Lang::EN);
    assert_eq!(in_sentence("Text Box"), "text box");
    set_current(RU());
    assert_eq!(in_sentence("Текстовое поле"), "текстовое поле");
    set_current(Lang::EN);
}

#[test]
fn russian_is_registered() {
    let ru = Lang::from_code("ru").expect("ru registered");
    assert_eq!(ru.name(), "Русский");
    assert_eq!(normalize_pref("RU"), Some("ru"));
    assert_eq!(lang_from_tag("ru_RU.UTF-8"), Some(ru));
    assert_eq!(lang_from_tag("ru-BY"), Some(ru));
    assert_eq!(first_supported("ru-RU\r\nen-US"), Some(ru));
    assert_eq!(tr(ru, "Rapport de l'utilisateur.pdf"), "Rapport de l'utilisateur.pdf");
    // one (1), few (2-4), many (0, 5+), with 11-14 exception
    assert_eq!((0..=3).map(|n| (ru.0.plural)(n)).collect::<Vec<_>>(), [2, 0, 1, 1]);
    let mut app = crate::PdfCraftApp::default();
    app.set_option("language", "ru").unwrap();
    assert_eq!(app.language, "ru");
    let mut restored = crate::PdfCraftApp::default();
    restored.restore(&app.persist());
    assert_eq!(restored.language, "ru");
}

/// Every bundled catalog is well-formed and consistent with its sources.
#[test]
fn bundled_catalogs_are_consistent() {
    let mut codes = std::collections::HashSet::new();
    for l in &LANGUAGES {
        assert!(codes.insert(l.code), "duplicate code {}", l.code);
        assert!(l.code == l.code.to_ascii_lowercase() && !l.name.is_empty(), "{}", l.code);
        let (entries, errors) = parse_entries(l.source, l.plural_forms());
        assert!(errors.is_empty(), "{}: {errors:?}", l.code);
        for e in entries.iter().filter(|e| e.context == "@id") {
            let command = pdfcraft_engine::commands::command(&e.source);
            let Some(command) = command else { panic!("{}: unknown command id {:?}", l.code, e.source) };
            assert_eq!(placeholders(&e.translation), placeholders(command.label), "{}: {}", l.code, e.source);
            assert_eq!(e.translation.ends_with('…'), command.label.ends_with('…'), "{}: ellipsis mismatch: {}", l.code, e.source);
        }
    }
}

/// The translation gaps report (`cargo xtask i18n-report`, a step of `cargo xtask ci`): for
/// each language, how many English strings it has no translation for, and the first few.
/// It never fails: a missing string shows in English.
///
/// The English strings are the UI source's literals, the commands, All tools, the engine's
/// own wordings, and every key of every bundled catalog (which covers contextual, plural and
/// dynamically looked-up strings the scan can't see).
#[test]
fn translation_gaps_report() {
    use std::collections::BTreeSet;
    const SHOWN: usize = 3;
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
    for language in &LANGUAGES {
        let (entries, _) = parse_entries(language.source, language.plural_forms());
        keys.extend(entries.into_iter().filter(|e| e.context != "@id").map(|e| (e.context, e.source)));
    }
    let mut report = format!("Translation gaps: {} English strings\n", keys.len());
    for lang in Lang::all().filter(|l| *l != Lang::EN) {
        let catalog = lang.catalog();
        let missing: Vec<&(String, String)> = keys
            .iter()
            .filter(|(context, source)| match context.as_str() {
                "" => catalog.plain(source).is_none(),
                "@plural" => source.split_once('|').is_none_or(|(one, other)| catalog.plural(one, other, 0).is_none()),
                // A contextual lookup falls back to the plain translation.
                context => catalog.contextual(context, source).is_none() && catalog.plain(source).is_none(),
            })
            .collect();
        let mut line = format!("  {:<8} {:>5} missing", lang.code(), missing.len());
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
    println!("{REPORT_START}\n{report}{REPORT_END}");
}

/// The lines `cargo xtask i18n-report` prints from the test output lie between these.
const REPORT_START: &str = "=== translation gaps ===";
const REPORT_END: &str = "=== end of translation gaps ===";

/// Preferences ▸ Date format ▸ Language offers exactly the interface languages, in order.
#[test]
fn every_interface_language_has_its_date_language() {
    // PdfCraft Plus ships fewer interface languages than upstream; dates written into PDFs
    // keep upstream's full list, which must still include every interface language.
    let dates: Vec<(&str, &str)> = pdfcraft_engine::dates::DATE_LANGUAGES.iter().map(|l| (l.code, l.name)).collect();
    for l in Lang::all() {
        assert!(dates.contains(&(l.code(), l.name())), "{} has no date language", l.code());
    }
}
