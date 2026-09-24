//! `#[derive(Redact)]`-Expansion.
//!
//! Spec: AW0-02-Brief, Abschnitt 3 (`#[derive(Redact)]`); Vertragsabschnitt
//! A.5 (`harw-observe`, `Redact`/`Redacted`) in `docs/design/build-history.md`.
//!
//! # Zweck
//! `harw_observe::Redact` ist mit Absicht schmal geschnitten:
//! `fn redact(&self) -> Redacted` liefert genau **einen** Wert für den ganzen
//! Empfänger. Für eine Struct mit mehreren Feldern erzeugt dieses Makro daher
//! keinen Wert pro Feld, sondern baut aus den Feldern, die laut ihrer Policy
//! erscheinen dürfen, eine einzige strukturierte Zeichenkette
//! (`"StructName { feld: wert, ... }"`) und liefert sie als ein einzelnes
//! `Redacted::Shown(..)` zurück. Das ist genau die Form, die ein `#[traced]`-
//! Makro (AW1-02) später als ein einzelnes Tracing-Feld je Argument
//! verwenden kann.
//!
//! # Anwendbarkeit
//! `#[derive(Redact)]` ist auf Structs mit ausschließlich benannten Feldern
//! oder auf Unit-Structs anwendbar. Tuple-Structs und Enums werden mit einem
//! Compile-Fehler abgelehnt.
//!
//! # Feldattribute
//! Pro Feld gilt genau eine der drei Policies:
//! - **ohne Attribut** (Voreinstellung): das Feld erscheint nicht in der
//!   Ausgabe (`Redacted::Omitted`-Semantik für dieses Feld). Das ist der
//!   Zweck der Voreinstellung: ein Feld, über das niemand nachgedacht hat,
//!   landet nicht im Log.
//! - `#[redact(show)]`: das Feld erscheint über `Display`.
//! - `#[redact(hash)]`: das Feld erscheint als blake3-Hex-Hash seiner
//!   `Display`-Darstellung, gekürzt auf 16 Zeichen.
//!
//! Widersprüchliche Attribute auf demselben Feld (z. B. sowohl `show` als
//! auch `hash`) und unbekannte Schlüssel sind Compile-Fehler.
//!
//! # Abhängigkeit auf `blake3`
//! Der für `#[redact(hash)]`-Felder erzeugte Code referenziert
//! `::blake3::hash`. Ein Crate, das mindestens ein `#[redact(hash)]`-Feld
//! nutzt, muss deshalb selbst von `blake3` abhängen (Workspace-Version, siehe
//! `[workspace.dependencies]` im Root-`Cargo.toml`).

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Data, DeriveInput, Fields, Ident};

/// Wie ein einzelnes Feld in der `redact()`-Ausgabe erscheint.
///
/// Siehe Modul-Doc, Abschnitt "Feldattribute".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldPolicy {
    /// Kein Attribut: das Feld erscheint nicht in der Ausgabe (Voreinstellung).
    Omitted,
    /// `#[redact(show)]`: das Feld erscheint über `Display`.
    Show,
    /// `#[redact(hash)]`: das Feld erscheint als gekürzter blake3-Hex-Hash.
    Hash,
}

/// Liest das `#[redact(...)]`-Attribut eines Feldes.
///
/// # Errors
/// - unbekannter Schlüssel (nicht `show`/`hash`) → `syn::Error`.
/// - widersprüchliche Schlüssel auf demselben Feld (z. B. `show` und `hash`)
///   → `syn::Error`.
///
/// # Design-doc reference
/// AW0-02-Brief, Abschnitt 3 (`#[derive(Redact)]` — Feldattribute).
fn parse_field_policy(attrs: &[Attribute]) -> syn::Result<FieldPolicy> {
    let mut policy: Option<FieldPolicy> = None;

    for attr in attrs {
        if !attr.path().is_ident("redact") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            let found = if meta.path.is_ident("show") {
                FieldPolicy::Show
            } else if meta.path.is_ident("hash") {
                FieldPolicy::Hash
            } else {
                return Err(meta.error("unbekanntes redact-Attribut; erwartet: show, hash"));
            };

            if let Some(existing) = policy {
                if existing != found {
                    return Err(
                        meta.error("widersprüchliche #[redact(...)]-Attribute auf einem Feld")
                    );
                }
            }
            policy = Some(found);
            Ok(())
        })?;
    }

    Ok(policy.unwrap_or(FieldPolicy::Omitted))
}

