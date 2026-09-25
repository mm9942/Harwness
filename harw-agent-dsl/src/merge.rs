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
//! - `min` ergibt das Kleinere aus geerbtem und gegebenem Zahlenwert (ein
//!   Patch kann nur verschärfen); `max-within-parent` übernimmt den gegebenen
//!   Wert, verlangt aber, dass er den geerbten nicht übersteigt — mehr zu
//!   verlangen ist ein Fehler ([`DslError::PatchExceedsParent`]), keine
//!   stille Kappung.
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
/// - `Min` — das Kleinere aus geerbtem und gegebenem Zahlenwert.
/// - `MaxWithinParent` — der gegebene Zahlenwert, höchstens der geerbte.
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
    /// Das Kleinere aus geerbtem und gegebenem Zahlenwert (§7 `min`).
    Min {
        /// Gegebener Zahlenwert (Integer oder Float).
        value: toml::Value,
    },
    /// Der gegebene Zahlenwert, sofern er den geerbten nicht übersteigt
    /// (§7 `max-within-parent`); sonst [`DslError::PatchExceedsParent`].
    #[serde(rename = "max-within-parent")]
    MaxWithinParent {
        /// Gegebener Zahlenwert (Integer oder Float).
        value: toml::Value,
    },
}

impl MergeOp {
    /// Der Operatorname, wie er in `[patch.*]`-Tabellen steht.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            MergeOp::Replace { .. } => "replace",
            MergeOp::Append { .. } => "append",
            MergeOp::Prepend { .. } => "prepend",
            MergeOp::Remove { .. } => "remove",
            MergeOp::Intersect { .. } => "intersect",
            MergeOp::Min { .. } => "min",
            MergeOp::MaxWithinParent { .. } => "max-within-parent",
        }
    }
}

/// Die Schlüssel, unter denen eine Patch-Tabelle Operatoren führt (§7), in
/// Anwendungsreihenfolge. `max_within_parent` ist als Schreibweise ohne
/// Bindestrich zusätzlich zugelassen.
pub const PATCH_OPERATOR_KEYS: &[&str] = &[
    "replace",
    "prepend",
    "append",
    "remove",
    "intersect",
    "min",
    "max-within-parent",
    "max_within_parent",
];

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
/// - [`DslError::PatchTypeMismatch`]: wenn eine Array-Operation auf einem
///   Nicht-Array oder `min`/`max-within-parent` auf einer Nicht-Zahl läuft.
/// - [`DslError::PatchExceedsParent`]: wenn `max-within-parent` mehr verlangt
///   als der geerbte Wert.
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
            let arr = require_array(current, "append")?;
            arr.extend(values);
            Ok(())
        }
        MergeOp::Prepend { values } => {
            let arr = require_array(current, "prepend")?;
            let mut new_arr = values;
            new_arr.append(arr);
            *arr = new_arr;
            Ok(())
        }
        MergeOp::Remove { values } => {
            let arr = require_array(current, "remove")?;
            arr.retain(|item| !values.contains(item));
            Ok(())
        }
        MergeOp::Intersect { values } => {
            let arr = require_array(current, "intersect")?;
            arr.retain(|item| values.contains(item));
            Ok(())
        }
        MergeOp::Min { value } => {
            let inherited = require_number(current, "min")?;
            let given = require_number(&value, "min")?;
            if given < inherited {
                *current = value;
            }
            Ok(())
        }
        MergeOp::MaxWithinParent { value } => {
            let inherited = require_number(current, "max-within-parent")?;
            let given = require_number(&value, "max-within-parent")?;
            if given > inherited {
                return Err(DslError::PatchExceedsParent {
                    requested: value.to_string(),
                    parent: current.to_string(),
                    location: crate::error::DiagLocation::none(),
                });
            }
            *current = value;
            Ok(())
        }
    }
}

/// Erzwingt, dass `val` ein `toml::Value::Array` ist, und gibt eine mutable Referenz zurück.
///
/// # Fehler
/// - [`DslError::PatchTypeMismatch`]: wenn `val` kein Array ist.
fn require_array<'a>(
    val: &'a mut toml::Value,
    op: &str,
) -> Result<&'a mut Vec<toml::Value>, DslError> {
    let type_name = val.type_str();
    val.as_array_mut()
        .ok_or_else(|| DslError::PatchTypeMismatch {
            op: op.to_owned(),
            expected: "array",
            found: type_name,
            location: crate::error::DiagLocation::none(),
        })
}

