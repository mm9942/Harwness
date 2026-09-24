//! Trace-Kontext: Verknüpfung von Arbeit über Prozess- und
//! Sitzungsgrenzen hinweg.
//!
//! # Verantwortungsbereich
//! Trägt [`TraceContext`] (Vertrag A.4, `docs/design/build-history.md`) —
//! ein eingefrorener Wire-Typ, seit AW1-01 ihn in `StoredJob` und
//! `ChildLeaseRecord` schreibt. Jede Feldänderung nach diesem Punkt bricht
//! Bestandsdateien; die Serde-Form (drei Felder, `deny_unknown_fields`)
//! ist deshalb wörtlich aus dem Vertrag übernommen.
//!
//! # Nebenläufigkeit
//! `TraceContext` trägt nur `String`/`Option<String>`-Felder, ist
//! `Send + Sync` und ohne Interior Mutability.
//!
//! # Fehler
//! Die validierenden Konstruktoren [`TraceContext::new`] und
//! [`TraceContext::with_parent`] geben [`crate::error::ObserveError`]
//! zurück, wenn eine Kennung nicht dem Hex-Format entspricht. Die
//! `pub`-Felder selbst bleiben ungeschützt konstruierbar
//! (`TraceContext { trace_id, span_id, parent_span_id }`) — das ist eine
//! bewusste Vertragsentscheidung (A.4), keine Lücke dieses Moduls.
//!
//! # Examples
//! ```
//! use harw_observe::TraceContext;
//!
//! let ctx = TraceContext::new("0".repeat(32), "1".repeat(16)).expect("gültige Hexzeichenketten");
//! assert!(ctx.parent_span_id.is_none());
//! ```

use crate::error::ObserveError;

/// Verknüpft Arbeit über Prozess- und Sitzungsgrenzen hinweg.
///
/// # Warum die Serde-Form hier festgelegt wird
/// AW1-01 schreibt diesen Typ in `StoredJob` und `ChildLeaseRecord` —
/// beides bestehende Dateiformate mit der Anforderung „bestehende Dateien
/// bleiben lesbar". Nach AW1-01 bricht jede Formänderung Bestandsdateien.
/// Der Typ trägt daher dieselbe Härte wie ein Wire-Typ.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceContext {
    /// 32 Hexzeichen.
    pub trace_id: String,
    /// 16 Hexzeichen.
    pub span_id: String,
    /// Elternspanne, falls die Arbeit geerbt wurde.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
}

impl TraceContext {
    /// Baut einen `TraceContext` aus geprüften Kennungen.
    ///
    /// # Description
    /// Validiert Länge und Zeichensatz von `trace_id` (32 Hexzeichen,
    /// Kleinschreibung) und `span_id` (16 Hexzeichen, Kleinschreibung),
    /// bevor der Wert entsteht. `parent_span_id` bleibt `None`; siehe
    /// [`TraceContext::with_parent`] für eine geerbte Spanne.
    ///
    /// # Arguments
    /// - `trace_id` (`impl Into<String>`): 32 Hexzeichen (Kleinschreibung).
    /// - `span_id` (`impl Into<String>`): 16 Hexzeichen (Kleinschreibung).
    ///
    /// # Returns
    /// Der validierte `TraceContext` ohne Elternspanne.
    ///
    /// # Errors
    /// - [`ObserveError::InvalidTraceId`]: `trace_id` ist keine 32
    ///   Hexzeichen.
    /// - [`ObserveError::InvalidSpanId`]: `span_id` ist keine 16
    ///   Hexzeichen.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::TraceContext;
    /// assert!(TraceContext::new("0".repeat(32), "0".repeat(16)).is_ok());
    /// assert!(TraceContext::new("too-short", "0".repeat(16)).is_err());
    /// ```
    pub fn new(
        trace_id: impl Into<String>,
        span_id: impl Into<String>,
    ) -> Result<Self, ObserveError> {
        let trace_id = trace_id.into();
        let span_id = span_id.into();
        if !is_lowercase_hex_of_len(&trace_id, 32) {
            return Err(ObserveError::InvalidTraceId { value: trace_id });
        }
        if !is_lowercase_hex_of_len(&span_id, 16) {
            return Err(ObserveError::InvalidSpanId { value: span_id });
        }
        Ok(Self {
            trace_id,
            span_id,
            parent_span_id: None,
        })
    }

