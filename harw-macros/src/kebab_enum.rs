//! `#[derive(KebabEnum)]`-Expansion.
//!
//! Spec: AP W1-26e (KebabEnum-Derive-Makro).
//!
//! # Zweck
//! Fieldless Enums (Betriebsarten, Kategorien, Status-Werte, …) tauchen im
//! Workspace wiederholt mit einer handgeschriebenen kebab-case-`Display`/
//! `FromStr`-Paarung auf. Dieses Makro erzeugt beide Impls sowie eine
//! `ALL`-Konstante aus der reinen Varianten-Deklaration.
//!
//! # Anwendbarkeit
//! `#[derive(KebabEnum)]` ist ausschließlich auf Enums anwendbar, deren
//! Varianten **ausnahmslos** Unit-Varianten sind (keine Tuple- oder
//! Named-Varianten) und die mindestens eine Variante besitzen:
//!
//! ```ignore
//! #[derive(KebabEnum)]
//! pub enum Permission {
//!     Observer,
//!     Operator,
//!     #[kebab_enum(rename = "super-admin")]
//!     Maintainer,
//! }
//! ```
//!
//! Jede Nicht-Unit-Variante erzeugt einen Compile-Fehler.
//!
//! # Generierte API
//!
//! ```ignore
//! impl Permission {
//!     pub const ALL: &'static [Permission];
//!     pub fn as_str(&self) -> &'static str;
//! }
//! impl std::fmt::Display for Permission { .. }   // kanonischer kebab-case-Name
//! impl std::str::FromStr for Permission {         // akzeptiert kebab-case UND
//!     type Err = <error>;                         // snake_case, case-insensitiv
//!     ..
//! }
//! ```
//!
//! `ALL` listet alle Varianten in Deklarationsreihenfolge.
//!
//! # kebab-case-Konvertierungsregel
//!
//! Der `Ident` jeder Variante wird in Wörter zerlegt und mit `-` verbunden,
//! anschließend vollständig kleingeschrieben. Wortgrenzen entstehen an:
//!
//! 1. jedem expliziten Trennzeichen (`_`, `-`, Leerzeichen) im Bezeichner,
//! 2. dem Übergang von Klein-/Ziffernzeichen zu einem Großbuchstaben
//!    (`AddCriterion` → `add`, `criterion` → `add-criterion`),
//! 3. dem **Ende** eines Akronym-Laufs aus mehreren Großbuchstaben, wenn
//!    darauf ein Kleinbuchstabe folgt — der letzte Großbuchstabe des Laufs
//!    beginnt dann ein neues Wort.
//!
//! Regel 3 verhindert, dass Akronyme buchstabenweise auseinandergerissen
//! werden: `HTTPServer` → Lauf `HTTP`, danach beginnt mit `Server` ein neues
//! Wort (weil auf das `S` das kleine `e` folgt) → `http-server`, **nicht**
//! `h-t-t-p-server`. Ein Enum-Bezeichner, der komplett aus Großbuchstaben
//! besteht (z. B. `ID`), bildet dagegen ein einziges Wort (`id`), da kein
//! Kleinbuchstabe-Übergang eine Aufspaltung auslöst.
//!
//! Diese Regel ist implementiert in [`to_kebab_case`] und deckungsgleich mit
//! dem, was gängige Rename-Konventionen (z. B. `serde(rename_all = "kebab-case")`)
//! für Akronyme erwarten.
//!
//! `#[kebab_enum(rename = "...")]` an einer Variante überschreibt den
//! automatisch abgeleiteten Namen vollständig (auch für `Display`).
//!
//! # FromStr-Toleranz
//!
//! `FromStr::from_str` normalisiert die Eingabe (`_` → `-`, dann
//! `to_ascii_lowercase()`) und vergleicht sie gegen die ebenso normalisierte
//! kanonische Form jeder Variante. Dadurch werden kebab-case, SNAKE_CASE,
//! Snake_Case und beliebige Groß-/Kleinschreibung akzeptiert; `Display`
//! liefert stets ausschließlich die kanonische kebab-case-Form.
//!
//! Erzeugen zwei Varianten (nach Normalisierung, inkl. `rename`) denselben
//! Namen, ist das ein Compile-Fehler (sonst wäre `FromStr` für diesen Namen
//! nicht mehr eindeutig).
//!
//! # Attribute
//!
//! - `#[kebab_enum(error = "pfad::zu::Error")]` (Enum-Ebene) — Pfad des
//!   Fehlertyps für `FromStr::Err`. Default: `crate::error::InvalidId`,
//!   dieselbe Konvention wie bei `#[derive(HarwId)]` (siehe `id.rs`), damit
//!   ein Crate für beide Makros denselben Fehlertyp wiederverwenden kann.
//! - `#[kebab_enum(ctor = "methodenname")]` (Enum-Ebene) — Name der
//!   assoziierten Fehler-Konstruktorfunktion. Default: `unknown_variant`.
//! - `#[kebab_enum(case = "snake")]` (Enum-Ebene) — kanonische Namen in
//!   snake_case statt kebab-case (`CompleteDrain` → `complete_drain`), passend
//!   zu `serde(rename_all = "snake_case")`. Default bzw. `case = "kebab"`:
//!   kebab-case. `FromStr`/`parse` akzeptieren unabhängig davon beide
//!   Schreibweisen und beliebige Groß-/Kleinschreibung.
//! - `#[kebab_enum(parse_option)]` (Enum-Ebene) — erzeugt zusätzlich
//!   `pub fn parse(value: &str) -> Option<Self>`; wie `FromStr`, aber ohne
//!   Fehlertyp und mit abgeschnittenem Leerraum um die Eingabe.
//! - `#[kebab_enum(no_from_str)]` (Enum-Ebene) — erzeugt **kein** `FromStr`
//!   (dann sind `error`/`ctor` bedeutungslos und es braucht keinen Fehlertyp).
//! - `#[kebab_enum(rename = "...")]` (Varianten-Ebene) — überschreibt den
//!   kanonischen Namen dieser Variante.
//!
//! # Fehlertyp-Vertrag (wichtig für Konsumenten)
//!
//! Wie bei `HarwId` kann der Fehlertyp nicht im Proc-Macro-Crate liegen. Der
//! Konsument muss unter dem konfigurierten Pfad eine assoziierte Funktion
//! mit dieser Signatur bereitstellen (Default-Namen):
//!
//! ```ignore
//! impl crate::error::InvalidId {
//!     pub fn unknown_variant(type_name: &'static str, value: &str) -> Self { /* ... */ }
//! }
//! ```
//!
//! `type_name` ist der Enum-Name (z. B. `"Permission"`), `value` die
//! ursprüngliche, nicht normalisierte Eingabe. Ein `type Err = ()` wurde
//! bewusst verworfen: ohne Kontext (Enum-Name, unbekannter Wert) lässt sich
//! aus `()` keine brauchbare Fehlermeldung mehr rekonstruieren; ein
//! sprechender, aber weiterhin pfad-konfigurierbarer Fehlertyp behält beide
//! Eigenschaften (Aussagekraft und Entkopplung vom Proc-Macro-Crate).

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Data, DeriveInput, Fields, Ident, LitStr, Path, Variant};

