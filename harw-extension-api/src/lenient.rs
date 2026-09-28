//! Nachsichtige Deserialisierung numerischer Tool-Argumente.
//!
//! # Verantwortung
//! Modelle senden numerische Tool-Argumente (`max_bytes`, `max_entries`, …)
//! gelegentlich als JSON-String (`"8000"`) statt als Zahl. `serde` lehnt das
//! standardmäßig ab, was in der Praxis zu einem messbaren Anteil scheiternder
//! Tool-Aufrufe führt (siehe AP TOOLARGS). Dieses Modul stellt
//! `deserialize_with`-Funktionen bereit, die sowohl Zahl als auch getrimmten
//! Ziffern-String akzeptieren, ohne das JSON-Schema (weiterhin `integer`) zu
//! verändern — die Nachsicht betrifft ausschließlich die Deserialisierung.
//!
//! # Schlüsseltypen
//! - [`lenient_opt_u64`], [`lenient_opt_usize`]: `deserialize_with`-Funktionen
//!   für `Option<u64>`- bzw. `Option<usize>`-Felder.
//!
//! # Nebenläufigkeit
//! Zustandslose freie Funktionen; `Send + Sync` trivial (keine Felder).
//!
//! # Fehler
//! Bei nicht-numerischem String, negativer Zahl, Fließkommazahl oder einem
//! `usize`-Überlauf wird `serde::de::Error::custom` mit der Meldung
//! "erwartet eine nicht-negative Ganzzahl (Zahl oder Ziffern-String)"
//! zurückgegeben.
//!
//! # Examples
//! ```rust,no_run
//! use serde::Deserialize;
//!
//! #[derive(Debug, Deserialize)]
//! struct Args {
//!     #[serde(default, deserialize_with = "harw_extension_api::lenient::lenient_opt_u64")]
//!     max_bytes: Option<u64>,
//! }
//! ```

use serde::Deserialize;
use serde::de::Error as _;

/// Interne Hilfsdarstellung: JSON-Zahl (in allen Vorzeichen-/Kommavarianten)
/// oder JSON-String.
///
/// `untagged` lässt `serde_json` alle Formen für dasselbe Feld akzeptieren;
/// die eigentliche Validierung (Ziffern, kein Vorzeichen, kein Dezimalpunkt)
/// passiert danach manuell in [`parse_digits`] bzw. direkt beim Matchen der
/// Varianten. `Neg` und `Float` existieren nur, damit negative Zahlen und
/// Fließkommazahlen mit der festen [`ERR_MSG`]-Meldung abgelehnt werden,
/// statt in serdes generische Fehlermeldung des untagged Enums zu fallen
/// (die entstünde, wenn keine Variante passt).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum NumOrString {
    /// Eine nicht-negative JSON-Ganzzahl.
    Num(u64),
    /// Eine negative JSON-Ganzzahl — wird stets abgelehnt.
    Neg(i64),
    /// Eine JSON-Fließkommazahl — wird stets abgelehnt.
    Float(f64),
    /// Ein JSON-String, der (getrimmt) nur ASCII-Ziffern enthalten darf.
    Str(String),
}

/// Fehlermeldung für alle Ablehnungsfälle dieses Moduls.
const ERR_MSG: &str = "erwartet eine nicht-negative Ganzzahl (Zahl oder Ziffern-String)";

/// Parst einen getrimmten Ziffern-String zu `u64`.
///
/// # Description
/// Lehnt leere Strings, Vorzeichen, Dezimalpunkte und alles Nicht-ASCII-Ziffrige
/// ab. Nur reine ASCII-Ziffern (`0-9`) nach dem Trimmen sind gültig.
fn parse_digits<E: serde::de::Error>(s: &str) -> Result<u64, E> {
    let trimmed = s.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(E::custom(ERR_MSG));
    }
    trimmed.parse::<u64>().map_err(|_| E::custom(ERR_MSG))
}

/// Deserialisiert ein optionales `u64`-Feld nachsichtig.
///
/// # Description
/// Akzeptiert `null`/fehlend (→ `None`), eine JSON-Zahl oder einen getrimmten
/// Ziffern-String. Negative Zahlen, Fließkommawerte und nicht-numerische
/// Strings werden abgelehnt.
///
/// # Errors
/// [`serde::de::Error::custom`] mit der Meldung
/// "erwartet eine nicht-negative Ganzzahl (Zahl oder Ziffern-String)".
///
/// # Examples
/// ```rust,no_run
/// use serde::Deserialize;
/// #[derive(Deserialize)]
/// struct Args {
///     #[serde(default, deserialize_with = "harw_extension_api::lenient::lenient_opt_u64")]
///     max_bytes: Option<u64>,
/// }
/// ```
pub fn lenient_opt_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    let opt = Option::<NumOrString>::deserialize(d)?;
    match opt {
        None => Ok(None),
        Some(NumOrString::Num(n)) => Ok(Some(n)),
        Some(NumOrString::Neg(_)) | Some(NumOrString::Float(_)) => Err(D::Error::custom(ERR_MSG)),
        Some(NumOrString::Str(s)) => parse_digits(&s).map(Some),
    }
}

