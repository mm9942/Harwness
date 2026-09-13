//! Fehlertyp dieser Crate.
//!
//! # Verantwortungsbereich
//! Trägt den einzigen Fehlertyp von `harw-context`: [`ContextError`], gebaut
//! mit `#[derive(harw_macros::HarwError)]` nach dem Muster aus dem
//! Contract-Master (Kopfteil „Fehler"). Der einzige Fehlerfall dieser Crate
//! ist ein ungültiger Name: [`crate::fragment::SectionName`],
//! [`crate::fragment::FragmentLabel`] und [`crate::selector::Selector`]
//! verlangen alle drei dasselbe — nicht leer, keine Steuerzeichen — und
//! teilen sich deshalb den Validierungspfad [`validate_name`], statt die
//! Regel dreimal zu wiederholen.
//!
//! Fehlgeschlagene Fragment-Zulassungen laufen **nicht** über diesen Typ,
//! sondern über [`crate::ceiling::CeilingViolation`]: eine Ablehnung durch
//! eine `ContextCeiling` ist kein Konstruktionsfehler, sondern eine
//! Richtlinienentscheidung mit eigenem, spezifischerem Vokabular.
//!
//! # Exportierte Typen
//! [`ContextError`].
//!
//! # Nebenläufigkeit
//! Reiner Werttyp ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_context::SectionName;
//!
//! let err = SectionName::try_new("").unwrap_err();
//! assert_eq!(
//!     err.to_string(),
//!     "section name '' is empty or contains control characters"
//! );
//! ```

/// Fehler dieser Crate.
///
/// # Description
/// Ein einziger Fehlerfall: ein Name (Sektion, Fragment-Label oder
/// Selektor-Muster) ist nach dem Trimmen leer oder enthält ein
/// Unicode-Steuerzeichen. Das Feld `kind` benennt, welcher der drei
/// Namenstypen betroffen war, damit die Fehlermeldung ohne Blick in den
/// Aufrufcode verständlich bleibt (CLAUDE.md-Vorgabe: jede Variante trägt
/// den vollen Kontext).
#[derive(Debug, Clone, PartialEq, Eq, harw_macros::HarwError)]
pub enum ContextError {
    /// Der Name ist nach dem Trimmen leer oder enthält ein Steuerzeichen.
    #[msg("{kind} name '{value}' is empty or contains control characters")]
    InvalidName {
        /// Welcher Namenstyp betroffen war, z. B. `"section"`, `"fragment
        /// label"` oder `"selector pattern"`.
        kind: &'static str,
        /// Der abgelehnte Rohwert, unverändert.
        value: String,
    },
}

/// Validiert einen Namen nach der gemeinsamen Regel aller Namenstypen dieser
/// Crate.
///
/// # Description
/// Ein Name ist gültig, wenn er nach dem Trimmen nicht leer ist und kein
/// Unicode-Steuerzeichen enthält. Wird von `SectionName::try_new`,
/// `FragmentLabel::try_new` und `Selector::try_new` geteilt, damit die Regel
/// an genau einer Stelle steht statt dreimal dupliziert zu werden.
///
/// # Arguments
/// - `kind` (`&'static str`): Bezeichner des Namenstyps für die Fehlermeldung.
/// - `value` (`impl Into<String>`): der zu prüfende Rohwert.
///
/// # Returns
/// `Ok(String)` mit dem unveränderten Eingabewert, wenn er gültig ist.
///
/// # Errors
/// [`ContextError::InvalidName`], wenn der getrimmte Wert leer ist oder ein
/// Steuerzeichen enthält.
///
/// `pub(crate)`, daher ohne `# Examples`: ein Doctest sieht die Crate von
/// außen und könnte diese Funktion nicht aufrufen. Die Testabdeckung liegt
/// in `#[cfg(test)] mod tests` weiter unten.
pub(crate) fn validate_name(
    kind: &'static str,
    value: impl Into<String>,
) -> Result<String, ContextError> {
    let value = value.into();
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(ContextError::InvalidName { kind, value });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{validate_name, ContextError};

    #[test]
    fn test_validate_name_rejects_empty_value() {
        assert!(matches!(
            validate_name("section", ""),
            Err(ContextError::InvalidName { .. })
        ));
    }

    #[test]
    fn test_validate_name_rejects_whitespace_only_value() {
        assert!(matches!(
            validate_name("section", "   \t"),
            Err(ContextError::InvalidName { .. })
        ));
    }

    #[test]
    fn test_validate_name_rejects_control_character() {
        assert!(matches!(
            validate_name("fragment label", "a\u{0007}b"),
            Err(ContextError::InvalidName { .. })
        ));
    }

    #[test]
    fn test_validate_name_accepts_plain_name() {
        assert_eq!(
            validate_name("section", "history.tail").unwrap(),
            "history.tail"
        );
    }

    #[test]
    fn test_context_error_display_names_kind_and_value() {
        let err = ContextError::InvalidName {
            kind: "section",
            value: String::new(),
        };
        assert_eq!(
            err.to_string(),
            "section name '' is empty or contains control characters"
        );
    }
}
