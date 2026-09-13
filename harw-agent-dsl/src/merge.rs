//! Explizite Merge-Semantik für TOML-Feldoperationen (§7 DSL-Spec).
//!
//! Dieses Modul definiert [`MergeOp`], die Menge der zulässigen expliziten
//! Merge-Operatoren, sowie [`apply_merge_op`], die eine Operation auf einem
//! `toml::Value`-Slot ausführt.
//!
//! # Schlüsseltypen
//! - [`MergeOp`] — diskriminierter Union aller Merge-Operationen
//!
//! # Invarianten (§7)
//! - Jedes geerbte Feld folgt genau einem expliziten Operator.
//! - Für Authority-tragende Felder ist nur `Intersect` und `Remove` erlaubt.
//! - `min`/`max-within-parent` sind für eine Folge-Wave reserviert.
//!
//! # Nebenläufigkeit
//! `apply_merge_op` ist zustandslos und thread-sicher.

use serde::{Deserialize, Serialize};

use crate::error::DslError;

/// Explizite Merge-Operation für ein TOML-Feld (§7).
///
/// # Beschreibung
/// Jeder geerbte Wert muss durch genau eine dieser Operationen aktualisiert werden.
/// Unsichtbare rekursive TOML-Merges sind verboten (§7: "Harwness must not use an
/// invisible recursive TOML merge").
///
/// # Varianten
/// - `Replace` — ersetzt den aktuellen Wert vollständig.
/// - `Append` — hängt Elemente an ein Array an.
/// - `Prepend` — stellt Elemente einem Array voran.
/// - `Remove` — entfernt Elemente aus einem Array.
/// - `Intersect` — behält nur Elemente, die in beiden Mengen vorkommen.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::merge::MergeOp;
///
/// let op = MergeOp::Replace { value: toml::Value::String("neu".to_owned()) };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MergeOp {
    /// Ersetzt den aktuellen Wert vollständig.
    Replace {
        /// Neuer Wert.
        value: toml::Value,
    },
    /// Hängt Elemente an ein Array an.
    Append {
        /// Anzuhängende Elemente.
        values: Vec<toml::Value>,
    },
    /// Stellt Elemente einem Array voran.
    Prepend {
        /// Voranzustellende Elemente.
        values: Vec<toml::Value>,
    },
    /// Entfernt Elemente aus einem Array.
    Remove {
        /// Zu entfernende Elemente.
        values: Vec<toml::Value>,
    },
    /// Behält nur Elemente, die in beiden Mengen vorkommen (für Authority-Felder).
    Intersect {
        /// Referenzmenge für die Schnittbildung.
        values: Vec<toml::Value>,
    },
}

/// Wendet eine [`MergeOp`] auf einen `toml::Value`-Slot an.
///
/// # Beschreibung
/// Modifiziert `current` gemäß der übergebenen Operation in-place.
/// Array-Operationen (`Append`, `Prepend`, `Remove`, `Intersect`) erfordern,
/// dass `current` ein `toml::Value::Array` ist oder durch `Replace` wird.
///
/// # Argumente
/// - `current` (`&mut toml::Value`): der zu modifizierende Wert.
/// - `op` (`MergeOp`): die anzuwendende Operation.
///
/// # Rückgabe
/// `Ok(())` bei Erfolg.
///
/// # Fehler
/// - [`DslError::UnknownMergeOp`]: wenn eine Array-Operation auf einem Nicht-Array ausgeführt wird.
///
/// # Nebenläufigkeit
/// Zustandslos; thread-sicher unter exklusivem Zugriff auf `current`.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::merge::{MergeOp, apply_merge_op};
///
/// let mut val = toml::Value::Array(vec![
///     toml::Value::String("a".to_owned()),
/// ]);
/// apply_merge_op(&mut val, MergeOp::Append {
///     values: vec![toml::Value::String("b".to_owned())],
/// }).unwrap();
///
/// let arr = val.as_array().unwrap();
/// assert_eq!(arr.len(), 2);
/// ```
pub fn apply_merge_op(current: &mut toml::Value, op: MergeOp) -> Result<(), DslError> {
    match op {
        MergeOp::Replace { value } => {
            *current = value;
            Ok(())
        }
        MergeOp::Append { values } => {
            let arr = require_array(current)?;
            arr.extend(values);
            Ok(())
        }
        MergeOp::Prepend { values } => {
            let arr = require_array(current)?;
            let mut new_arr = values;
            new_arr.append(arr);
            *arr = new_arr;
            Ok(())
        }
        MergeOp::Remove { values } => {
            let arr = require_array(current)?;
            arr.retain(|item| !values.contains(item));
            Ok(())
        }
        MergeOp::Intersect { values } => {
            let arr = require_array(current)?;
            arr.retain(|item| values.contains(item));
            Ok(())
        }
    }
}

