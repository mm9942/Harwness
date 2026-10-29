//! Session-specific translation of TUI-owned interface messages.
//!
//! Catalogs are embedded by component zone and loaded once. Use explicit-language
//! helpers across async boundaries; [`with_language`] is synchronous thread-local
//! render/handler context only. See `locales/README.md` for the catalog contract.

use harw_config::UiLanguage;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::OnceLock;

// Register each new component zone here; every JSON key starts with that zone.
const CATALOG_SOURCES: &[(&str, &str)] = &[
    ("dispatch", include_str!("locales/dispatch.json")),
    ("common", include_str!("locales/common.json")),
    ("settings", include_str!("locales/settings.json")),
];
type Catalog = BTreeMap<String, BTreeMap<String, String>>;

thread_local! {
    static LANGUAGE: Cell<UiLanguage> = const { Cell::new(UiLanguage::English) };
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut catalog = Catalog::new();
        for &(zone, source) in CATALOG_SOURCES {
            let entries: Catalog = serde_json::from_str(source)
                .unwrap_or_else(|error| panic!("invalid {zone} TUI catalog: {error}"));
            for (key, translations) in entries {
                assert!(
                    catalog.insert(key.clone(), translations).is_none(),
                    "duplicate TUI catalog key: {key}"
                );
            }
        }
        catalog
    })
}

fn lookup<'a>(catalog: &'a Catalog, language: UiLanguage, key: &str, english: &'a str) -> &'a str {
    catalog
        .get(key)
        .and_then(|translations| translations.get(language.as_str()))
        .map(String::as_str)
        .filter(|message| !message.is_empty())
        .unwrap_or(english)
}

