//! The language of the strings the Rust side hands to the UI.
//!
//! The Slint strings are translated by Slint's own bundled `.po` catalogs
//! (`crates/starpod-ui/translations`). The strings built in Rust (the
//! connect reasons, the traffic and expiry texts, banners) go through this
//! module instead. Each Russian string is keyed by its English source text,
//! the way gettext keys its catalogs, so the English code reads as it is and
//! an untranslated string falls back to English.
//!
//! The current language is one global. The service sets it when it builds a
//! snapshot, from the saved preference. `STARPOD_UI_LANG` (`en` or `ru`)
//! overrides the preference, for the screenshot tool.

use std::sync::atomic::{AtomicU8, Ordering};

/// A language the UI ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
}

impl Lang {
    /// The locale tag, as Slint's `select_bundled_translation` and the
    /// catalog directory name it.
    pub const fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ru => "ru",
        }
    }
}

/// The environment variable that overrides the saved language.
pub const LANG_ENV: &str = "STARPOD_UI_LANG";

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// The language the Rust-side strings are in right now.
pub fn current() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::Ru,
        _ => Lang::En,
    }
}

/// Make `lang` the language of the Rust-side strings.
pub fn set(lang: Lang) {
    CURRENT.store(
        match lang {
            Lang::En => 0,
            Lang::Ru => 1,
        },
        Ordering::Relaxed,
    );
}

/// Serializes the tests that set the language, so a test that pins Russian
/// cannot race one that expects English on another test thread.
#[cfg(test)]
static TEST_LANG: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A test's hold on the language. While it lives the language is the one it
/// pinned; on drop the language goes back to English.
#[cfg(test)]
pub(crate) struct PinnedLang {
    _guard: std::sync::MutexGuard<'static, ()>,
}

/// Pin the Rust-side language to `lang` for the rest of a test, so the test
/// does not depend on the machine's locale.
#[cfg(test)]
pub(crate) fn pin(lang: Lang) -> PinnedLang {
    let guard = TEST_LANG
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    set(lang);
    PinnedLang { _guard: guard }
}

#[cfg(test)]
impl Drop for PinnedLang {
    fn drop(&mut self) {
        set(Lang::En);
    }
}

/// The language for a saved preference: `"en"` or `"ru"` pin it, anything
/// else (the empty string, the default) follows the system locale.
pub fn resolve(pref: &str, system_locale: Option<&str>) -> Lang {
    match pref.trim() {
        "en" => Lang::En,
        "ru" => Lang::Ru,
        _ => lang_of_locale(system_locale.unwrap_or("")),
    }
}

/// The language for a locale such as `ru-RU` or `ru_RU.UTF-8`. Only Russian
/// is translated, so every other locale is English.
pub fn lang_of_locale(locale: &str) -> Lang {
    let primary = locale
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if primary == "ru" { Lang::Ru } else { Lang::En }
}

/// The language to use: the `STARPOD_UI_LANG` override if it is set to a
/// known language, otherwise the saved preference, otherwise the system.
pub fn effective(pref: &str) -> Lang {
    let override_value = std::env::var(LANG_ENV).ok();
    let system = sys_locale::get_locale();
    effective_with(pref, override_value.as_deref(), system.as_deref())
}

/// `effective` with its inputs given, so the rule can be tested.
pub fn effective_with(pref: &str, override_value: Option<&str>, system: Option<&str>) -> Lang {
    match override_value.map(|value| value.trim().to_ascii_lowercase()) {
        Some(value) if value == "en" => Lang::En,
        Some(value) if value == "ru" => Lang::Ru,
        _ => resolve(pref, system),
    }
}