/// Geparste `#[kebab_enum(...)]`-Konfiguration auf Enum-Ebene.
///
/// # Design-doc reference
/// Spec section "KebabEnum — Attribute" (Modul-Doc oben).
struct KebabEnumArgs {
    /// Pfad des Fehlertyps für `FromStr::Err`.
    error_path: Path,
    /// Name der assoziierten Fehler-Konstruktorfunktion (Typname, Wert → Fehler).
    ctor_ident: Ident,
    /// Kanonische Schreibweise der automatisch abgeleiteten Namen.
    case: Case,
    /// Zusätzlich `parse(&str) -> Option<Self>` erzeugen.
    parse_option: bool,
    /// Kein `FromStr` erzeugen.
    no_from_str: bool,
}

/// Kanonische Schreibweise der abgeleiteten Namen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Case {
    Kebab,
    Snake,
}

impl Case {
    fn separator(self) -> &'static str {
        match self {
            Self::Kebab => "-",
            Self::Snake => "_",
        }
    }
}

impl Default for KebabEnumArgs {
    fn default() -> Self {
        Self {
            error_path: syn::parse_quote!(crate::error::InvalidId),
            ctor_ident: Ident::new("unknown_variant", proc_macro2::Span::call_site()),
            case: Case::Kebab,
            parse_option: false,
            no_from_str: false,
        }
    }
}

