//! `#[derive(HarwId)]`-Expansion.
//!
//! Spec: AP W1-26e (HarwId-Derive-Makro).
//!
//! # Zweck
//! Ersetzt die pro Crate wiederholte Handschrift-Boilerplate für String-Newtype-IDs
//! (vgl. `harw-types/src/ids.rs::newtype_id!` und die ~40-Zeilen-Blöcke in
//! `harw-plan/src/ids.rs`) durch ein Derive-Makro, das dieselben Konstruktoren,
//! Trait-Impls und Validierungsregeln erzeugt — inklusive der in `harw-plan`
//! bislang fehlenden Leer-Validierung.
//!
//! # Anwendbarkeit
//! `#[derive(HarwId)]` ist ausschließlich auf Tuple-Structs mit **genau einem**
//! Feld vom Typ `String` anwendbar, z. B.:
//!
//! ```ignore
//! #[derive(HarwId)]
//! pub struct PlanId(String);
//! ```
//!
//! Jede Abweichung (Enum, Named-Struct, Unit-Struct, mehr als ein Feld, Feldtyp
//! ungleich `String`) erzeugt einen Compile-Fehler mit einer erklärenden
//! Meldung (siehe [`validate_single_string_tuple_field`]).
//!
//! # Generierte API
//! Für `struct Foo(String);` erzeugt das Makro:
//!
//! ```ignore
//! impl Foo {
//!     pub fn try_new(value: impl Into<String>) -> Result<Self, <error>>;
//!     pub fn as_str(&self) -> &str;
//!     pub fn into_inner(self) -> String;
//!     // nur mit #[harw_id(infallible)]:
//!     pub fn new(value: impl Into<String>) -> Self;
//! }
//! impl std::fmt::Display for Foo { .. }
//! impl std::str::FromStr for Foo { type Err = <error>; .. }
//! impl AsRef<str> for Foo { .. }
//! impl std::borrow::Borrow<str> for Foo { .. }
//! impl PartialEq<str> for Foo { .. }
//! impl PartialEq<&str> for Foo { .. }
//! impl PartialEq<String> for Foo { .. }
//! ```
//!
//! `try_new` und `FromStr::from_str` lehnen leere und reine
//! Whitespace-Werte ab (`value.trim().is_empty()`), analog zu
//! `harw-types::ids::validate_id`.
//!
//! # Attribute
//!
//! - `#[harw_id(infallible)]` — erzeugt zusätzlich einen unvalidierten
//!   Kompatibilitätskonstruktor `pub fn new(value: impl Into<String>) -> Self`
//!   für Bestandscode, der sich nicht sofort auf `try_new` migrieren lässt
//!   (z. B. `harw-plan::ids::PlanId::new`, das aktuell leere Strings
//!   akzeptiert). Es wird **kein** `#[deprecated]` erzeugt, da das in
//!   Crates mit `-D warnings` den Build bricht; stattdessen trägt der
//!   generierte Konstruktor einen Doc-Hinweis, neue Aufrufer sollten
//!   `try_new` verwenden.
//! - `#[harw_id(no_serde)]` — hat **keinen** Codegen-Effekt. Das Makro
//!   erzeugt grundsätzlich keine `Serialize`/`Deserialize`-Impls; der
//!   Konsument leitet serde weiterhin selbst am Struct ab
//!   (`#[derive(Serialize, Deserialize)]`, ggf. mit `#[serde(transparent)]`
//!   für eine reine String-Repräsentation auf der Wire-Ebene, vgl.
//!   `harw-types::ids`). Das Attribut existiert nur, damit Aufrufer die
//!   Abwesenheit von serde-Codegen explizit dokumentieren können, ohne dass
//!   das Makro eine unbekannte Attribut-Meldung wirft.
//! - `#[harw_id(error = "pfad::zu::Error")]` — Pfad des Fehlertyps für
//!   `try_new`/`FromStr::Err`. Default: `crate::error::InvalidId`.
//! - `#[harw_id(ctor = "methodenname")]` — Name der assoziierten Funktion auf
//!   dem Fehlertyp, die die Leer-Validierung meldet. Default: `empty`.
//! - `#[harw_id(validate = "pfad::zur::fn")]` — ersetzt die Standard-Leerprüfung
//!   in `try_new` durch eine eigene Regel. Signatur:
//!   `fn(&str) -> Result<(), <error>>`, wobei `<error>` der unter `error`
//!   konfigurierte Typ ist; `ctor` wird dann nicht verwendet. Die Funktion muss
//!   Leerwerte selbst ablehnen, falls gewünscht.
//!
//! Mehrere `#[harw_id(...)]`-Attribute auf demselben Item werden zusammengeführt;
//! bei doppelten Schlüsseln gewinnt das zuletzt gesehene Attribut.
//!
//! # Fehlertyp-Vertrag (wichtig für Konsumenten)
//!
//! Proc-Macro-Crates dürfen ausschließlich Makros exportieren — der
//! Fehlertyp kann also nicht in `harw-macros` selbst liegen. Das Makro
//! generiert deshalb **keinen** Fehlertyp, sondern referenziert ihn über
//! einen konfigurierbaren Pfad (`error`) und ruft darauf eine konfigurierbare
//! assoziierte Funktion (`ctor`) mit dem Feldnamen als `&'static str` auf:
//!
//! ```ignore
//! // vom Konsumenten bereitzustellen (Default-Namen):
//! impl crate::error::InvalidId {
//!     pub fn empty(field: &'static str) -> Self { /* ... */ }
//! }
//! ```
//!
//! Diese Konvention (Pfad **und** Methodenname konfigurierbar statt eines
//! fest verdrahteten `InvalidId::empty`) wurde bewusst gewählt: bestehender
//! Code hat bereits einen anderen Konstruktornamen etabliert
//! (`harw-types::error::InvalidId::new(id_type)`, siehe
//! `harw-types/src/ids.rs`), und künftige Crates (`harw-research`,
//! `harw-code-graph`, …) sollen ihren Fehlertyp nicht umbenennen müssen, nur
//! um dieses Makro nutzen zu können. Ein starr fest verdrahteter Name hätte
//! genau das erzwungen. Der Default (`crate::error::InvalidId::empty`) gilt
//! für neue Crates, die keine abweichende Konvention mitbringen.
//!
//! Die generierten Signaturen im API-Abschnitt oben nennen den Fehlertyp aus
//! Lesbarkeitsgründen `<error>`; er ist **niemals** ein von diesem Makro
//! definierter Typ namens `InvalidHarwId`, sondern immer der unter `error`
//! konfigurierte (oder der Default-)Pfad.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Data, DeriveInput, Fields, Ident, LitStr, Path, Type};