/// Expander für das `Redact`-Derive-Makro.
///
/// Validiert, dass `input` eine Struct mit benannten Feldern (oder eine
/// Unit-Struct) ist, liest pro Feld die `#[redact(...)]`-Policy (siehe
/// [`parse_field_policy`]) und erzeugt `impl ::harw_observe::Redact`. Die
/// erzeugte `redact()`-Methode baut aus den nicht ausgelassenen Feldern eine
/// einzige, strukturierte Zeichenkette (`"StructName { feld: wert, ... }"`)
/// und liefert sie als `Redacted::Shown(..)` zurück — passend zur
/// Vertragsform `fn redact(&self) -> Redacted` (genau ein Ergebnis je Wert),
/// siehe Vertragsabschnitt A.5.
///
/// # Errors
/// - kein Struct (Enum, Union) → `syn::Error`.
/// - Tuple-Struct → `syn::Error`.
/// - ungültige `#[redact(...)]`-Konfiguration → siehe [`parse_field_policy`].
///
/// # Design-doc reference
/// AW0-02-Brief, Abschnitt 3 (`#[derive(Redact)]`); Vertragsabschnitt A.5.
pub(crate) fn expand_redact(input: &DeriveInput) -> syn::Result<TokenStream> {
    let struct_name = &input.ident;
    let struct_name_str = struct_name.to_string();

    let data = match &input.data {
        Data::Struct(data) => data,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "Redact kann nur auf structs angewendet werden, nicht auf enums oder unions",
            ));
        }
    };

    let named_fields = match &data.fields {
        Fields::Named(named) => Some(&named.named),
        Fields::Unit => None,
        Fields::Unnamed(_) => {
            return Err(syn::Error::new_spanned(
                &data.fields,
                "Redact erfordert benannte Felder oder eine Unit-Struct, keine Tuple-Struct",
            ));
        }
    };

    let mut field_tokens: Vec<TokenStream> = Vec::new();
    if let Some(named_fields) = named_fields {
        for field in named_fields {
            let policy = parse_field_policy(&field.attrs)?;
            let ident: &Ident = field.ident.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(field, "Redact erfordert benannte Felder")
            })?;
            let field_name_str = ident.to_string();

            match policy {
                FieldPolicy::Omitted => {}
                FieldPolicy::Show => {
                    field_tokens.push(quote! {
                        __harw_redact_parts.push(::std::format!(
                            "{}: {}",
                            #field_name_str,
                            &self.#ident,
                        ));
                    });
                }
                FieldPolicy::Hash => {
                    field_tokens.push(quote! {
                        {
                            let __harw_redact_rendered = ::std::format!("{}", &self.#ident);
                            let __harw_redact_hash =
                                ::blake3::hash(__harw_redact_rendered.as_bytes()).to_hex();
                            let __harw_redact_short: ::std::string::String =
                                __harw_redact_hash.as_str().chars().take(16).collect();
                            __harw_redact_parts.push(::std::format!(
                                "{}: {}",
                                #field_name_str,
                                __harw_redact_short,
                            ));
                        }
                    });
                }
            }
        }
    }

    Ok(quote! {
        impl ::harw_observe::Redact for #struct_name {
            fn redact(&self) -> ::harw_observe::Redacted {
                let mut __harw_redact_parts: ::std::vec::Vec<::std::string::String> =
                    ::std::vec::Vec::new();
                #(#field_tokens)*
                ::harw_observe::Redacted::Shown(if __harw_redact_parts.is_empty() {
                    ::std::format!("{} {{}}", #struct_name_str)
                } else {
                    ::std::format!(
                        "{} {{ {} }}",
                        #struct_name_str,
                        __harw_redact_parts.join(", "),
                    )
                })
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
            enum Foo { A, B }
        };
        let Err(err) = expand_redact(&input) else {
            return Err(TestError::Unexpected("enums must be rejected".to_owned()));
        };
        assert!(err.to_string().contains("structs"));
        Ok(())
    }

    #[test]
    fn expand_rejects_tuple_struct() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct Foo(String);
        };
        let Err(err) = expand_redact(&input) else {
            return Err(TestError::Unexpected(
                "tuple structs must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("Tuple-Struct"));
        Ok(())
    }

    #[test]
    fn expand_accepts_unit_struct() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct Foo;
        };
        let tokens = expand_redact(&input)
            .map_err(ctx("unit structs must expand"))?
            .to_string();
        assert!(tokens.contains("impl :: harw_observe :: Redact for Foo"));
        // Die Ausgabe `Foo {}` entsteht erst zur **Laufzeit** aus
        // `format!("{} {{}}", "Foo")` -- sie steht nicht als Zeichenkette in
        // den erzeugten Token. Geprüft wird deshalb, was das Makro
        // tatsächlich emittiert: die Formatvorlage und den Namensliteral.
        assert!(tokens.contains("\"{} {{}}\""));
        assert!(tokens.contains("\"Foo\""));
        Ok(())
    }

    #[test]
    fn expand_rejects_unknown_field_attribute() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct Foo {
                #[redact(bogus)]
                a: String,
            }
        };
        let Err(err) = expand_redact(&input) else {
            return Err(TestError::Unexpected(
                "unknown redact keys must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("unbekanntes redact-Attribut"));
        Ok(())
    }

    #[test]
    fn expand_rejects_conflicting_field_attributes() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct Foo {
                #[redact(show, hash)]
                a: String,
            }
        };
        let Err(err) = expand_redact(&input) else {
            return Err(TestError::Unexpected(
                "conflicting redact keys must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("widersprüchliche"));
        Ok(())
    }

    #[test]
    fn expand_omits_fields_without_attribute() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct Foo {
                secret: String,
            }
        };
        let tokens = expand_redact(&input)
            .map_err(ctx("struct without attributes must expand"))?
            .to_string();
        assert!(!tokens.contains("secret"));
        Ok(())
    }

    #[test]
    fn expand_show_uses_display_and_field_name() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct Foo {
                #[redact(show)]
                clan: String,
            }
        };
        let tokens = expand_redact(&input)
            .map_err(ctx("show field must expand"))?
            .to_string();
        // Der Feldname wird als eigenes String-Literal-Argument übergeben,
        // nicht in das Format-Literal selbst eingebacken.
        assert!(tokens.contains("\"clan\""));
        assert!(tokens.contains("self . clan"));
        Ok(())
    }

    #[test]
    fn expand_hash_uses_blake3_and_truncates() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct Foo {
                #[redact(hash)]
                token: String,
            }
        };
        let tokens = expand_redact(&input)
            .map_err(ctx("hash field must expand"))?
            .to_string();
        assert!(tokens.contains(":: blake3 :: hash"));
        assert!(tokens.contains("to_hex"));
        assert!(tokens.contains("take"));
        assert!(tokens.contains("16"));
        assert!(tokens.contains("self . token"));
        Ok(())
    }
}
