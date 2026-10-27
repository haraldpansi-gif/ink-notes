//! Sprache der Oberfläche: Deutsch oder Englisch.
//!
//! Die Texte stehen im Quelltext auf Deutsch in `t!("…")`; dieser deutsche Text ist zugleich
//! der Schlüssel für die englische Fassung in `en.rs`. Platzhalter heißen `{name}` und werden
//! mit `t!("Seite {number}", number = n)` gefüllt (Formatangaben wie `{:.1}` vorher mit
//! `format!` anwenden). Texte in Tabellen markiert `tn!("…")`, übersetzt werden sie beim
//! Anzeigen mit `tr(…)`. Ein Test prüft, dass jeder markierte Text übersetzt ist.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

mod en;

/// Sprache der Oberfläche
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    German,
    English,
}

impl Language {
    pub const ALL: [Language; 2] = [Language::German, Language::English];

    /// Name der Sprache in ihr selbst (für die Auswahl, unabhängig von der aktuellen Sprache)
    pub fn native_name(self) -> &'static str {
        match self {
            Language::German => "Deutsch",
            Language::English => "English",
        }
    }

    /// Sprache aus den Locale-Variablen (`LC_ALL`, `LC_MESSAGES`, `LANG`, `LANGUAGE`; `C` und
    /// `POSIX` zählen nicht): Deutsch bei `de…`, sonst Englisch.
    pub fn from_locale(vars: &[Option<String>]) -> Language {
        let locale = vars
            .iter()
            .flatten()
            .map(|v| v.trim())
            .find(|v| !v.is_empty() && *v != "C" && !v.starts_with("C.") && *v != "POSIX");
        match locale {
            Some(v) if v.to_ascii_lowercase().starts_with("de") => Language::German,
            _ => Language::English,
        }
    }

    /// Sprache des Systems
    pub fn system() -> Language {
        let var = |name| std::env::var(name).ok();
        Language::from_locale(&[
            var("LC_ALL"),
            var("LC_MESSAGES"),
            var("LANG"),
            var("LANGUAGE"),
        ])
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Stellt die Sprache der Oberfläche um (wirkt ab dem nächsten Frame).
pub fn set_language(language: Language) {
    CURRENT.store(language as u8, Ordering::Relaxed);
}

/// Aktuelle Sprache der Oberfläche
pub fn language() -> Language {
    match CURRENT.load(Ordering::Relaxed) {
        0 => Language::German,
        _ => Language::English,
    }
}

fn english() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| en::EN.iter().copied().collect())
}