/// Geparste `#[harw_id(...)]`-Konfiguration für ein Item.
///
/// # Design-doc reference
/// Spec section "HarwId — Attribut-Grammatik" (Modul-Doc oben).
struct HarwIdArgs {
    /// Ob zusätzlich ein unvalidierter `new`-Konstruktor erzeugt wird.
    infallible: bool,
    /// Pfad des Fehlertyps für `try_new`/`FromStr::Err`.
    error_path: Path,
    /// Name der assoziierten Fehler-Konstruktorfunktion (Feldname → Fehler).
    ctor_ident: Ident,
    /// Optionale eigene Validierungsfunktion (ersetzt die Leerprüfung).
    validate: Option<Path>,
}

impl Default for HarwIdArgs {
    fn default() -> Self {
        Self {
            infallible: false,
            error_path: syn::parse_quote!(crate::error::InvalidId),
            ctor_ident: Ident::new("empty", proc_macro2::Span::call_site()),
            validate: None,
        }
    }
}

/// Liest alle `#[harw_id(...)]`-Attribute eines Items ein.
///
/// # Errors
/// Liefert `syn::Error`, wenn ein unbekannter Schlüssel auftritt oder `error`
/// / `ctor` keinen gültigen Pfad bzw. Bezeichner enthalten.
///
/// # Design-doc reference
/// Spec section "HarwId — Attribut-Grammatik" (Modul-Doc oben).
fn parse_harw_id_args(attrs: &[Attribute]) -> syn::Result<HarwIdArgs> {
    let mut args = HarwIdArgs::default();

    for attr in attrs {
        if !attr.path().is_ident("harw_id") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("infallible") {
                args.infallible = true;
                Ok(())
            } else if meta.path.is_ident("no_serde") {
                // Kein Codegen-Effekt: das Makro erzeugt ohnehin keine
                // serde-Impls. Wird nur akzeptiert, damit Konsumenten die
                // Abwesenheit von serde-Codegen explizit dokumentieren
                // können, ohne einen "unbekanntes Attribut"-Fehler zu erhalten.
                Ok(())
            } else if meta.path.is_ident("error") {
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
            } else if meta.path.is_ident("validate") {
                let lit: LitStr = meta.value()?.parse()?;
                args.validate = Some(syn::parse_str(&lit.value()).map_err(|e| {
                    syn::Error::new_spanned(
                        &lit,
                        format!("`validate` muss ein gültiger Pfad sein: {e}"),
                    )
                })?);
                Ok(())
            } else {
                Err(meta.error(
                    "unbekanntes harw_id-Attribut; erwartet: infallible, no_serde, \
                     error = \"...\", ctor = \"...\", validate = \"...\"",
                ))
            }
        })?;
    }

    Ok(args)
}

