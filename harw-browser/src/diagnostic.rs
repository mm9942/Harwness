//! # diagnostic
//!
//! ## Responsibility
//! This module owns **structured, severity-ranked failure/warning evidence**
//! ([`BrowserDiagnostic`]) surfaced alongside actions and observations,
//! together with its classification vocabulary ([`Severity`],
//! [`DiagnosticSource`], [`DiagnosticCategory`]). It does not own the
//! decision of *when* to raise a diagnostic (that is a backend concern) or
//! the artifact bytes referenced as evidence (see [`crate::artifact`]).
//!
//! ## Key types exported
//! - [`Severity`] — how serious a diagnostic is, from `Critical` to `Info`.
//! - [`DiagnosticSource`] — which subsystem raised the diagnostic (network,
//!   console, selector, driver, etc.).
//! - [`DiagnosticCategory`] — the specific kind of failure/warning being
//!   reported.
//! - [`BrowserDiagnostic`] — a single diagnostic: severity, source, category,
//!   human-readable message, attached evidence, and an optional linked
//!   effect.
//!
//! ## Concurrency
//! Single-threaded, pure data types; `Send + Sync` follows automatically from
//! the fields. No locking or shared state.
//!
//! ## Errors
//! This module defines no fallible operations itself; diagnostics are
//! non-fatal evidence attached to [`crate::action::ActionOutcome`] and
//! [`crate::observation::BrowserObservation`] rather than propagated as
//! [`crate::error::Error`].
//!
//! ## Examples
//! ```rust,no_run
//! use harw_browser::diagnostic::{BrowserDiagnostic, DiagnosticCategory, DiagnosticSource, Severity};
//!
//! let diagnostic = BrowserDiagnostic::new(
//!     Severity::Low,
//!     DiagnosticSource::Selector,
//!     DiagnosticCategory::SelectorStale,
//!     "element moved before click",
//! );
//! assert_eq!(diagnostic.severity, Severity::Low);
//! ```

// Spec: CONTRACT_harw_browser.md, section `diagnostic.rs`.

use crate::artifact::ArtifactRef;
use crate::ids::EffectId;

/// How serious a [`BrowserDiagnostic`] is.
///
/// # Description
/// Declaration order is `Critical, High, Medium, Low, Info`, so under the
/// derived `Ord`, `Critical` (discriminant 0) is "least" and `Info`
/// (discriminant 4) is "greatest". Callers wanting "most severe first" should
/// sort ascending by this derived order.
///
/// # Concurrency
/// `Copy`, plain data.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

/// Which subsystem raised a [`BrowserDiagnostic`].
///
/// # Concurrency
/// `Copy`, plain data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DiagnosticSource {
    Network,
    Console,
    Navigation,
    Selector,
    Automation,
    Driver,
    Application,
}

/// The specific kind of failure or warning a [`BrowserDiagnostic`] reports.
///
/// # Concurrency
/// `Copy`, plain data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DiagnosticCategory {
    NetworkFailure,
    UnexpectedStatus,
    SecurityConsoleEntry,
    JavaScriptException,
    NavigationAborted,
    SelectorStale,
    SelectorNotFound,
    OptimisticUiMismatch,
    UnconfirmedAction,
    SessionExpiry,
    ProcessFailure,
}

/// A single piece of structured, severity-ranked failure or warning evidence.
///
/// # Description
/// Combines a [`Severity`], a [`DiagnosticSource`], a [`DiagnosticCategory`],
/// and a human-readable `message` with optional supporting `evidence`
/// ([`ArtifactRef`]s, e.g. a screenshot) and an optional `effect_id` linking
/// the diagnostic back to the [`crate::action::ActionOutcome`] that produced
/// it.
///
/// # Concurrency
/// Plain data, `Send + Sync`, no interior mutability.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BrowserDiagnostic {
    pub severity: Severity,
    pub source: DiagnosticSource,
    pub category: DiagnosticCategory,
    pub message: String,
    pub evidence: Vec<ArtifactRef>,
    pub effect_id: Option<EffectId>,
}