/// Liest alle `#[kebab_enum(...)]`-Attribute auf Enum-Ebene ein.
///
/// # Errors
/// Liefert `syn::Error` für unbekannte Schlüssel oder einen ungültigen Pfad
/// bzw. Bezeichner in `error` / `ctor`.
///
/// # Design-doc reference
/// Spec section "KebabEnum — Attribute" (Modul-Doc oben).
fn parse_kebab_enum_args(attrs: &[Attribute]) -> syn::Result<KebabEnumArgs> {
    let mut args = KebabEnumArgs::default();

    for attr in attrs {
        if !attr.path().is_ident("kebab_enum") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("error") {
                let lit: LitStr = meta.value()?.parse()?;
                args.error_path = syn::parse_str(&lit.value()).map_err(|e| {
                    syn::Error::new_spanned(
                        &lit,
                        format!("`error` muss ein gültiger Pfad sein: {e}"),
                    )
                })?;
                Ok(())
            } else if meta.path.is_ident("ctor") {
                let lit: LitStr = meta.value()?.parse()?;
                args.ctor_ident = syn::parse_str(&lit.value()).map_err(|e| {
                    syn::Error::new_spanned(
                        &lit,
                        format!("`ctor` muss ein gültiger Bezeichner sein: {e}"),
                    )
                })?;
                Ok(())
            } else if meta.path.is_ident("case") {
                let lit: LitStr = meta.value()?.parse()?;
                args.case = match lit.value().as_str() {
                    "kebab" => Case::Kebab,
                    "snake" => Case::Snake,
                    other => {
                        return Err(syn::Error::new_spanned(
                            &lit,
                            format!("`case` muss \"kebab\" oder \"snake\" sein, nicht \"{other}\""),
                        ));
                    }
                };
                Ok(())
            } else if meta.path.is_ident("parse_option") {
                args.parse_option = true;
                Ok(())
            } else if meta.path.is_ident("no_from_str") {
                args.no_from_str = true;
                Ok(())
            } else {
                Err(meta.error(
                    "unbekanntes kebab_enum-Attribut auf Enum-Ebene; erwartet: \
                     error = \"...\", ctor = \"...\", case = \"...\", parse_option, no_from_str",
                ))
            }
        })?;
    }

    Ok(args)
}

/// Liest ein optionales `#[kebab_enum(rename = "...")]` auf einer Variante ein.
///
/// # Errors
/// Liefert `syn::Error` bei einem unbekannten Schlüssel auf Varianten-Ebene.
///
/// # Design-doc reference
/// Spec section "KebabEnum — Attribute" (Modul-Doc oben).
fn parse_variant_rename(variant: &Variant) -> syn::Result<Option<LitStr>> {
    let mut rename = None;

    for attr in &variant.attrs {
        if !attr.path().is_ident("kebab_enum") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                let lit: LitStr = meta.value()?.parse()?;
                rename = Some(lit);
                Ok(())
            } else {
                Err(meta.error(
                    "unbekanntes kebab_enum-Attribut auf Varianten-Ebene; erwartet: rename = \"...\"",
                ))
            }
        })?;
    }

    Ok(rename)
}

/// Wandelt einen Rust-Bezeichner (z. B. einen Enum-Varianten-`Ident`) in
/// kebab-case (bzw. snake_case, siehe [`Case`]) um.
///
/// Siehe Modul-Doc, Abschnitt "kebab-case-Konvertierungsregel", für die
/// vollständige Spezifikation der Wortgrenzen-Regeln (insbesondere den
/// Akronym-Sonderfall `HTTPServer` → `http-server`).
///
/// # Design-doc reference
/// Spec section "KebabEnum — kebab-case-Konvertierungsregel" (Modul-Doc oben).
fn to_case(ident: &str, case: Case) -> String {
    let chars: Vec<char> = ident.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();

    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' || c == ' ' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }

        if c.is_uppercase() {
            let prev_is_lower_or_digit = i > 0
                && chars[i - 1] != '_'
                && chars[i - 1] != '-'
                && chars[i - 1] != ' '
                && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit());
            let prev_is_upper = i > 0 && chars[i - 1].is_uppercase();
            let next_is_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();

            let starts_new_word =
                !current.is_empty() && (prev_is_lower_or_digit || (prev_is_upper && next_is_lower));
            if starts_new_word {
                words.push(std::mem::take(&mut current));
            }
        }

        current.extend(c.to_lowercase());
    }

    if !current.is_empty() {
        words.push(current);
    }

    words.join(case.separator())
}