/// Prüft, ob `ty` (nach dem letzten Pfadsegment) exakt `String` ist.
///
/// Akzeptiert sowohl `String` als auch qualifizierte Formen wie
/// `std::string::String`, da nur das letzte Pfadsegment betrachtet wird.
fn is_string_type(ty: &Type) -> bool {
    match ty {
        Type::Path(tp) => tp
            .path
            .segments
            .last()
            .map(|s| s.ident == "String")
            .unwrap_or(false),
        _ => false,
    }
}

/// Validiert, dass `input` eine Tuple-Struct mit genau einem `String`-Feld ist.
///
/// # Errors
/// Liefert `syn::Error` für Enums, Named-/Unit-Structs, Tuple-Structs mit
/// != 1 Feld sowie ein einzelnes Feld mit einem anderen Typ als `String`.
///
/// # Design-doc reference
/// Spec section "HarwId — Anwendbarkeit" (Modul-Doc oben).
fn validate_single_string_tuple_field(input: &DeriveInput) -> syn::Result<()> {
    let data = match &input.data {
        Data::Struct(data) => data,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "HarwId kann nur auf structs angewendet werden, nicht auf enums oder unions",
            ));
        }
    };

    let fields = match &data.fields {
        Fields::Unnamed(f) => f,
        _ => {
            return Err(syn::Error::new_spanned(
                &data.fields,
                "HarwId erfordert eine Tuple-Struct mit genau einem `String`-Feld, \
                 z. B. `struct Foo(String);`",
            ));
        }
    };

    if fields.unnamed.len() != 1 {
        return Err(syn::Error::new_spanned(
            fields,
            format!(
                "HarwId erfordert eine Tuple-Struct mit genau einem Feld, gefunden: {}",
                fields.unnamed.len()
            ),
        ));
    }

    let field = &fields.unnamed[0];
    if !is_string_type(&field.ty) {
        return Err(syn::Error::new_spanned(
            &field.ty,
            "HarwId erfordert, dass das einzige Feld vom Typ `String` ist",
        ));
    }

    Ok(())
}