/// Text `german` in der Sprache `language`; fehlt die Übersetzung, bleibt er deutsch.
pub fn translate(language: Language, german: &'static str) -> &'static str {
    match language {
        Language::German => german,
        Language::English => english().get(german).copied().unwrap_or(german),
    }
}

/// Text in der aktuellen Sprache
pub fn tr(german: &'static str) -> &'static str {
    translate(language(), german)
}

/// Setzt die Werte für die Platzhalter `{name}` ein.
pub fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match args.iter().find(|(key, _)| *key == name) {
                    Some((_, value)) => out.push_str(&value.to_string()),
                    None => out.push_str(&rest[open..open + close + 2]),
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Übersetzt einen deutschen Text: `t!("Abbrechen")` ergibt `&'static str`,
/// `t!("Seite {number}", number = n)` einen `String` mit eingesetzten Werten.
#[macro_export]
macro_rules! t {
    ($german:literal) => {
        $crate::i18n::tr($german)
    };
    ($german:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::fill(
            $crate::i18n::tr($german),
            &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+],
        )
    };
}

/// Markiert einen deutschen Text zum Übersetzen, ohne ihn schon zu übersetzen (für Tabellen;
/// angezeigt wird er mit `tr`).
#[macro_export]
macro_rules! tn {
    ($german:literal) => {
        $german
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quelltexte mit übersetzbaren Texten
    const SOURCES: &[(&str, &str)] = &[
        ("assets.rs", include_str!("assets.rs")),
        ("autosave.rs", include_str!("autosave.rs")),
        ("background.rs", include_str!("background.rs")),
        ("document.rs", include_str!("document.rs")),
        ("export.rs", include_str!("export.rs")),
        ("file_picker.rs", include_str!("file_picker.rs")),
        ("geometry.rs", include_str!("geometry.rs")),
        ("library.rs", include_str!("library.rs")),
        ("app.rs", include_str!("app.rs")),
        ("page_export.rs", include_str!("page_export.rs")),
        ("pdf_doc.rs", include_str!("pdf_doc.rs")),
        ("pdf_merge.rs", include_str!("pdf_merge.rs")),
        ("canvas.rs", include_str!("canvas.rs")),
        ("selection.rs", include_str!("selection.rs")),
        ("settings.rs", include_str!("settings.rs")),
        ("storage.rs", include_str!("storage.rs")),
        ("text.rs", include_str!("text.rs")),
        ("ui.rs", include_str!("ui.rs")),
        ("updater.rs", include_str!("updater.rs")),
        ("import.rs", include_str!("import.rs")),
        ("rnote.rs", include_str!("rnote.rs")),
        ("saber.rs", include_str!("saber.rs")),
        ("xopp.rs", include_str!("xopp.rs")),
    ];

    /// Alle Texte in `t!("…")` und `tn!("…")` eines Quelltexts, auch über mehrere Zeilen
    /// (Escapes wie `\n` aufgelöst)
    fn marked_texts(src: &str) -> Vec<String> {
        let chars: Vec<char> = src.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let rest: String = chars[i..chars.len().min(i + 4)].iter().collect();
            let marker_len = if rest.starts_with("t!(") {
                3
            } else if rest.starts_with("tn!(") {
                4
            } else {
                i += 1;
                continue;
            };
            // `t!(` nicht als Ende eines längeren Namens (z. B. `format!(`) zählen
            if i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
                i += 1;
                continue;
            }
            i += marker_len;
            while chars.get(i).is_some_and(|c| c.is_whitespace()) {
                i += 1;
            }
            if chars.get(i) != Some(&'"') {
                continue;
            }
            i += 1;
            let mut text = String::new();
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    i += 1;
                    match chars.get(i) {
                        Some('n') => text.push('\n'),
                        // Zeilenfortsetzung: Umbruch und führende Leerzeichen entfallen
                        Some('\n') => {
                            while chars.get(i + 1).is_some_and(|c| c.is_whitespace()) {
                                i += 1;
                            }
                        }
                        Some(other) => text.push(*other),
                        None => {}
                    }
                } else {
                    text.push(chars[i]);
                }
                i += 1;
            }
            out.push(text);
        }
        out
    }

    fn placeholders(text: &str) -> Vec<&str> {
        let mut names: Vec<&str> = text
            .split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name))
            .collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn every_marked_text_has_an_english_translation() {
        let table = english();
        let mut missing: Vec<String> = SOURCES
            .iter()
            .flat_map(|(file, src)| {
                marked_texts(src)
                    .into_iter()
                    .filter(|text| !table.contains_key(text.as_str()))
                    .map(move |text| format!("{file}: {text:?}"))
            })
            .collect();
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "Ohne Übersetzung:\n{}",
            missing.join("\n")
        );
    }

    #[test]
    fn every_translation_is_used_and_keeps_its_placeholders() {
        let used: std::collections::HashSet<String> = SOURCES
            .iter()
            .flat_map(|(_, src)| marked_texts(src))
            .collect();
        let mut seen = std::collections::HashSet::new();
        for (german, english) in en::EN {
            assert!(seen.insert(german), "Doppelt: {german:?}");
            assert!(used.contains(*german), "Nicht (mehr) verwendet: {german:?}");
            assert_eq!(
                placeholders(german),
                placeholders(english),
                "Platzhalter verschieden: {german:?} / {english:?}"
            );
        }
    }

    #[test]
    fn translate_falls_back_to_german() {
        assert_eq!(translate(Language::German, "Abbrechen"), "Abbrechen");
        assert_eq!(translate(Language::English, "Abbrechen"), "Cancel");
        assert_eq!(
            translate(Language::English, "kein bekannter Text"),
            "kein bekannter Text"
        );
    }

    #[test]
    fn fill_replaces_named_placeholders() {
        assert_eq!(
            fill(
                "Seite {number} von {count}",
                &[("number", &3), ("count", &"7")]
            ),
            "Seite 3 von 7"
        );
        assert_eq!(fill("{unbekannt} bleibt", &[]), "{unbekannt} bleibt");
        assert_eq!(fill("offen {", &[]), "offen {");
    }

    #[test]
    fn locale_selects_language() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(
            Language::from_locale(&[None, None, s("de_AT.UTF-8")]),
            Language::German
        );
        assert_eq!(
            Language::from_locale(&[s("en_US.UTF-8"), None, s("de_DE.UTF-8")]),
            Language::English
        );
        assert_eq!(
            Language::from_locale(&[s("C"), None, s("de_DE.UTF-8")]),
            Language::German
        );
        assert_eq!(
            Language::from_locale(&[s("C.UTF-8"), None, s("de_DE.UTF-8")]),
            Language::German
        );
        assert_eq!(
            Language::from_locale(&[None, None, None]),
            Language::English
        );
        assert_eq!(
            Language::from_locale(&[None, None, s("fr_FR.UTF-8")]),
            Language::English
        );
    }
}
