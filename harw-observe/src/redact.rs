//! Redaktion für Diagnoseausgaben.
//!
//! # Verantwortungsbereich
//! Trägt [`Redacted`] und [`Redact`] (Vertrag A.5,
//! `docs/design/build-history.md`). Legt fest, wie ein Wert in Logs und
//! Diagnosen erscheinen darf — nie, wie er intern verarbeitet wird. Ein
//! künftiges Ableitungsmakro (außerhalb dieser Crate) wird
//! `Redacted::Omitted` als Voreinstellung erzeugen: ein Feld, über das
//! niemand nachgedacht hat, erscheint gar nicht.
//!
//! # Nebenläufigkeit
//! `Redacted` ist `Clone` und ohne Interior Mutability. `Redact::redact`
//! nimmt `&self` und darf beliebig aus jedem Thread aufgerufen werden.
//!
//! # Fehler
//! Keine — Redaktion ist eine reine, unfehlbare Abbildung.
//!
//! # Examples
//! ```
//! use harw_observe::{FieldValue, Redact, Redacted};
//!
//! let value = FieldValue::I64(42);
//! assert_eq!(value.redact(), Redacted::Shown("42".to_owned()));
//! ```

use crate::field::FieldValue;

/// Wie ein Wert in Diagnoseausgaben erscheint.
///
/// Die Voreinstellung des Ableitungsmakros ist [`Redacted::Omitted`]: ein
/// Feld, über das niemand nachgedacht hat, erscheint gar nicht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redacted {
    /// Erscheint überhaupt nicht in der Ausgabe.
    Omitted,
    /// Erscheint unverändert.
    Shown(String),
    /// Erscheint als Digest/Hash statt im Klartext.
    Hashed(String),
}

/// Ein Typ, der weiß, wie er in Diagnoseausgaben erscheinen darf.
pub trait Redact {
    /// Die diagnosetaugliche Form dieses Wertes.
    ///
    /// # Returns
    /// Die [`Redacted`]-Form, in der dieser Wert in Logs/Diagnosen
    /// erscheinen darf.
    fn redact(&self) -> Redacted;
}

impl Redact for FieldValue {
    /// `FieldValue` trägt laut Vertrag A.1 „Zahlen und geschlossene
    /// Aufzählungen, keine Inhalte" — alle Varianten sind daher unbedenklich
    /// zeigbar (`Redacted::Shown`), nie geheim.
    fn redact(&self) -> Redacted {
        match self {
            FieldValue::Str(s) => Redacted::Shown((*s).to_owned()),
            FieldValue::Owned(s) => Redacted::Shown(s.to_owned()),
            FieldValue::I64(v) => Redacted::Shown(v.to_string()),
            FieldValue::U64(v) => Redacted::Shown(v.to_string()),
            FieldValue::F64(v) => Redacted::Shown(v.to_string()),
            FieldValue::Bool(v) => Redacted::Shown(v.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redacted_variants_are_distinguishable() {
        assert_ne!(Redacted::Omitted, Redacted::Shown("x".to_owned()));
        assert_ne!(
            Redacted::Shown("x".to_owned()),
            Redacted::Hashed("x".to_owned())
        );
    }

    #[test]
    fn test_field_value_str_redacts_to_shown() {
        assert_eq!(
            FieldValue::Str("gauge").redact(),
            Redacted::Shown("gauge".to_owned())
        );
    }

    #[test]
    fn test_field_value_owned_redacts_to_shown() {
        assert_eq!(
            FieldValue::Owned("dyn".to_owned()).redact(),
            Redacted::Shown("dyn".to_owned())
        );
    }

    #[test]
    fn test_field_value_bool_redacts_to_shown() {
        assert_eq!(
            FieldValue::Bool(true).redact(),
            Redacted::Shown("true".to_owned())
        );
    }

    #[test]
    fn test_field_value_f64_redacts_to_shown() {
        assert_eq!(
            FieldValue::F64(1.5).redact(),
            Redacted::Shown("1.5".to_owned())
        );
    }

    #[test]
    fn test_field_value_u64_redacts_to_shown() {
        assert_eq!(FieldValue::U64(7).redact(), Redacted::Shown("7".to_owned()));
    }
}