impl BrowserDiagnostic {
    /// Builds a new diagnostic with no evidence and no linked effect.
    ///
    /// # Arguments
    /// - `severity` (`Severity`): how serious this diagnostic is. Owned
    ///   (`Copy`).
    /// - `source` (`DiagnosticSource`): which subsystem raised it. Owned
    ///   (`Copy`).
    /// - `category` (`DiagnosticCategory`): the specific failure/warning
    ///   kind. Owned (`Copy`).
    /// - `message` (`impl Into<String>`): a human-readable description.
    ///   Converted to an owned `String`.
    ///
    /// # Returns
    /// `BrowserDiagnostic` — with `evidence: Vec::new()` and
    /// `effect_id: None`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::diagnostic::{BrowserDiagnostic, DiagnosticCategory, DiagnosticSource, Severity};
    ///
    /// let diagnostic = BrowserDiagnostic::new(
    ///     Severity::High,
    ///     DiagnosticSource::Network,
    ///     DiagnosticCategory::NetworkFailure,
    ///     "request timed out",
    /// );
    /// assert!(diagnostic.evidence.is_empty());
    /// ```
    pub fn new(
        severity: Severity,
        source: DiagnosticSource,
        category: DiagnosticCategory,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            source,
            category,
            message: message.into(),
            evidence: Vec::new(),
            effect_id: None,
        }
    }

    /// Appends one artifact reference to `evidence`.
    ///
    /// # Description
    /// Consumes and returns `self` so calls can be chained, and can be
    /// called multiple times to attach several pieces of evidence in order.
    ///
    /// # Arguments
    /// - `evidence` (`ArtifactRef`): the artifact reference to attach.
    ///   Ownership is transferred into the diagnostic's evidence list.
    ///
    /// # Returns
    /// `BrowserDiagnostic` — `self` with `evidence` extended by one entry.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::artifact::{ArtifactKind, ArtifactRef};
    /// use harw_browser::diagnostic::{BrowserDiagnostic, DiagnosticCategory, DiagnosticSource, Severity};
    ///
    /// let diagnostic = BrowserDiagnostic::new(
    ///     Severity::Medium,
    ///     DiagnosticSource::Console,
    ///     DiagnosticCategory::JavaScriptException,
    ///     "uncaught exception",
    /// )
    /// .with_evidence(ArtifactRef::new(ArtifactKind::Text, "text/plain", 12));
    /// assert_eq!(diagnostic.evidence.len(), 1);
    /// ```
    pub fn with_evidence(mut self, evidence: ArtifactRef) -> Self {
        self.evidence.push(evidence);
        self
    }

    /// Links this diagnostic to the effect that produced it.
    ///
    /// # Description
    /// Consumes and returns `self` so calls can be chained onto
    /// [`BrowserDiagnostic::new`] and [`BrowserDiagnostic::with_evidence`].
    ///
    /// # Arguments
    /// - `effect_id` (`EffectId`): the identifier of the
    ///   [`crate::action::ActionOutcome`] this diagnostic relates to. Owned
    ///   (`Copy`).
    ///
    /// # Returns
    /// `BrowserDiagnostic` — `self` with `effect_id` set to
    /// `Some(effect_id)`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::diagnostic::{BrowserDiagnostic, DiagnosticCategory, DiagnosticSource, Severity};
    /// use harw_browser::ids::EffectId;
    ///
    /// let diagnostic = BrowserDiagnostic::new(
    ///     Severity::Info,
    ///     DiagnosticSource::Application,
    ///     DiagnosticCategory::SessionExpiry,
    ///     "session expired",
    /// )
    /// .with_effect(EffectId::new());
    /// assert!(diagnostic.effect_id.is_some());
    /// ```
    pub fn with_effect(mut self, effect_id: EffectId) -> Self {
        self.effect_id = Some(effect_id);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::ArtifactKind;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_severity_critical_less_than_info() {
        assert!(Severity::Critical < Severity::Info);
    }

    #[test]
    fn test_new_sets_fields_with_empty_evidence_and_no_effect() {
        let diagnostic = BrowserDiagnostic::new(
            Severity::High,
            DiagnosticSource::Network,
            DiagnosticCategory::NetworkFailure,
            "request timed out",
        );
        assert_eq!(diagnostic.severity, Severity::High);
        assert_eq!(diagnostic.source, DiagnosticSource::Network);
        assert_eq!(diagnostic.category, DiagnosticCategory::NetworkFailure);
        assert_eq!(diagnostic.message, "request timed out");
        assert!(diagnostic.evidence.is_empty());
        assert_eq!(diagnostic.effect_id, None);
    }

    #[test]
    fn test_with_evidence_appends_artifact() {
        let artifact = ArtifactRef::new(ArtifactKind::Text, "text/plain", 12);
        let diagnostic = BrowserDiagnostic::new(
            Severity::Medium,
            DiagnosticSource::Console,
            DiagnosticCategory::JavaScriptException,
            "uncaught exception",
        )
        .with_evidence(artifact.clone());
        assert_eq!(diagnostic.evidence.len(), 1);
        assert_eq!(diagnostic.evidence[0], artifact);
    }

    #[test]
    fn test_with_evidence_appends_multiple_in_order() {
        let first = ArtifactRef::new(ArtifactKind::Text, "text/plain", 1);
        let second = ArtifactRef::new(ArtifactKind::Screenshot, "image/png", 2);
        let diagnostic = BrowserDiagnostic::new(
            Severity::Low,
            DiagnosticSource::Selector,
            DiagnosticCategory::SelectorStale,
            "stale element",
        )
        .with_evidence(first.clone())
        .with_evidence(second.clone());
        assert_eq!(diagnostic.evidence, vec![first, second]);
    }

    #[test]
    fn test_with_effect_sets_effect_id() {
        let effect_id = EffectId::new();
        let diagnostic = BrowserDiagnostic::new(
            Severity::Info,
            DiagnosticSource::Application,
            DiagnosticCategory::SessionExpiry,
            "session expired",
        )
        .with_effect(effect_id);
        assert_eq!(diagnostic.effect_id, Some(effect_id));
    }

    #[test]
    fn test_full_builder_chain_combines_evidence_and_effect() {
        // Exercises new() -> with_evidence() (twice) -> with_effect() in a single
        // chain, matching how a real diagnostic is assembled by call sites.
        let effect_id = EffectId::new();
        let first_evidence = ArtifactRef::new(ArtifactKind::Screenshot, "image/png", 10);
        let second_evidence = ArtifactRef::new(ArtifactKind::Html, "text/html", 20);

        let diagnostic = BrowserDiagnostic::new(
            Severity::Critical,
            DiagnosticSource::Network,
            DiagnosticCategory::NetworkFailure,
            "request to /api/checkout failed with status 502",
        )
        .with_evidence(first_evidence.clone())
        .with_evidence(second_evidence.clone())
        .with_effect(effect_id);

        assert_eq!(diagnostic.severity, Severity::Critical);
        assert_eq!(diagnostic.source, DiagnosticSource::Network);
        assert_eq!(diagnostic.category, DiagnosticCategory::NetworkFailure);
        assert_eq!(
            diagnostic.message,
            "request to /api/checkout failed with status 502"
        );
        assert_eq!(diagnostic.evidence, vec![first_evidence, second_evidence]);
        assert_eq!(diagnostic.effect_id, Some(effect_id));
    }

    #[test]
    fn test_browser_diagnostic_serde_json_round_trip() -> TestResult {
        let diagnostic = BrowserDiagnostic::new(
            Severity::Medium,
            DiagnosticSource::Driver,
            DiagnosticCategory::ProcessFailure,
            "driver process exited unexpectedly",
        )
        .with_evidence(ArtifactRef::new(ArtifactKind::Text, "text/plain", 8))
        .with_effect(EffectId::new());

        let json = serde_json::to_string(&diagnostic).map_err(ctx("diagnostic serializes"))?;
        let decoded: BrowserDiagnostic =
            serde_json::from_str(&json).map_err(ctx("diagnostic deserializes"))?;
        assert_eq!(decoded, diagnostic);
        Ok(())
    }
}