/// Liest einen Zahlenwert (Integer oder Float) als `f64` für den Vergleich
/// von `min`/`max-within-parent`.
///
/// Integer bis 2^53 werden exakt verglichen; größere Werte sind für Limits
/// bedeutungslos.
///
/// # Fehler
/// - [`DslError::PatchTypeMismatch`]: wenn `val` keine Zahl ist.
#[allow(clippy::cast_precision_loss)]
fn require_number(val: &toml::Value, op: &str) -> Result<f64, DslError> {
    match val {
        toml::Value::Integer(n) => Ok(*n as f64),
        toml::Value::Float(n) => Ok(*n),
        other => Err(DslError::PatchTypeMismatch {
            op: op.to_owned(),
            expected: "number",
            found: other.type_str(),
            location: crate::error::DiagLocation::none(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn str_val(s: &str) -> toml::Value {
        toml::Value::String(s.to_owned())
    }

    fn arr(items: &[&str]) -> toml::Value {
        toml::Value::Array(items.iter().map(|s| str_val(s)).collect())
    }

    #[test]
    fn test_replace_op() -> TestResult {
        let mut val = str_val("alt");
        apply_merge_op(
            &mut val,
            MergeOp::Replace {
                value: str_val("neu"),
            },
        )?;
        assert_eq!(val, str_val("neu"));
        Ok(())
    }

    #[test]
    fn test_append_op_on_array() -> TestResult {
        let mut val = arr(&["a", "b"]);
        apply_merge_op(
            &mut val,
            MergeOp::Append {
                values: vec![str_val("c")],
            },
        )?;
        let items = val.as_array().ok_or(TestError::Missing("val as array"))?;
        assert_eq!(items.len(), 3);
        assert_eq!(items[2], str_val("c"));
        Ok(())
    }

    #[test]
    fn test_prepend_op_on_array() -> TestResult {
        let mut val = arr(&["b", "c"]);
        apply_merge_op(
            &mut val,
            MergeOp::Prepend {
                values: vec![str_val("a")],
            },
        )?;
        let items = val.as_array().ok_or(TestError::Missing("val as array"))?;
        assert_eq!(items[0], str_val("a"));
        assert_eq!(items.len(), 3);
        Ok(())
    }

    #[test]
    fn test_remove_op() -> TestResult {
        let mut val = arr(&["web-search", "file-read", "web-fetch"]);
        apply_merge_op(
            &mut val,
            MergeOp::Remove {
                values: vec![str_val("web-search"), str_val("web-fetch")],
            },
        )?;
        let items = val.as_array().ok_or(TestError::Missing("val as array"))?;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], str_val("file-read"));
        Ok(())
    }

    #[test]
    fn test_intersect_op_on_array() -> TestResult {
        let mut val = arr(&["a", "b", "c", "d"]);
        apply_merge_op(
            &mut val,
            MergeOp::Intersect {
                values: vec![str_val("b"), str_val("d"), str_val("e")],
            },
        )?;
        let items = val.as_array().ok_or(TestError::Missing("val as array"))?;
        assert_eq!(items.len(), 2);
        assert!(items.contains(&str_val("b")));
        assert!(items.contains(&str_val("d")));
        Ok(())
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
        assert!(matches!(result, Err(DslError::PatchTypeMismatch { .. })));
    }

    #[test]
    fn test_min_takes_the_smaller_value() -> TestResult {
        let mut val = toml::Value::Integer(40);
        apply_merge_op(
            &mut val,
            MergeOp::Min {
                value: toml::Value::Integer(24),
            },
        )?;
        assert_eq!(val, toml::Value::Integer(24));
        apply_merge_op(
            &mut val,
            MergeOp::Min {
                value: toml::Value::Integer(100),
            },
        )?;
        assert_eq!(val, toml::Value::Integer(24), "min never raises");
        Ok(())
    }

    #[test]
    fn test_min_on_non_number_is_a_type_mismatch() {
        let mut val = str_val("x");
        let result = apply_merge_op(
            &mut val,
            MergeOp::Min {
                value: toml::Value::Integer(1),
            },
        );
        assert!(matches!(result, Err(DslError::PatchTypeMismatch { .. })));
    }

    #[test]
    fn test_max_within_parent_sets_value_at_or_below_parent() -> TestResult {
        let mut val = toml::Value::Integer(40);
        apply_merge_op(
            &mut val,
            MergeOp::MaxWithinParent {
                value: toml::Value::Integer(40),
            },
        )?;
        assert_eq!(val, toml::Value::Integer(40));
        apply_merge_op(
            &mut val,
            MergeOp::MaxWithinParent {
                value: toml::Value::Integer(10),
            },
        )?;
        assert_eq!(val, toml::Value::Integer(10));
        Ok(())
    }

    #[test]
    fn test_max_within_parent_above_parent_is_an_error_not_a_clamp() {
        let mut val = toml::Value::Integer(40);
        let result = apply_merge_op(
            &mut val,
            MergeOp::MaxWithinParent {
                value: toml::Value::Integer(41),
            },
        );
        assert!(matches!(result, Err(DslError::PatchExceedsParent { .. })));
        assert_eq!(val, toml::Value::Integer(40), "the value stays unchanged");
    }

    #[test]
    fn test_merge_op_names_match_patch_keys() {
        for op in [
            MergeOp::Replace {
                value: str_val("a"),
            },
            MergeOp::Append { values: vec![] },
            MergeOp::Prepend { values: vec![] },
            MergeOp::Remove { values: vec![] },
            MergeOp::Intersect { values: vec![] },
            MergeOp::Min {
                value: toml::Value::Integer(1),
            },
            MergeOp::MaxWithinParent {
                value: toml::Value::Integer(1),
            },
        ] {
            assert!(PATCH_OPERATOR_KEYS.contains(&op.name()), "{}", op.name());
        }
    }

    #[test]
    fn test_replace_on_array_works() -> TestResult {
        let mut val = arr(&["a"]);
        apply_merge_op(
            &mut val,
            MergeOp::Replace {
                value: toml::Value::Integer(42),
            },
        )?;
        assert_eq!(val, toml::Value::Integer(42));
        Ok(())
    }
}
