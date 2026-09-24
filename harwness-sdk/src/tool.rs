//! Eigene Werkzeuge (`Tool`) und Kontextquellen (`ContextSource`).
//!
//! # Beschreibung
//! Beide Traits sind die stabile Form der internen Erweiterungspunkte
//! (Tool-Provider und Kontext-Provider). Sie sprechen ausschließlich JSON und
//! SDK-Typen; die Übersetzung in die internen Traits liegt in
//! `crate::adapter` und ist nicht Teil der öffentlichen Fläche.
//!
//! # Rechte
//! Ein SDK-Werkzeug läuft im Prozess des Einbettenden, nicht in der Sandbox
//! der Runtime: es hat genau die Rechte des Host-Programms. Ob das Modell es
//! ohne Rückfrage aufrufen darf, entscheidet wie bei jedem Werkzeug die
//! [`crate::ApprovalPolicy`] zusammen mit dem [`crate::ApprovalHandler`].

use std::fmt;
use std::future::Future;

use crate::BoxFuture;
use crate::ids::SessionId;

/// Aufrufkontext eines Werkzeugs.
#[derive(Debug, Clone)]
pub struct ToolContext {
    session_id: SessionId,
    call_id: String,
    cancel: Option<harw_types::cancel::CancelToken>,
}

impl ToolContext {
    /// Baut den Kontext aus dem internen Ausführungskontext.
    pub(crate) fn new(
        session_id: SessionId,
        call_id: String,
        cancel: Option<harw_types::cancel::CancelToken>,
    ) -> Self {
        Self {
            session_id,
            call_id,
            cancel,
        }
    }

    /// Sitzung, deren Modell das Werkzeug aufruft.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Kennung dieses Aufrufs.
    #[must_use]
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// `true`, sobald der Turn abgebrochen wurde; lange Werkzeuge sollten
    /// dann zügig mit einem [`ToolError`] enden.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(harw_types::cancel::CancelToken::is_cancelled)
    }
}

/// Fehler eines Werkzeugs; die Meldung sieht das Modell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    message: String,
}

impl ToolError {
    /// Baut einen Fehler mit nutzersicherer Meldung (keine Geheimnisse).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Die Meldung.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

/// Ein Werkzeug, das das Modell aufrufen kann.
///
/// # Anforderungen
/// - `name`: 1–64 Zeichen aus `[A-Za-z0-9_.-]`, eindeutig unter den
///   SDK-Werkzeugen einer [`crate::Harwness`].
/// - `parameters`: ein JSON-Schema mit `"type": "object"`.
///
/// Ein `String`-Ergebnis erreicht das Modell als Text, jedes andere
/// JSON-Ergebnis als JSON.
///
/// # Beispiel
/// ```rust
/// use harwness_sdk::{BoxFuture, Tool, ToolContext, ToolError};
/// use serde_json::{json, Value};
///
/// struct Clock;
///
/// impl Tool for Clock {
///     fn name(&self) -> &str { "host.clock" }
///     fn description(&self) -> &str { "Liefert die Uhrzeit des Hosts." }
///     fn parameters(&self) -> Value { json!({"type": "object", "properties": {}}) }
///     fn call<'a>(&'a self, _ctx: &'a ToolContext, _args: Value)
///         -> BoxFuture<'a, Result<Value, ToolError>>
///     {
///         Box::pin(async { Ok(json!("12:00")) })
///     }
/// }
/// ```
pub trait Tool: Send + Sync + 'static {
    /// Werkzeugname, wie ihn das Modell sieht.
    fn name(&self) -> &str;

    /// Beschreibung für das Modell.
    fn description(&self) -> &str;

    /// JSON-Schema der Argumente (`"type": "object"`).
    fn parameters(&self) -> serde_json::Value;

    /// Ob parallele Aufrufe innerhalb einer Modellantwort sicher sind.
    fn parallel_safe(&self) -> bool {
        false
    }

    /// Führt das Werkzeug aus.
    fn call<'a>(
        &'a self,
        ctx: &'a ToolContext,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Result<serde_json::Value, ToolError>>;
}