/// Erzwingt, dass `val` ein `toml::Value::Array` ist, und gibt eine mutable Referenz zurück.
///
/// # Fehler
/// - [`DslError::UnknownMergeOp`]: wenn `val` kein Array ist.
fn require_array(val: &mut toml::Value) -> Result<&mut Vec<toml::Value>, DslError> {
    if !val.is_array() {
        let type_name = val.type_str().to_owned();
        return Err(DslError::UnknownMergeOp {
            name: format!("Array-Operation auf Nicht-Array-Typ '{type_name}'"),
        });
    }
    Ok(val.as_array_mut().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn str_val(s: &str) -> toml::Value {
        toml::Value::String(s.to_owned())
    }

    fn arr(items: &[&str]) -> toml::Value {
        toml::Value::Array(items.iter().map(|s| str_val(s)).collect())
    }

    #[test]
    fn test_replace_op() {
        let mut val = str_val("alt");
        apply_merge_op(
            &mut val,
            MergeOp::Replace {
                value: str_val("neu"),
            },
        )
        .unwrap();
        assert_eq!(val, str_val("neu"));
    }

    #[test]
    fn test_append_op_on_array() {
        let mut val = arr(&["a", "b"]);
        apply_merge_op(
            &mut val,
            MergeOp::Append {
                values: vec![str_val("c")],
            },
        )
        .unwrap();
        assert_eq!(val.as_array().unwrap().len(), 3);
        assert_eq!(val.as_array().unwrap()[2], str_val("c"));
    }

    #[test]
    fn test_prepend_op_on_array() {
        let mut val = arr(&["b", "c"]);
        apply_merge_op(
            &mut val,
            MergeOp::Prepend {
                values: vec![str_val("a")],
            },
        )
        .unwrap();
        let items = val.as_array().unwrap();
        assert_eq!(items[0], str_val("a"));
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn test_remove_op() {
        let mut val = arr(&["web-search", "file-read", "web-fetch"]);
        apply_merge_op(
            &mut val,
            MergeOp::Remove {
                values: vec![str_val("web-search"), str_val("web-fetch")],
            },
        )
        .unwrap();
        let items = val.as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], str_val("file-read"));
    }

    #[test]
    fn test_intersect_op_on_array() {
        let mut val = arr(&["a", "b", "c", "d"]);
        apply_merge_op(
            &mut val,
            MergeOp::Intersect {
                values: vec![str_val("b"), str_val("d"), str_val("e")],
            },
        )
        .unwrap();
        let items = val.as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.contains(&str_val("b")));
        assert!(items.contains(&str_val("d")));
    }

    #[test]
    fn test_append_on_non_array_errors() {
        let mut val = str_val("kein-array");
        let result = apply_merge_op(
            &mut val,
            MergeOp::Append {
                values: vec![str_val("x")],
            },
        );
        assert!(matches!(result, Err(DslError::UnknownMergeOp { .. })));
    }

    #[test]
    fn test_replace_on_array_works() {
        let mut val = arr(&["a"]);
        apply_merge_op(
            &mut val,
            MergeOp::Replace {
                value: toml::Value::Integer(42),
            },
        )
        .unwrap();
        assert_eq!(val, toml::Value::Integer(42));
    }
}