/// The translation of `text` in `lang`, or `text` itself when it has none.
pub fn t_in(lang: Lang, text: &'static str) -> &'static str {
    let russian = RU.iter().find(|(en, _)| *en == text).map(|(_, ru)| *ru);
    match (lang, russian) {
        (Lang::Ru, Some(ru)) => ru,
        _ => text,
    }
}

/// The translation of `text` in the current language.
pub fn t(text: &'static str) -> &'static str {
    t_in(current(), text)
}

/// `text` translated in `lang`, with each `{}` replaced by the next argument.
pub fn fmt_in(lang: Lang, text: &'static str, args: &[&str]) -> String {
    fill(t_in(lang, text), args)
}

/// `text` translated in the current language, with its `{}` filled in.
pub fn fmt(text: &'static str, args: &[&str]) -> String {
    fmt_in(current(), text, args)
}

fn fill(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    let mut args = args.iter();
    while let Some(at) = rest.find("{}") {
        out.push_str(&rest[..at]);
        if let Some(arg) = args.next() {
            out.push_str(arg);
        }
        rest = &rest[at + 2..];
    }
    out.push_str(rest);
    out
}

/// English source text and its Russian translation.
const RU: &[(&str, &str)] = &[
    (
        "The accent colour must be written as #rrggbb, for example #de6161",
        "Цвет акцента записывается как #rrggbb, например #de6161",
    ),
    // Why Connect is blocked (snapshot.rs).
    ("Install the engine first.", "Сначала установите ядро."),
    ("Add a subscription first", "Сначала добавьте подписку"),
    (
        "Refresh your subscription — it has no servers yet",
        "Обновите подписку — в ней пока нет серверов",
    ),
    (
        "This subscription has reached its device limit. Contact support to free a slot.",
        "Для этой подписки достигнут лимит устройств. Свяжитесь с поддержкой, чтобы освободить место.",
    ),
    // Byte units and traffic (ui/mapping.rs).
    ("B", "Б"),
    ("KB", "КБ"),
    ("MB", "МБ"),
    ("GB", "ГБ"),
    ("TB", "ТБ"),
    ("{} of {}", "{} из {}"),
    ("{} used", "{} использовано"),
    ("{} left", "осталось {}"),
    ("Unlimited", "Без лимита"),
    ("Expires {}", "Истекает {}"),
    ("Expired {}", "Истекла {}"),
    ("{}/s", "{}/с"),
    // Uptime (ui/mapping.rs).
    ("{}h {}m", "{}ч {}м"),
    ("{}m {}s", "{}м {}с"),
    ("{}s", "{}с"),
    ("{} ms", "{} мс"),
    ("timeout", "таймаут"),
    // Banners (ui/runtime.rs, ui/callbacks.rs).
    (
        "Starpod {} is available",
        "Доступна новая версия Starpod {}",
    ),
    ("the link was not used: {}", "ссылка не использована: {}"),
    (
        "could not copy the logs: {}",
        "не удалось скопировать журнал: {}",
    ),
    (
        "the link could not be opened: {}",
        "не удалось открыть ссылку: {}",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_russian_or_regional_russian_system_locale_selects_russian() {
        assert_eq!(lang_of_locale("ru-RU"), Lang::Ru);
        assert_eq!(lang_of_locale("ru_RU.UTF-8"), Lang::Ru);
        assert_eq!(lang_of_locale("ru"), Lang::Ru);
    }

    #[test]
    fn every_other_system_locale_is_english() {
        assert_eq!(lang_of_locale("de-DE"), Lang::En);
        assert_eq!(lang_of_locale("en_US.UTF-8"), Lang::En);
        assert_eq!(lang_of_locale("rust"), Lang::En);
        assert_eq!(lang_of_locale(""), Lang::En);
    }

    #[test]
    fn an_empty_preference_follows_the_system_and_a_pinned_one_wins() {
        assert_eq!(resolve("", Some("ru-RU")), Lang::Ru);
        assert_eq!(resolve("", Some("fr-FR")), Lang::En);
        assert_eq!(resolve("", None), Lang::En);
        assert_eq!(resolve("en", Some("ru-RU")), Lang::En);
        assert_eq!(resolve("ru", Some("de-DE")), Lang::Ru);
        assert_eq!(resolve("klingon", Some("ru-RU")), Lang::Ru);
    }

    #[test]
    fn the_environment_override_beats_the_saved_preference() {
        assert_eq!(effective_with("en", Some("ru"), Some("de-DE")), Lang::Ru);
        assert_eq!(effective_with("ru", Some(" EN "), Some("ru-RU")), Lang::En);
        assert_eq!(effective_with("ru", Some("fr"), None), Lang::Ru);
        assert_eq!(effective_with("", None, Some("ru-RU")), Lang::Ru);
    }

    #[test]
    fn a_russian_string_is_translated_and_english_is_unchanged() {
        assert_eq!(
            t_in(Lang::Ru, "Install the engine first."),
            "Сначала установите ядро."
        );
        assert_eq!(
            t_in(Lang::En, "Install the engine first."),
            "Install the engine first."
        );
        // A string with no translation falls back to English.
        assert_eq!(t_in(Lang::Ru, "No such message"), "No such message");
    }

    #[test]
    fn a_pinned_russian_language_gives_russian_rust_side_strings() {
        let _lang = pin(Lang::Ru);
        assert_eq!(t("Install the engine first."), "Сначала установите ядро.");
        assert_eq!(fmt("Expires {}", &["14.11.2026"]), "Истекает 14.11.2026");
        drop(_lang);
        let _english = pin(Lang::En);
        assert_eq!(t("Install the engine first."), "Install the engine first.");
    }

    #[test]
    fn placeholders_are_filled_in_order_in_either_language() {
        assert_eq!(
            fmt_in(Lang::En, "{} of {}", &["12.3 GB", "100 GB"]),
            "12.3 GB of 100 GB"
        );
        assert_eq!(
            fmt_in(Lang::Ru, "{} of {}", &["12.3 ГБ", "100 ГБ"]),
            "12.3 ГБ из 100 ГБ"
        );
        assert_eq!(fmt_in(Lang::Ru, "{} left", &["1 ГБ"]), "осталось 1 ГБ");
    }
}