/// Ein [`Tool`] aus einer asynchronen Closure.
///
/// # Beispiel
/// ```rust
/// use harwness_sdk::{FnTool, ToolError};
/// use serde_json::json;
///
/// let add = FnTool::new(
///     "math.add",
///     "Addiert a und b.",
///     json!({"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
///            "required": ["a", "b"]}),
///     |args| async move {
///         let a = args["a"].as_f64().ok_or_else(|| ToolError::new("a fehlt"))?;
///         let b = args["b"].as_f64().ok_or_else(|| ToolError::new("b fehlt"))?;
///         Ok::<_, ToolError>(json!(a + b))
///     },
/// );
/// # let _ = add;
/// ```
pub struct FnTool<F> {
    name: String,
    description: String,
    parameters: serde_json::Value,
    parallel_safe: bool,
    run: F,
}

impl<F> FnTool<F> {
    /// Baut das Werkzeug.
    ///
    /// Die Schranken stehen schon hier (nicht erst am `Tool`-Impl), damit der
    /// Compiler Argument- und Ergebnistyp der Closure ableiten kann.
    pub fn new<Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        run: F,
    ) -> Self
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, ToolError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            parallel_safe: false,
            run,
        }
    }

    /// Erklärt parallele Aufrufe für sicher (Vorgabe: `false`).
    #[must_use]
    pub fn with_parallel_safe(mut self, parallel_safe: bool) -> Self {
        self.parallel_safe = parallel_safe;
        self
    }
}

impl<F> fmt::Debug for FnTool<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnTool")
            .field("name", &self.name)
            .field("parallel_safe", &self.parallel_safe)
            .finish_non_exhaustive()
    }
}

impl<F, Fut> Tool for FnTool<F>
where
    F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<serde_json::Value, ToolError>> + Send + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.parameters.clone()
    }

    fn parallel_safe(&self) -> bool {
        self.parallel_safe
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ToolContext,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Result<serde_json::Value, ToolError>> {
        Box::pin((self.run)(arguments))
    }
}

/// Ein Kontextbaustein, den eine [`ContextSource`] je Turn beisteuert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextItem {
    /// Kurzes, stabiles Etikett (z. B. `host.user`).
    pub label: String,
    /// Inhalt; wird als Daten (nicht als Anweisung) in den Kontext gelegt.
    pub content: String,
}

impl ContextItem {
    /// Baut einen Baustein.
    pub fn new(label: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            content: content.into(),
        }
    }
}

/// Steuert zu jedem Turn Kontext bei.
///
/// # Vertrauen
/// Beiträge landen in der niedrigsten Vertrauensklasse („Daten"): das Modell
/// behandelt sie als Information, nicht als Anweisung.
pub trait ContextSource: Send + Sync + 'static {
    /// Eindeutiger Namensraum dieser Quelle (nicht leer, z. B. `host.crm`).
    fn namespace(&self) -> &'static str;

    /// Liefert die Bausteine für den nächsten Turn.
    fn items<'a>(&'a self, session_id: &'a SessionId) -> BoxFuture<'a, Vec<ContextItem>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn fn_tools_run_their_closure() -> TestResult {
        let tool = FnTool::new(
            "math.double",
            "Verdoppelt n.",
            json!({"type": "object"}),
            |args: serde_json::Value| async move {
                let n = args["n"].as_i64().ok_or_else(|| ToolError::new("n fehlt"))?;
                Ok::<_, ToolError>(json!(n * 2))
            },
        )
        .with_parallel_safe(true);
        let ctx = ToolContext::new(SessionId::new("s")?, "c".into(), None);
        assert_eq!(tool.call(&ctx, json!({"n": 4})).await?, json!(8));
        let missing = tool.call(&ctx, json!({})).await;
        assert_eq!(missing, Err(ToolError::new("n fehlt")));
        assert!(tool.parallel_safe());
        assert!(!ctx.is_cancelled());
        Ok(())
    }
}