/// Expander für das `HarwId`-Derive-Makro.
///
/// Validiert die Struct-Form (siehe [`validate_single_string_tuple_field`]),
/// liest die `#[harw_id(...)]`-Konfiguration (siehe [`parse_harw_id_args`])
/// und erzeugt Konstruktoren, `Display`, `FromStr` sowie String-Vergleichs-
/// und Konvertierungs-Impls.
///
/// # Errors
/// Siehe [`validate_single_string_tuple_field`] und [`parse_harw_id_args`].
///
/// # Design-doc reference
/// Spec section "HarwId — generierte API" (Modul-Doc oben).
pub(crate) fn expand_harw_id(input: &DeriveInput) -> syn::Result<TokenStream> {
    let struct_name = &input.ident;
    let args = parse_harw_id_args(&input.attrs)?;
    validate_single_string_tuple_field(input)?;

    let error_path = &args.error_path;
    let ctor_ident = &args.ctor_ident;
    let struct_name_str = struct_name.to_string();

    let infallible_ctor = if args.infallible {
        quote! {
            /// Kompatibilitätskonstruktor für Bestandscode ohne Validierung.
            ///
            /// # Hinweis
            /// Neue Aufrufer sollten [`Self::try_new`] verwenden: dieser
            /// Konstruktor lehnt leere oder reine Whitespace-Werte **nicht** ab.
            #[must_use]
            pub fn new(value: impl ::core::convert::Into<::std::string::String>) -> Self {
                Self(value.into())
            }
        }
    } else {
        TokenStream::new()
    };

    let check = if let Some(validate) = &args.validate {
        quote! {
            match #validate(&value) {
                ::core::result::Result::Ok(()) => ::core::result::Result::Ok(Self(value)),
                ::core::result::Result::Err(err) => ::core::result::Result::Err(err),
            }
        }
    } else {
        quote! {
            if value.trim().is_empty() {
                ::core::result::Result::Err(#error_path::#ctor_ident(#struct_name_str))
            } else {
                ::core::result::Result::Ok(Self(value))
            }
        }
    };

    Ok(quote! {
        impl #struct_name {
            /// Fallibler Konstruktor: validiert `value` (Standard: lehnt leere und
            /// reine Whitespace-Werte ab; mit `validate = ...` die eigene Regel).
            ///
            /// # Errors
            /// Liefert einen Fehler, wenn die Validierung `value` ablehnt.
            pub fn try_new(
                value: impl ::core::convert::Into<::std::string::String>,
            ) -> ::core::result::Result<Self, #error_path> {
                let value = value.into();
                #check
            }

            #infallible_ctor

            /// Borrowt den inneren String-Slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Gibt den inneren `String` konsumierend zurück.
            #[must_use]
            pub fn into_inner(self) -> ::std::string::String {
                self.0
            }
        }

        impl ::std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl ::std::str::FromStr for #struct_name {
            type Err = #error_path;

            fn from_str(s: &str) -> ::core::result::Result<Self, Self::Err> {
                Self::try_new(s)
            }
        }

        impl ::std::convert::AsRef<str> for #struct_name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl ::std::borrow::Borrow<str> for #struct_name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl ::core::cmp::PartialEq<str> for #struct_name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl ::core::cmp::PartialEq<&str> for #struct_name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl ::core::cmp::PartialEq<::std::string::String> for #struct_name {
            fn eq(&self, other: &::std::string::String) -> bool {
                &self.0 == other
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn expand_rejects_enum() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            pub enum Foo { A, B }
        };
        let Err(err) = expand_harw_id(&input) else {
            return Err(TestError::Unexpected("enums must be rejected".to_owned()));
        };
        assert!(err.to_string().contains("structs"));
        Ok(())
    }

    #[test]
    fn expand_rejects_named_struct() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            pub struct Foo { value: String }
        };
        let Err(err) = expand_harw_id(&input) else {
            return Err(TestError::Unexpected(
                "named structs must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("Tuple-Struct"));
        Ok(())
    }

    #[test]
    fn expand_rejects_multiple_fields() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            pub struct Foo(String, String);
        };
        let Err(err) = expand_harw_id(&input) else {
            return Err(TestError::Unexpected(
                "multi-field tuple structs must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("genau einem Feld"));
        Ok(())
    }

    #[test]
    fn expand_rejects_non_string_field() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            pub struct Foo(u64);
        };
        let Err(err) = expand_harw_id(&input) else {
            return Err(TestError::Unexpected(
                "non-String fields must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("String"));
        Ok(())
    }

    #[test]
    fn expand_accepts_basic_tuple_struct_with_defaults() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            pub struct Foo(String);
        };
        let tokens = expand_harw_id(&input)
            .map_err(ctx("valid tuple struct must expand"))?
            .to_string();

        assert!(tokens.contains("fn try_new"));
        assert!(tokens.contains("fn as_str"));
        assert!(tokens.contains("fn into_inner"));
        assert!(tokens.contains("impl :: std :: fmt :: Display for Foo"));
        assert!(tokens.contains("impl :: std :: str :: FromStr for Foo"));
        assert!(tokens.contains("impl :: std :: convert :: AsRef < str > for Foo"));
        assert!(tokens.contains("impl :: std :: borrow :: Borrow < str > for Foo"));
        assert!(tokens.contains("impl :: core :: cmp :: PartialEq < str > for Foo"));
        assert!(tokens.contains("impl :: core :: cmp :: PartialEq < & str > for Foo"));
        assert!(
            tokens.contains(
                "impl :: core :: cmp :: PartialEq < :: std :: string :: String > for Foo"
            )
        );
        // Default error path and ctor.
        assert!(tokens.contains("crate :: error :: InvalidId"));
        assert!(tokens.contains(":: empty"));
        // Without #[harw_id(infallible)] no compatibility constructor is emitted.
        assert!(!tokens.contains("fn new ("));
        Ok(())
    }

    #[test]
    fn expand_infallible_attribute_adds_compat_constructor() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[harw_id(infallible)]
            pub struct Foo(String);
        };
        let tokens = expand_harw_id(&input)
            .map_err(ctx("infallible attribute must expand"))?
            .to_string();
        assert!(tokens.contains("fn new ("));
        assert!(tokens.contains("Kompatibilitätskonstruktor"));
        Ok(())
    }

    #[test]
    fn expand_no_serde_attribute_is_accepted_without_codegen_effect() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[harw_id(no_serde)]
            pub struct Foo(String);
        };
        let with_no_serde = expand_harw_id(&input)
            .map_err(ctx("no_serde attribute must be accepted"))?
            .to_string();

        let plain_input: DeriveInput = syn::parse_quote! {
            pub struct Foo(String);
        };
        let plain = expand_harw_id(&plain_input)
            .map_err(ctx("plain struct must expand"))?
            .to_string();

        assert_eq!(with_no_serde, plain);
        Ok(())
    }

    #[test]
    fn expand_custom_error_and_ctor_attributes_are_used() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[harw_id(error = "my_crate::error::MyError", ctor = "custom_empty")]
            pub struct Foo(String);
        };
        let tokens = expand_harw_id(&input)
            .map_err(ctx("custom error/ctor must expand"))?
            .to_string();

        assert!(tokens.contains("my_crate :: error :: MyError"));
        assert!(tokens.contains(":: custom_empty"));
        assert!(!tokens.contains("crate :: error :: InvalidId"));
        Ok(())
    }

    #[test]
    fn expand_validate_attribute_replaces_blank_check() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[harw_id(error = "my_crate::MyError", validate = "my_crate::check_foo")]
            pub struct Foo(String);
        };
        let tokens = expand_harw_id(&input)
            .map_err(ctx("validate attribute must expand"))?
            .to_string();
        assert!(tokens.contains("my_crate :: check_foo (& value)"));
        assert!(!tokens.contains("is_empty"));
        Ok(())
    }

    #[test]
    fn expand_rejects_invalid_validate_path() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[harw_id(validate = "not a path!!")]
            pub struct Foo(String);
        };
        let Err(err) = expand_harw_id(&input) else {
            return Err(TestError::Unexpected(
                "invalid validate path must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("gültiger Pfad"));
        Ok(())
    }

    #[test]
    fn expand_rejects_unknown_attribute_key() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[harw_id(bogus)]
            pub struct Foo(String);
        };
        let Err(err) = expand_harw_id(&input) else {
            return Err(TestError::Unexpected(
                "unknown attribute keys must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("unbekanntes harw_id-Attribut"));
        Ok(())
    }

    #[test]
    fn expand_rejects_invalid_error_path() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[harw_id(error = "not a path!!")]
            pub struct Foo(String);
        };
        let Err(err) = expand_harw_id(&input) else {
            return Err(TestError::Unexpected(
                "invalid error path must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("gültiger Pfad"));
        Ok(())
    }
}
