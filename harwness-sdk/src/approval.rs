//! Freigaben: wer entscheidet, ob ein zurückgehaltener Werkzeugaufruf läuft.
//!
//! # Beschreibung
//! Die Runtime hält Werkzeugaufrufe, die ihre Freigabepolitik
//! ([`ApprovalPolicy`]) nicht ohne Rückfrage durchlässt, an und pausiert den
//! Turn. [`crate::Session::send`] fragt dann den konfigurierten
//! [`ApprovalHandler`] und setzt den Turn mit dessen [`Decision`] fort. Ohne
//! eigenen Handler gilt [`AutoDeny`]: eine Frage, die niemand beantworten
//! kann, ist keine Freigabe.
//!
//! Antwortet der Handler nicht innerhalb des konfigurierten Freigabe-Timeouts,
//! wird der Aufruf abgelehnt.

use std::future::Future;
use std::sync::Arc;

use crate::BoxFuture;
use crate::ids::SessionId;

/// Eine offene Freigabeanfrage.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ApprovalRequest {
    /// Sitzung, deren Turn pausiert.
    pub session_id: SessionId,
    /// Kennung der Anfrage (stabil für diese Pause).
    pub request_id: String,
    /// Kennung des zurückgehaltenen Werkzeugaufrufs.
    pub call_id: String,
    /// Werkzeugname (z. B. `fs.write`, `shell.exec`).
    pub tool: String,
    /// Argumente des Aufrufs als JSON.
    pub arguments: serde_json::Value,
}

/// Die Entscheidung über eine [`ApprovalRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision {
    /// Der Aufruf darf laufen.
    Approve,
    /// Der Aufruf wird abgelehnt; `reason` sieht das Modell.
    Deny {
        /// Begründung für das Modell.
        reason: String,
    },
}

impl Decision {
    /// Kurzform für [`Decision::Deny`].
    #[must_use]
    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
        }
    }
}

/// Beantwortet Freigabeanfragen.
///
/// # Beschreibung
/// Objektsicher (`Arc<dyn ApprovalHandler>`), deshalb liefert `decide` ein
/// geboxtes Future statt `async fn`. Für Closures siehe [`approval_fn`].
///
/// # Nebenläufigkeit
/// `Send + Sync`; eine Sitzung ruft `decide` nie parallel auf, mehrere
/// Sitzungen derselben [`crate::Harwness`] aber schon.
///
/// # Beispiel
/// ```rust
/// use harwness_sdk::{ApprovalHandler, ApprovalRequest, BoxFuture, Decision};
///
/// struct ReadOnly;
///
/// impl ApprovalHandler for ReadOnly {
///     fn decide<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Decision> {
///         let allowed = request.tool.starts_with("fs.read");
///         Box::pin(async move {
///             if allowed { Decision::Approve } else { Decision::deny("read-only host") }
///         })
///     }
/// }
/// ```
pub trait ApprovalHandler: Send + Sync + 'static {
    /// Entscheidet über eine Anfrage.
    fn decide<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Decision>;
}

/// Lehnt jede Anfrage ab. Vorgabe ohne eigenen Handler.
#[derive(Debug, Clone, Copy, Default)]
pub struct AutoDeny;

/// Begründung, die [`AutoDeny`] dem Modell mitgibt.
pub const AUTO_DENY_REASON: &str =
    "approval denied: the embedding host has no approval handler for this call";

impl ApprovalHandler for AutoDeny {
    fn decide<'a>(&'a self, _request: &'a ApprovalRequest) -> BoxFuture<'a, Decision> {
        Box::pin(async { Decision::deny(AUTO_DENY_REASON) })
    }
}

/// Ein [`ApprovalHandler`] aus einer Closure; siehe [`approval_fn`].
pub struct FnApproval<F> {
    decide: F,
}

impl<F> std::fmt::Debug for FnApproval<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FnApproval").finish_non_exhaustive()
    }
}

impl<F, Fut> ApprovalHandler for FnApproval<F>
where
    F: Fn(ApprovalRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Decision> + Send + 'static,
{
    fn decide<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Decision> {
        Box::pin((self.decide)(request.clone()))
    }
}

/// Baut einen [`ApprovalHandler`] aus einer asynchronen Closure.
///
/// # Beispiel
/// ```rust
/// use harwness_sdk::{approval_fn, Decision};
///
/// let handler = approval_fn(|request| async move {
///     if request.tool == "fs.write" { Decision::Approve } else { Decision::deny("nur Schreiben") }
/// });
/// # let _ = handler;
/// ```
pub fn approval_fn<F, Fut>(decide: F) -> FnApproval<F>
where
    F: Fn(ApprovalRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Decision> + Send + 'static,
{
    FnApproval { decide }
}

/// Wie streng die Runtime vor Werkzeugaufrufen nachfragt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ApprovalPolicy {
    /// Jeder Werkzeugaufruf wird dem [`ApprovalHandler`] vorgelegt.
    AlwaysAsk,
    /// Lesende Werkzeuge laufen durch; Veränderndes und Ausführendes fragt
    /// nach. Vorgabe.
    #[default]
    Delegated,
    /// Kein Aufruf fragt nach. Nur für vollständig vertrauenswürdige Hosts.
    FullAccess,
}

impl ApprovalPolicy {
    /// Die interne Stufe.
    pub(crate) fn to_core(self) -> harw_extension_api::ApprovalMode {
        match self {
            Self::AlwaysAsk => harw_extension_api::ApprovalMode::AlwaysAsk,
            Self::Delegated => harw_extension_api::ApprovalMode::Delegated,
            Self::FullAccess => harw_extension_api::ApprovalMode::FullAccess,
        }
    }
}

/// Die Vorgabe-Instanz als Trait-Objekt.
pub(crate) fn default_handler() -> Arc<dyn ApprovalHandler> {
    Arc::new(AutoDeny)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn request(tool: &str) -> Result<ApprovalRequest, crate::SdkError> {
        Ok(ApprovalRequest {
            session_id: SessionId::new("s")?,
            request_id: "r".into(),
            call_id: "c".into(),
            tool: tool.into(),
            arguments: serde_json::Value::Null,
        })
    }

    #[tokio::test]
    async fn auto_deny_denies_everything() -> TestResult {
        let decision = AutoDeny.decide(&request("fs.read")?).await;
        assert_eq!(decision, Decision::deny(AUTO_DENY_REASON));
        Ok(())
    }

    #[tokio::test]
    async fn closures_become_handlers() -> TestResult {
        let handler = approval_fn(|request: ApprovalRequest| async move {
            if request.tool == "fs.write" {
                Decision::Approve
            } else {
                Decision::deny("no")
            }
        });
        let shared: Arc<dyn ApprovalHandler> = Arc::new(handler);
        assert_eq!(
            shared.decide(&request("fs.write")?).await,
            Decision::Approve
        );
        assert_eq!(
            shared.decide(&request("shell.exec")?).await,
            Decision::deny("no")
        );
        Ok(())
    }

    #[test]
    fn policies_map_to_the_runtime_modes() {
        use harw_extension_api::ApprovalMode;
        assert_eq!(ApprovalPolicy::default().to_core(), ApprovalMode::Delegated);
        assert_eq!(ApprovalPolicy::AlwaysAsk.to_core(), ApprovalMode::AlwaysAsk);
        assert_eq!(
            ApprovalPolicy::FullAccess.to_core(),
            ApprovalMode::FullAccess
        );
    }
}