    /// Setzt die Elternspanne.
    ///
    /// # Arguments
    /// - `parent_span_id` (`impl Into<String>`): 16 Hexzeichen der
    ///   Elternspanne.
    ///
    /// # Returns
    /// `self` mit gesetztem `parent_span_id`.
    ///
    /// # Errors
    /// [`ObserveError::InvalidSpanId`], wenn `parent_span_id` keine 16
    /// Hexzeichen sind.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::TraceContext;
    /// let ctx = TraceContext::new("0".repeat(32), "0".repeat(16))
    ///     .unwrap()
    ///     .with_parent("1".repeat(16))
    ///     .unwrap();
    /// assert_eq!(ctx.parent_span_id.as_deref(), Some("1111111111111111"));
    /// ```
    pub fn with_parent(mut self, parent_span_id: impl Into<String>) -> Result<Self, ObserveError> {
        let parent_span_id = parent_span_id.into();
        if !is_lowercase_hex_of_len(&parent_span_id, 16) {
            return Err(ObserveError::InvalidSpanId {
                value: parent_span_id,
            });
        }
        self.parent_span_id = Some(parent_span_id);
        Ok(self)
    }
}

/// Prüft, ob `value` aus genau `len` Kleinbuchstaben-Hexzeichen besteht.
fn is_lowercase_hex_of_len(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    // Lokale Variablen heißen hier `ctx` (TraceContext-Instanzen) — der Test-Helfer
    // `ctx()` wird deshalb überall voll qualifiziert aufgerufen, um die Verschattung
    // zu vermeiden (siehe Worker-Zusatz).

    #[test]
    fn test_trace_context_new_accepts_valid_hex() -> TestResult {
        let ctx = TraceContext::new("a".repeat(32), "b".repeat(16))
            .map_err(crate::test_support::ctx("TraceContext::new"))?;
        assert_eq!(ctx.trace_id, "a".repeat(32));
        assert_eq!(ctx.span_id, "b".repeat(16));
        assert!(ctx.parent_span_id.is_none());
        Ok(())
    }

    #[test]
    fn test_trace_context_new_rejects_wrong_length() -> TestResult {
        let result = TraceContext::new("short", "b".repeat(16));
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, ObserveError::InvalidTraceId { .. }));
        Ok(())
    }

    #[test]
    fn test_trace_context_new_rejects_uppercase() -> TestResult {
        let result = TraceContext::new("A".repeat(32), "b".repeat(16));
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, ObserveError::InvalidTraceId { .. }));
        Ok(())
    }

    #[test]
    fn test_trace_context_new_rejects_invalid_span_id() -> TestResult {
        let result = TraceContext::new("a".repeat(32), "bad");
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, ObserveError::InvalidSpanId { .. }));
        Ok(())
    }

    #[test]
    fn test_trace_context_with_parent_sets_field() -> TestResult {
        let ctx = TraceContext::new("a".repeat(32), "b".repeat(16))
            .map_err(crate::test_support::ctx("TraceContext::new"))?
            .with_parent("c".repeat(16))
            .map_err(crate::test_support::ctx("TraceContext::with_parent"))?;
        assert_eq!(ctx.parent_span_id.as_deref(), Some("c".repeat(16).as_str()));
        Ok(())
    }

    #[test]
    fn test_trace_context_with_parent_rejects_invalid() -> TestResult {
        let ctx = TraceContext::new("a".repeat(32), "b".repeat(16))
            .map_err(crate::test_support::ctx("TraceContext::new"))?;
        let result = ctx.with_parent("bad");
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, ObserveError::InvalidSpanId { .. }));
        Ok(())
    }

    #[test]
    fn test_trace_context_serde_roundtrip() -> TestResult {
        let ctx = TraceContext::new("a".repeat(32), "b".repeat(16))
            .map_err(crate::test_support::ctx("TraceContext::new"))?;
        let json =
            serde_json::to_string(&ctx).map_err(crate::test_support::ctx("serialisieren"))?;
        let back: TraceContext =
            serde_json::from_str(&json).map_err(crate::test_support::ctx("deserialisieren"))?;
        assert_eq!(ctx, back);
        Ok(())
    }

    #[test]
    fn test_trace_context_deny_unknown_fields() {
        let json = r#"{"trace_id":"a","span_id":"b","extra":"x"}"#;
        let result: Result<TraceContext, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_trace_context_parent_span_id_omitted_when_none() -> TestResult {
        let ctx = TraceContext::new("a".repeat(32), "b".repeat(16))
            .map_err(crate::test_support::ctx("TraceContext::new"))?;
        let json =
            serde_json::to_string(&ctx).map_err(crate::test_support::ctx("serialisieren"))?;
        assert!(!json.contains("parent_span_id"));
        Ok(())
    }

    #[test]
    fn test_trace_context_parent_span_id_present_when_set() -> TestResult {
        let ctx = TraceContext::new("a".repeat(32), "b".repeat(16))
            .map_err(crate::test_support::ctx("TraceContext::new"))?
            .with_parent("c".repeat(16))
            .map_err(crate::test_support::ctx("TraceContext::with_parent"))?;
        let json =
            serde_json::to_string(&ctx).map_err(crate::test_support::ctx("serialisieren"))?;
        assert!(json.contains("parent_span_id"));
        Ok(())
    }
}