/// Translate a fixed interface label using the current synchronous scope.
///
/// Outside [`with_language`] the language is English. Missing translations return
/// `english`, which must exactly match the catalog's `en` message.
#[must_use]
pub fn tr(key: &str, english: &'static str) -> &'static str {
    tr_for(LANGUAGE.get(), key, english)
}

/// Translate a fixed interface label for a session's explicit language.
#[must_use]
pub fn tr_for(language: UiLanguage, key: &str, english: &'static str) -> &'static str {
    lookup(catalog(), language, key, english)
}

/// Translate and substitute named arguments using the current synchronous scope.
///
/// Argument values are copied literally, even when they contain placeholders.
/// Missing arguments remain as placeholders; extra arguments are ignored. If a
/// name is supplied more than once, its first value wins.
#[must_use]
pub fn tr_args(key: &str, english: &str, args: &[(&str, &str)]) -> String {
    tr_args_for(LANGUAGE.get(), key, english, args)
}

/// Translate and substitute named arguments for a session's explicit language.
///
/// Only the selected template is scanned, once. No inserted text is reprocessed.
#[must_use]
pub fn tr_args_for(
    language: UiLanguage,
    key: &str,
    english: &str,
    args: &[(&str, &str)],
) -> String {
    interpolate(lookup(catalog(), language, key, english), args)
}

fn interpolate(message: &str, args: &[(&str, &str)]) -> String {
    let mut output = String::with_capacity(message.len());
    let mut remaining = message;
    while let Some(start) = remaining.find('{') {
        output.push_str(&remaining[..start]);
        remaining = &remaining[start..];
        let Some(end) = remaining.find('}') else {
            break;
        };
        let name = &remaining[1..end];
        let valid_name = name
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        let value = if valid_name {
            args.iter()
                .find(|(argument, _)| *argument == name)
                .map(|(_, value)| *value)
        } else {
            None
        };
        output.push_str(value.unwrap_or(&remaining[..=end]));
        remaining = &remaining[end + 1..];
    }
    output.push_str(remaining);
    output
}

/// Run a synchronous render or handler in one session's language.
///
/// Nested scopes and unwinding restore the previous language. The scope is local
/// to this thread; new threads start in English. Never span an await or poll an
/// async runtime inside this closure. Returning a future does not scope its later
/// execution: use [`tr_for`] and [`tr_args_for`] across async boundaries instead.
pub fn with_language<R>(language: UiLanguage, action: impl FnOnce() -> R) -> R {
    struct RestoreLanguage(UiLanguage);

    impl Drop for RestoreLanguage {
        fn drop(&mut self) {
            LANGUAGE.set(self.0);
        }
    }

    let _restore = RestoreLanguage(LANGUAGE.replace(language));
    action()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Barrier};

    #[test]
    fn catalogs_have_complete_locale_and_placeholder_parity() {
        assert!(!CATALOG_SOURCES.is_empty());
        let codes: BTreeSet<_> = UiLanguage::ALL
            .map(UiLanguage::as_str)
            .into_iter()
            .collect();
        let mut keys = BTreeSet::new();
        for (zone, source) in CATALOG_SOURCES {
            let entries: BTreeMap<String, BTreeMap<String, String>> =
                serde_json::from_str(source).expect("valid catalog JSON");
            assert!(!entries.is_empty(), "empty {zone} catalog");
            for (key, translations) in entries {
                assert!(key.starts_with(&format!("{zone}.")), "{key}");
                assert!(keys.insert(key.clone()), "duplicate key {key}");
                assert_eq!(
                    translations
                        .keys()
                        .map(String::as_str)
                        .collect::<BTreeSet<_>>(),
                    codes,
                    "{key} locale coverage"
                );
                let english = &translations["en"];
                let expected = placeholders(english);
                for language in UiLanguage::ALL {
                    let message = &translations[language.as_str()];
                    assert!(!message.trim().is_empty(), "{key}: {language:?}");
                    assert_eq!(placeholders(message), expected, "{key}: {language:?}");
                    assert_eq!(
                        tr_args_for(language, &key, english, &[]),
                        *message,
                        "{key} lookup: {language:?}"
                    );
                }
            }
        }
    }

    fn placeholders(message: &str) -> BTreeMap<&str, usize> {
        let mut names = BTreeMap::new();
        for part in message.split('{').skip(1) {
            let (name, _) = part.split_once('}').expect("closed catalog placeholder");
            assert!(
                name.starts_with(
                    |character: char| character.is_ascii_alphabetic() || character == '_'
                )
            );
            assert!(
                name.bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            );
            *names.entry(name).or_default() += 1;
        }
        names
    }

    #[test]
    fn activation_label_is_exact_in_every_language() {
        for (language, expected) in [
            (UiLanguage::English, "Activated"),
            (UiLanguage::German, "Aktiviert"),
            (UiLanguage::Spanish, "Activado"),
            (UiLanguage::French, "Activé"),
            (UiLanguage::Portuguese, "Ativado"),
            (UiLanguage::Japanese, "有効"),
            (UiLanguage::ChineseSimplified, "已激活"),
            (UiLanguage::Korean, "활성화됨"),
            (UiLanguage::Vietnamese, "Đã kích hoạt"),
            (UiLanguage::Hindi, "सक्रिय"),
        ] {
            assert_eq!(tr_for(language, "common.activated", "Activated"), expected);
        }
    }

    #[test]
    fn unknown_keys_fall_back_to_english_instead_of_exposing_keys() {
        for language in UiLanguage::ALL {
            assert_eq!(
                tr_for(language, "missing.internal_key", "Fallback"),
                "Fallback"
            );
            assert_eq!(
                tr_args_for(
                    language,
                    "missing.with_args",
                    "Hello, {name}",
                    &[("name", "Mia")]
                ),
                "Hello, Mia"
            );
        }
    }

    #[test]
    fn missing_or_empty_locale_entries_use_the_callers_fallback() {
        let sparse: Catalog = serde_json::from_str(
            r#"{"example.label":{"en":"Catalog English","de":"","fr":"Français"}}"#,
        )
        .expect("sparse fixture");
        assert_eq!(
            lookup(&sparse, UiLanguage::French, "example.label", "Fallback"),
            "Français"
        );
        assert_eq!(
            lookup(&sparse, UiLanguage::German, "example.label", "Fallback"),
            "Fallback"
        );
        assert_eq!(
            lookup(&sparse, UiLanguage::Japanese, "example.label", "Fallback"),
            "Fallback"
        );
        assert_eq!(
            lookup(&sparse, UiLanguage::French, "example.missing", "Fallback"),
            "Fallback"
        );
    }

    #[test]
    fn interpolation_never_reprocesses_inserted_user_text() {
        assert_eq!(
            tr_args_for(
                UiLanguage::German,
                "common.error_detail",
                "Error: {message}",
                &[("message", "user {other} {message}"), ("other", "REPLACED")]
            ),
            "Fehler: user {other} {message}"
        );
        assert_eq!(
            tr_args_for(
                UiLanguage::English,
                "missing",
                "{second}/{first}/{second}/{missing}",
                &[
                    ("first", "α"),
                    ("second", "β"),
                    ("second", "ignored"),
                    ("extra", "unused")
                ]
            ),
            "β/α/β/{missing}"
        );
    }

    #[test]
    fn interpolation_preserves_literal_and_unclosed_braces() {
        assert_eq!(
            tr_args_for(
                UiLanguage::English,
                "missing",
                "{{name}} {not a name} {} {name} tail {",
                &[("name", "Mia"), ("not a name", "wrong"), ("", "wrong")]
            ),
            "{{name}} {not a name} {} Mia tail {"
        );
    }

    #[test]
    fn nested_session_scopes_restore_the_previous_language() {
        assert_eq!(tr("common.activated", "Activated"), "Activated");
        let returned = with_language(UiLanguage::German, || {
            assert_eq!(tr("common.activated", "Activated"), "Aktiviert");
            assert_eq!(
                tr_args(
                    "common.error_detail",
                    "Error: {message}",
                    &[("message", "x")]
                ),
                "Fehler: x"
            );
            with_language(UiLanguage::French, || {
                assert_eq!(tr("common.activated", "Activated"), "Activé");
                assert_eq!(
                    tr_for(UiLanguage::English, "common.activated", "Activated"),
                    "Activated"
                );
            });
            assert_eq!(tr("common.activated", "Activated"), "Aktiviert");
            42
        });
        assert_eq!(returned, 42);
        assert_eq!(tr("common.activated", "Activated"), "Activated");
    }

    #[test]
    fn panics_restore_inner_and_outer_scopes() {
        let result = std::panic::catch_unwind(|| {
            with_language(UiLanguage::German, || {
                let inner = std::panic::catch_unwind(|| {
                    with_language(UiLanguage::Japanese, || {
                        assert_eq!(tr("common.activated", "Activated"), "有効");
                        panic!("inner session panic");
                    });
                });
                assert_eq!(
                    inner.expect_err("inner panic").downcast_ref::<&str>(),
                    Some(&"inner session panic")
                );
                assert_eq!(tr("common.activated", "Activated"), "Aktiviert");
                panic!("outer session panic");
            });
        });
        assert_eq!(
            result.expect_err("outer panic").downcast_ref::<&str>(),
            Some(&"outer session panic")
        );
        assert_eq!(tr("common.activated", "Activated"), "Activated");
    }

    #[test]
    fn simultaneous_threads_have_independent_session_languages() {
        let barrier = Arc::new(Barrier::new(3));
        let threads: Vec<_> = [
            (UiLanguage::German, "Aktiviert"),
            (UiLanguage::French, "Activé"),
        ]
        .into_iter()
        .map(|(language, expected)| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let initial = tr("common.activated", "Activated");
                let observed = with_language(language, || {
                    barrier.wait();
                    let observed = tr("common.activated", "Activated");
                    barrier.wait();
                    observed
                });
                assert_eq!(initial, "Activated");
                assert_eq!(observed, expected);
                assert_eq!(tr("common.activated", "Activated"), "Activated");
            })
        })
        .collect();
        barrier.wait();
        let parent_language = tr("common.activated", "Activated");
        barrier.wait();
        assert_eq!(parent_language, "Activated");
        for thread in threads {
            thread.join().expect("session thread");
        }
    }
}