/// Ein aufgelöster Varianten-Eintrag: Bezeichner plus kanonischer und
/// normalisierter (Groß-/Kleinschreibung sowie `_`/`-` vereinheitlicht) Name.
struct VariantInfo {
    ident: Ident,
    /// Von `Display`/`as_str` gelieferter Name (kebab-/snake-case oder `rename`-Wert).
    canonical: String,
    /// `canonical` mit `_` → `-` und vollständig kleingeschrieben; Basis für
    /// den `FromStr`-Vergleich und die Kollisionsprüfung.
    normalized: String,
}

/// Expander für das `KebabEnum`-Derive-Makro.
///
/// Validiert, dass `input` ein Enum aus ausschließlich Unit-Varianten ist,
/// löst pro Variante den kanonischen Namen auf (automatisch abgeleitet oder
/// per `rename`), prüft auf Namenskollisionen und erzeugt `ALL`, `as_str`,
/// `Display` und `FromStr`.
///
/// # Errors
/// - Kein Enum → `syn::Error`.
/// - Enum ohne Varianten → `syn::Error`.
/// - Nicht-Unit-Variante → `syn::Error`.
/// - Zwei Varianten mit identischem normalisiertem Namen → `syn::Error`.
/// - Ungültige `#[kebab_enum(...)]`-Konfiguration → `syn::Error` (siehe
///   [`parse_kebab_enum_args`], [`parse_variant_rename`]).
///
/// # Design-doc reference
/// Spec section "KebabEnum — generierte API" (Modul-Doc oben).
pub(crate) fn expand_kebab_enum(input: &DeriveInput) -> syn::Result<TokenStream> {
    let enum_name = &input.ident;
    let args = parse_kebab_enum_args(&input.attrs)?;

    let data = match &input.data {
        Data::Enum(data) => data,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "KebabEnum kann nur auf enums angewendet werden",
            ));
        }
    };

    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            "KebabEnum erfordert mindestens eine Variante",
        ));
    }

    let mut infos: Vec<VariantInfo> = Vec::with_capacity(data.variants.len());
    for variant in &data.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                &variant.ident,
                format!(
                    "KebabEnum erfordert ausschließlich Unit-Varianten; Variante `{}` hat Felder",
                    variant.ident
                ),
            ));
        }

        let rename = parse_variant_rename(variant)?;
        let canonical = match rename {
            Some(lit) => lit.value(),
            None => to_case(&variant.ident.to_string(), args.case),
        };
        let normalized = canonical.replace('_', "-").to_ascii_lowercase();

        infos.push(VariantInfo {
            ident: variant.ident.clone(),
            canonical,
            normalized,
        });
    }

    for i in 0..infos.len() {
        for j in (i + 1)..infos.len() {
            if infos[i].normalized == infos[j].normalized {
                return Err(syn::Error::new_spanned(
                    &infos[j].ident,
                    format!(
                        "Varianten `{}` und `{}` erzeugen denselben kebab-Namen `{}`; \
                         verwende #[kebab_enum(rename = \"...\")] zur Unterscheidung",
                        infos[i].ident, infos[j].ident, infos[i].normalized,
                    ),
                ));
            }
        }
    }

    let error_path = &args.error_path;
    let ctor_ident = &args.ctor_ident;
    let enum_name_str = enum_name.to_string();

    let all_variants: Vec<TokenStream> = infos
        .iter()
        .map(|v| {
            let ident = &v.ident;
            quote! { #enum_name::#ident }
        })
        .collect();

    let as_str_arms: Vec<TokenStream> = infos
        .iter()
        .map(|v| {
            let ident = &v.ident;
            let canonical = &v.canonical;
            quote! { #enum_name::#ident => #canonical, }
        })
        .collect();

    let from_str_arms: Vec<TokenStream> = infos
        .iter()
        .map(|v| {
            let ident = &v.ident;
            let normalized = &v.normalized;
            quote! { #normalized => ::core::result::Result::Ok(#enum_name::#ident), }
        })
        .collect();

    let parse_arms: Vec<TokenStream> = infos
        .iter()
        .map(|v| {
            let ident = &v.ident;
            let normalized = &v.normalized;
            quote! { #normalized => ::core::option::Option::Some(#enum_name::#ident), }
        })
        .collect();

    let parse_fn = args.parse_option.then(|| {
        quote! {
            /// Liest einen Namen (kebab-/snake-case, beliebige Groß-/Kleinschreibung,
            /// umgebender Leerraum wird ignoriert); unbekannt ergibt `None`.
            #[must_use]
            pub fn parse(value: &str) -> ::core::option::Option<Self> {
                let normalized = value.trim().replace('_', "-").to_ascii_lowercase();
                match normalized.as_str() {
                    #(#parse_arms)*
                    _ => ::core::option::Option::None,
                }
            }
        }
    });

    let from_str_impl = (!args.no_from_str).then(|| {
        quote! {
            impl ::std::str::FromStr for #enum_name {
                type Err = #error_path;

                fn from_str(s: &str) -> ::core::result::Result<Self, Self::Err> {
                    let normalized = s.replace('_', "-").to_ascii_lowercase();
                    match normalized.as_str() {
                        #(#from_str_arms)*
                        _ => ::core::result::Result::Err(#error_path::#ctor_ident(#enum_name_str, s)),
                    }
                }
            }
        }
    });

    Ok(quote! {
        impl #enum_name {
            /// Alle Varianten in Deklarationsreihenfolge.
            pub const ALL: &'static [#enum_name] = &[#(#all_variants),*];

            /// Kanonischer Name dieser Variante (kebab- oder snake-case).
            #[must_use]
            pub const fn as_str(&self) -> &'static str {
                match self {
                    #(#as_str_arms)*
                }
            }

            #parse_fn
        }

        impl ::std::fmt::Display for #enum_name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        #from_str_impl
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn to_case_kebab(ident: &str) -> String {
        to_case(ident, Case::Kebab)
    }

    #[test]
    fn snake_case_joins_words_with_underscore() {
        assert_eq!(to_case("CompleteDrain", Case::Snake), "complete_drain");
        assert_eq!(to_case("HTTPServer", Case::Snake), "http_server");
    }

    #[test]
    fn kebab_case_converts_simple_pascal_case() {
        assert_eq!(to_case_kebab("AddCriterion"), "add-criterion");
    }

    #[test]
    fn kebab_case_groups_leading_acronym_before_a_word() {
        assert_eq!(to_case_kebab("HTTPServer"), "http-server");
    }

    #[test]
    fn kebab_case_treats_all_caps_identifier_as_one_word() {
        assert_eq!(to_case_kebab("ID"), "id");
    }

    #[test]
    fn kebab_case_handles_existing_separators() {
        assert_eq!(to_case_kebab("Already_Snake"), "already-snake");
    }

    #[test]
    fn kebab_case_handles_single_letter() {
        assert_eq!(to_case_kebab("A"), "a");
    }

    #[test]
    fn expand_rejects_non_enum() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            pub struct Foo(String);
        };
        let Err(err) = expand_kebab_enum(&input) else {
            return Err(TestError::Unexpected(
                "non-enum input must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("enums"));
        Ok(())
    }

    #[test]
    fn expand_rejects_enum_without_variants() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            pub enum Foo {}
        };
        let Err(err) = expand_kebab_enum(&input) else {
            return Err(TestError::Unexpected(
                "empty enums must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("mindestens eine Variante"));
        Ok(())
    }

    #[test]
    fn expand_rejects_non_unit_variant() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            pub enum Foo {
                A,
                B(String),
            }
        };
        let Err(err) = expand_kebab_enum(&input) else {
            return Err(TestError::Unexpected(
                "tuple variants must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("ausschließlich Unit-Varianten"));
        Ok(())
    }

    #[test]
    fn expand_rejects_duplicate_normalized_names() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            pub enum Foo {
                AddCriterion,
                #[kebab_enum(rename = "add_criterion")]
                Other,
            }
        };
        let Err(err) = expand_kebab_enum(&input) else {
            return Err(TestError::Unexpected(
                "colliding kebab names must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("denselben kebab-Namen"));
        Ok(())
    }

    #[test]
    fn expand_rejects_unknown_enum_level_attribute() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[kebab_enum(bogus)]
            pub enum Foo { A }
        };
        let Err(err) = expand_kebab_enum(&input) else {
            return Err(TestError::Unexpected(
                "unknown enum-level keys must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("unbekanntes kebab_enum-Attribut"));
        Ok(())
    }

    #[test]
    fn expand_rejects_unknown_variant_level_attribute() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            pub enum Foo {
                #[kebab_enum(bogus)]
                A,
            }
        };
        let Err(err) = expand_kebab_enum(&input) else {
            return Err(TestError::Unexpected(
                "unknown variant-level keys must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("unbekanntes kebab_enum-Attribut"));
        Ok(())
    }

    #[test]
    fn expand_default_expansion_contains_expected_items() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            pub enum AddCriterion {
                AddCriterion,
                HTTPServer,
            }
        };
        let tokens = expand_kebab_enum(&input)
            .map_err(ctx("valid enum must expand"))?
            .to_string();

        assert!(tokens.contains("ALL"));
        assert!(tokens.contains("fn as_str"));
        assert!(tokens.contains("\"add-criterion\""));
        assert!(tokens.contains("\"http-server\""));
        assert!(tokens.contains("impl :: std :: fmt :: Display for AddCriterion"));
        assert!(tokens.contains("impl :: std :: str :: FromStr for AddCriterion"));
        // Default error path and ctor.
        assert!(tokens.contains("crate :: error :: InvalidId"));
        assert!(tokens.contains(":: unknown_variant"));
        Ok(())
    }

    #[test]
    fn expand_rename_overrides_canonical_name() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            pub enum Foo {
                #[kebab_enum(rename = "super-admin")]
                Maintainer,
            }
        };
        let tokens = expand_kebab_enum(&input)
            .map_err(ctx("renamed variant must expand"))?
            .to_string();
        assert!(tokens.contains("\"super-admin\""));
        assert!(!tokens.contains("\"maintainer\""));
        Ok(())
    }

    #[test]
    fn expand_custom_error_and_ctor_attributes_are_used() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[kebab_enum(error = "my_crate::error::MyError", ctor = "custom_unknown")]
            pub enum Foo { A }
        };
        let tokens = expand_kebab_enum(&input)
            .map_err(ctx("custom error/ctor must expand"))?
            .to_string();
        assert!(tokens.contains("my_crate :: error :: MyError"));
        assert!(tokens.contains(":: custom_unknown"));
        assert!(!tokens.contains("crate :: error :: InvalidId"));
        Ok(())
    }

    #[test]
    fn expand_rejects_invalid_error_path() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[kebab_enum(error = "not a path!!")]
            pub enum Foo { A }
        };
        let Err(err) = expand_kebab_enum(&input) else {
            return Err(TestError::Unexpected(
                "invalid error path must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("gültiger Pfad"));
        Ok(())
    }

    #[test]
    fn expand_snake_case_changes_canonical_names() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[kebab_enum(case = "snake")]
            pub enum Foo { CompleteDrain }
        };
        let tokens = expand_kebab_enum(&input)
            .map_err(ctx("snake case must expand"))?
            .to_string();
        assert!(tokens.contains("=> \"complete_drain\""));
        // FromStr normalises to kebab, so lookup stays tolerant.
        assert!(tokens.contains("\"complete-drain\" =>"));
        Ok(())
    }

    #[test]
    fn expand_rejects_unknown_case() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[kebab_enum(case = "camel")]
            pub enum Foo { A }
        };
        let Err(err) = expand_kebab_enum(&input) else {
            return Err(TestError::Unexpected(
                "unknown case must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("`case` muss"));
        Ok(())
    }

    #[test]
    fn expand_parse_option_and_no_from_str() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[kebab_enum(parse_option, no_from_str)]
            pub enum Foo { A }
        };
        let tokens = expand_kebab_enum(&input)
            .map_err(ctx("flags must expand"))?
            .to_string();
        assert!(tokens.contains("fn parse"));
        assert!(!tokens.contains("FromStr"));
        Ok(())
    }
}
