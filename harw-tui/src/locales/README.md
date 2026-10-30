# TUI message catalogs

Only translate interface text owned by the TUI. Never pass rendered user/model
content, code, paths, command IDs, tool payloads, or historical transcript text
through a translation lookup. Insert dynamic values as arguments without changing
their contents.

## Zones and keys

Each `<zone>.json` is one JSON object. Keys start with the zone and describe the
message's role: `common.activated`, `settings.title`, or
`approval.request_title`. Shared labels belong in `common.json`; component text
belongs in the component's own zone. Translate complete messages rather than
joining separately translated fragments. Keep keys stable when wording changes.

Every key maps all ten supported locale codes to nonempty, complete strings:

```json
{
  "settings.language_changed": {
    "en": "Interface language: {language}",
    "de": "Oberflächensprache: {language}",
    "es": "Idioma de la interfaz: {language}",
    "fr": "Langue de l’interface : {language}",
    "pt": "Idioma da interface: {language}",
    "ja": "表示言語: {language}",
    "zh-CN": "界面语言：{language}",
    "ko": "인터페이스 언어: {language}",
    "vi": "Ngôn ngữ giao diện: {language}",
    "hi": "इंटरफ़ेस की भाषा: {language}"
  }
}
```

Catalogs are embedded with `include_str!` and parsed into an immutable `OnceLock`
once per process. They require no filesystem, environment, network, or service
access at runtime. To add a zone, add its JSON file and its `(zone, include_str!)`
entry to `CATALOG_SOURCES` in `../i18n.rs`. Do not add per-locale files or duplicate
keys across zones.

## Calling the helpers

```rust,ignore
use harw_config::UiLanguage;
use harw_tui::i18n::{tr, tr_args, tr_for, tr_args_for, with_language};

let label = tr_for(UiLanguage::German, "common.activated", "Activated");
let message = tr_args_for(
    UiLanguage::German,
    "settings.language_changed",
    "Interface language: {language}",
    &[("language", "Deutsch")],
);
with_language(UiLanguage::German, || {
    let label = tr("common.activated", "Activated");
    let error = tr_args("common.error_detail", "Error: {message}", &[("message", "x")]);
});
```

`tr` and `tr_for` take an English `&'static str` and return a `&'static str`.
`tr_args` and `tr_args_for` take `&str` and return an owned `String`. Explicit
helpers take `UiLanguage` first and never modify the current scope. The `en`
catalog value must exactly equal the English fallback at each call site. Unknown
keys, absent language entries, and empty translations return that fallback;
internal keys are never displayed. Catalog syntax errors are developer errors
and are caught by the focused tests before shipping.

The unscoped default is English. `with_language` applies only to its synchronous
closure on the current thread and restores its previous language after nested
calls and panics. Scope each synchronous render or handler for its own session.
**Never hold a locale scope across an await, poll an async runtime inside it, or
use it as an async task's language storage.** Store the selected language in the
session and use the explicit-language helpers across async boundaries. Returning
a future from the closure does not scope that future's later execution. New
threads start in English and do not inherit the caller's scope.

## Placeholders

Use named `{identifier}` placeholders: an ASCII letter or underscore first,
followed by ASCII letters, digits, or underscores. Every locale must preserve the
same placeholder names and occurrence counts as English, although their order
can change. Translation uses a single scan of the selected message, so braces in
an inserted user value are copied literally and never interpreted again. Repeated
placeholders are replaced at each occurrence. For duplicate argument names, the
first supplied value wins. Unknown placeholders remain visible, extra arguments
are ignored, and malformed or unclosed braces are copied literally. Literal
braces are not an escaping syntax; avoid ambiguous brace expressions in catalogs.
These helpers do not provide automatic pluralization or formatting.

## Adding messages or languages

1. Choose the owning zone and a stable, descriptive key. Reuse shared labels when
   their meaning matches; the English activation label is exactly `Activated`
   and its German value is exactly `Aktiviert`.
2. Add complete translations for `en`, `de`, `es`, `fr`, `pt`, `ja`, `zh-CN`,
   `ko`, `vi`, and `hi`, preserving placeholders. Add a call with the identical
   English fallback. Keep dynamic content in arguments.
3. Run `cargo test -p harw-tui --lib i18n::tests` and `cargo fmt --all -- --check`.
   The catalog test checks every registered key's zone, locale coverage,
   placeholder names/counts, nonempty messages, and runtime lookup.
4. For a new supported locale, first extend the typed `harw_config::UiLanguage`
   contract (`ALL`, serialization, parsing, native name), then add that code to
   **every** key in every zone and update the all-language activation test and
   this guide. New locales must not silently leave partial catalogs.
