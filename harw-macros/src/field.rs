//! `field!`-Expansion — validierte Feldnamen für `harw-observe`.
//!
//! Spec: AW0-02-Brief, Abschnitt 1 (`field!`); Vertragsabschnitt A.1
//! (`harw-observe`, `FieldName`) in `docs/aw-contract-master.md`.
//!
//! # Zweck
//! `::harw_observe::FieldName` hat bewusst keinen öffentlichen validierenden
//! Konstruktor (nur `from_static_unchecked`, `#[doc(hidden)]`) — der einzige
//! vorgesehene Weg, einen Feldnamen zu erzeugen, ist dieses Makro. Es prüft
//! zur Compile-Zeit, was ein Laufzeit-Konstruktor erst zur Laufzeit prüfen
//! könnte, sodass ein ungültiger Feldname niemals in ein Binary gelangt.
//!
//! # Regeln
//! Ein Feldname ist gültig, wenn er
//! 1. nicht leer ist,
//! 2. ausschließlich Zeichen aus `[a-z0-9_.]` enthält,
//! 3. nicht mit `.` beginnt oder endet,
//! 4. keine zwei aufeinanderfolgenden Punkte enthält.
//!
//! [`validate_field_name`] implementiert diese Regeln und wird sowohl von
//! diesem Modul als auch von [`crate::metrics`] (für `labels`-Einträge)
//! verwendet, damit beide Makros exakt dieselbe Definition von „gültiger
//! Feldname" teilen.

use proc_macro2::TokenStream;
use quote::quote;
use syn::LitStr;

/// Prüft, ob `name` ein gültiger `harw-observe`-Feldname ist.
///
/// Siehe Modul-Doc, Abschnitt "Regeln", für die vollständige Spezifikation.
///
/// # Errors
/// Liefert eine deutschsprachige, menschenlesbare Fehlerbeschreibung, wenn
/// `name` leer ist, ein Zeichen außerhalb `[a-z0-9_.]` enthält, mit `.`
/// beginnt/endet, oder `..` enthält.
///
/// # Design-doc reference
/// AW0-02-Brief, Abschnitt 1 (`field!`).
pub(crate) fn validate_field_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Feldname darf nicht leer sein".to_owned());
    }
    if let Some(c) = name
        .chars()
        .find(|c| !matches!(c, 'a'..='z' | '0'..='9' | '_' | '.'))
    {
        return Err(format!(
            "Feldname `{name}` enthält ungültiges Zeichen '{c}'; erlaubt sind nur [a-z0-9_.]"
        ));
    }
    if name.starts_with('.') || name.ends_with('.') {
        return Err(format!(
            "Feldname `{name}` darf nicht mit '.' beginnen oder enden"
        ));
    }
    if name.contains("..") {
        return Err(format!(
            "Feldname `{name}` darf keine zwei aufeinanderfolgenden Punkte enthalten"
        ));
    }
    Ok(())
}

/// Expander für das `field!`-Makro.
///
/// Parst `input` als einzelnes String-Literal, validiert es über
/// [`validate_field_name`] und erzeugt
/// `::harw_observe::FieldName::from_static_unchecked(<literal>)`.
///
/// # Errors
/// - `input` ist kein einzelnes String-Literal → `syn::Error` (von `syn`s
///   Parser).
/// - der Literalwert verletzt eine der in [`validate_field_name`]
///   beschriebenen Regeln → `syn::Error`, gespannt auf das Literal.
///
/// # Design-doc reference
/// AW0-02-Brief, Abschnitt 1 (`field!`).
pub(crate) fn expand_field(input: TokenStream) -> syn::Result<TokenStream> {
    let lit: LitStr = syn::parse2(input)?;
    let value = lit.value();
    validate_field_name(&value).map_err(|reason| syn::Error::new_spanned(&lit, reason))?;

    Ok(quote! {
        ::harw_observe::FieldName::from_static_unchecked(#lit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_field_name_rejects_empty() {
        let err = validate_field_name("").expect_err("empty name must be rejected");
        assert!(err.contains("darf nicht leer sein"));
    }

    #[test]
    fn validate_field_name_rejects_uppercase() {
        let err = validate_field_name("Child.Admitted").expect_err("uppercase must be rejected");
        assert!(err.contains("ungültiges Zeichen"));
    }

    #[test]
    fn validate_field_name_rejects_leading_dot() {
        let err = validate_field_name(".child").expect_err("leading dot must be rejected");
        assert!(err.contains("beginnen oder enden"));
    }

    #[test]
    fn validate_field_name_rejects_trailing_dot() {
        let err = validate_field_name("child.").expect_err("trailing dot must be rejected");
        assert!(err.contains("beginnen oder enden"));
    }

    #[test]
    fn validate_field_name_rejects_double_dot() {
        let err =
            validate_field_name("child..admitted").expect_err("double dot must be rejected");
        assert!(err.contains("aufeinanderfolgenden Punkte"));
    }

    #[test]
    fn validate_field_name_accepts_valid_names() {
        assert!(validate_field_name("child.admitted").is_ok());
        assert!(validate_field_name("clan").is_ok());
        assert!(validate_field_name("a_b.c_d").is_ok());
    }

    #[test]
    fn expand_field_rejects_non_string_literal() {
        let input: TokenStream = quote! { 42 };
        assert!(expand_field(input).is_err());
    }

    #[test]
    fn expand_field_rejects_invalid_value() {
        let input: TokenStream = quote! { "" };
        let err = expand_field(input).expect_err("empty literal must be rejected");
        assert!(err.to_string().contains("darf nicht leer sein"));
    }

    #[test]
    fn expand_field_accepts_valid_value() {
        let input: TokenStream = quote! { "child.admitted" };
        let tokens = expand_field(input)
            .expect("valid literal must expand")
            .to_string();
        assert!(tokens.contains("FieldName"));
        assert!(tokens.contains("from_static_unchecked"));
        assert!(tokens.contains("\"child.admitted\""));
    }
}