/// Deserialisiert ein optionales `usize`-Feld nachsichtig.
///
/// # Description
/// Wie [`lenient_opt_u64`], konvertiert das Ergebnis aber zusätzlich per
/// `usize::try_from` — auf Plattformen mit 32-Bit-`usize` werden zu große
/// Werte abgelehnt.
///
/// # Errors
/// [`serde::de::Error::custom`] mit der Meldung
/// "erwartet eine nicht-negative Ganzzahl (Zahl oder Ziffern-String)".
///
/// # Examples
/// ```rust,no_run
/// use serde::Deserialize;
/// #[derive(Deserialize)]
/// struct Args {
///     #[serde(default, deserialize_with = "harw_extension_api::lenient::lenient_opt_usize")]
///     max_entries: Option<usize>,
/// }
/// ```
pub fn lenient_opt_usize<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<usize>, D::Error> {
    let opt = lenient_opt_u64(d)?;
    match opt {
        None => Ok(None),
        Some(n) => usize::try_from(n)
            .map(Some)
            .map_err(|_| D::Error::custom(ERR_MSG)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[derive(Debug, Deserialize)]
    struct U64Args {
        #[serde(default, deserialize_with = "lenient_opt_u64")]
        value: Option<u64>,
    }

    #[derive(Debug, Deserialize)]
    struct UsizeArgs {
        #[serde(default, deserialize_with = "lenient_opt_usize")]
        value: Option<usize>,
    }

    #[test]
    fn test_lenient_opt_u64_accepts_number() -> TestResult {
        let parsed: U64Args =
            serde_json::from_str(r#"{"value": 8000}"#).map_err(ctx("number parses"))?;
        assert_eq!(parsed.value, Some(8000));
        Ok(())
    }

    #[test]
    fn test_lenient_opt_u64_accepts_numeric_string() -> TestResult {
        let parsed: U64Args =
            serde_json::from_str(r#"{"value": "8000"}"#).map_err(ctx("numeric string parses"))?;
        assert_eq!(parsed.value, Some(8000));
        Ok(())
    }

    #[test]
    fn test_lenient_opt_u64_accepts_padded_numeric_string() -> TestResult {
        let parsed: U64Args = serde_json::from_str(r#"{"value": " 42 "}"#)
            .map_err(ctx("padded numeric string parses"))?;
        assert_eq!(parsed.value, Some(42));
        Ok(())
    }

    #[test]
    fn test_lenient_opt_u64_accepts_null() -> TestResult {
        let parsed: U64Args =
            serde_json::from_str(r#"{"value": null}"#).map_err(ctx("null parses"))?;
        assert_eq!(parsed.value, None);
        Ok(())
    }

    #[test]
    fn test_lenient_opt_u64_accepts_missing_field() -> TestResult {
        let parsed: U64Args = serde_json::from_str(r#"{}"#).map_err(ctx("missing field parses"))?;
        assert_eq!(parsed.value, None);
        Ok(())
    }

    #[test]
    fn test_lenient_opt_u64_rejects_float_string() -> TestResult {
        let Err(err) = serde_json::from_str::<U64Args>(r#"{"value": "8000.5"}"#) else {
            return Err(TestError::Unexpected(
                "float string must be rejected".into(),
            ));
        };
        assert!(err.to_string().contains(ERR_MSG));
        Ok(())
    }

    #[test]
    fn test_lenient_opt_u64_rejects_negative_string() -> TestResult {
        let Err(err) = serde_json::from_str::<U64Args>(r#"{"value": "-1"}"#) else {
            return Err(TestError::Unexpected(
                "negative string must be rejected".into(),
            ));
        };
        assert!(err.to_string().contains(ERR_MSG));
        Ok(())
    }

    #[test]
    fn test_lenient_opt_u64_rejects_non_numeric_string() -> TestResult {
        let Err(err) = serde_json::from_str::<U64Args>(r#"{"value": "abc"}"#) else {
            return Err(TestError::Unexpected(
                "non-numeric string must be rejected".into(),
            ));
        };
        assert!(err.to_string().contains(ERR_MSG));
        Ok(())
    }

    #[test]
    fn test_lenient_opt_u64_rejects_negative_number() -> TestResult {
        // JSON `-1` is not representable as u64 via the untagged `Num(u64)`
        // variant, but matches the `Neg(i64)` fallback variant, which is
        // rejected explicitly with `ERR_MSG` rather than serde's generic
        // "did not match any variant" message.
        let Err(err) = serde_json::from_str::<U64Args>(r#"{"value": -1}"#) else {
            return Err(TestError::Unexpected(
                "negative number must be rejected".into(),
            ));
        };
        assert!(err.to_string().contains(ERR_MSG));
        Ok(())
    }

    #[test]
    fn test_lenient_opt_u64_rejects_float_number() -> TestResult {
        // JSON `8000.5` matches neither `Num(u64)` nor `Neg(i64)`, but the
        // `Float(f64)` fallback variant, which is rejected explicitly with
        // `ERR_MSG`.
        let Err(err) = serde_json::from_str::<U64Args>(r#"{"value": 8000.5}"#) else {
            return Err(TestError::Unexpected(
                "float number must be rejected".into(),
            ));
        };
        assert!(err.to_string().contains(ERR_MSG));
        Ok(())
    }

    #[test]
    fn test_lenient_opt_usize_accepts_numeric_string() -> TestResult {
        let parsed: UsizeArgs =
            serde_json::from_str(r#"{"value": "100"}"#).map_err(ctx("numeric string parses"))?;
        assert_eq!(parsed.value, Some(100));
        Ok(())
    }

    #[test]
    fn test_lenient_opt_usize_accepts_number() -> TestResult {
        let parsed: UsizeArgs =
            serde_json::from_str(r#"{"value": 100}"#).map_err(ctx("number parses"))?;
        assert_eq!(parsed.value, Some(100));
        Ok(())
    }
}
