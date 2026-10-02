//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

use crate::error::RuntimeError;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
pub(crate) enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form.
    Unexpected(String),
    /// Ein Fehler mit Kontext (ersetzt `expect("…")`).
    Context {
        context: &'static str,
        source: String,
    },
    /// Fehler aus diesem Crate ([`RuntimeError`]).
    Runtime(RuntimeError),
    /// E/A-Fehler (`std::io::Error`).
    Io(std::io::Error),
}

/// Ergebnistyp für Tests dieses Crates.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "test: erwarteter Wert fehlt: {what}"),
            TestError::Unexpected(detail) => write!(f, "test: unerwartetes Ergebnis: {detail}"),
            TestError::Context { context, source } => write!(f, "test: {context}: {source}"),
            TestError::Runtime(err) => write!(f, "test: Runtime-Fehler: {err}"),
            TestError::Io(err) => write!(f, "test: E/A-Fehler: {err}"),
        }
    }
}

impl std::fmt::Debug for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TestError::Runtime(err) => Some(err),
            TestError::Io(err) => Some(err),
            TestError::Missing(_) | TestError::Unexpected(_) | TestError::Context { .. } => None,
        }
    }
}

impl From<RuntimeError> for TestError {
    fn from(err: RuntimeError) -> Self {
        TestError::Runtime(err)
    }
}

impl From<std::io::Error> for TestError {
    fn from(err: std::io::Error) -> Self {
        TestError::Io(err)
    }
}

/// Baut aus einem Kontext-String eine Funktion, die einen Fremdfehler in
/// [`TestError::Context`] übersetzt (ersetzt `.expect("…")` auf `Result`).
///
/// Deckt Fremdfehlertypen ab, für die keine eigene `TestError`-Variante
/// existiert (z. B. Fehler anderer `harw-*`-Crates in diesem Workspace).
pub(crate) fn ctx<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

/// Gateway-Port ohne Gateway (R18 D-B): jede Methode meldet
/// `PortError::Transport`. Für Tests, die nur prüfen, **wo** der Port landet
/// (Service-Maps, Registrierung der `gateway.*`-Operationen).
pub(crate) struct NullGatewayPort;

fn null_port<'a, T: Send + 'a>() -> harw_protocol::PortFuture<'a, T> {
    Box::pin(std::future::ready(Err(
        harw_protocol::PortError::Transport("null gateway port".to_owned()),
    )))
}

impl harw_protocol::GatewayPort for NullGatewayPort {
    fn status(&self) -> harw_protocol::PortFuture<'_, harw_protocol::session_wire::GatewayStatus> {
        null_port()
    }

    fn connections(
        &self,
    ) -> harw_protocol::PortFuture<'_, harw_protocol::session_wire::GatewayConnectionsResult> {
        null_port()
    }

    fn sessions(
        &self,
    ) -> harw_protocol::PortFuture<'_, Vec<harw_protocol::session_wire::SessionSummary>> {
        null_port()
    }

    fn listeners(
        &self,
    ) -> harw_protocol::PortFuture<'_, harw_protocol::session_wire::GatewayListenersResult> {
        null_port()
    }

    fn tools(
        &self,
    ) -> harw_protocol::PortFuture<'_, harw_protocol::session_wire::GatewayToolsResult> {
        null_port()
    }

    fn revoke_connection(
        &self,
        _params: harw_protocol::session_wire::GatewayRevokeParams,
    ) -> harw_protocol::PortFuture<'_, harw_protocol::session_wire::GatewayRevokeResult> {
        null_port()
    }

    fn drain(
        &self,
        _params: harw_protocol::session_wire::GatewayDrainParams,
    ) -> harw_protocol::PortFuture<'_, harw_protocol::session_wire::GatewayStatus> {
        null_port()
    }

    fn set_listener(
        &self,
        _params: harw_protocol::session_wire::GatewayListenerSetParams,
    ) -> harw_protocol::PortFuture<'_, harw_protocol::session_wire::GatewayListenerInfo> {
        null_port()
    }

    fn grant_tools(
        &self,
        _params: harw_protocol::session_wire::GatewayToolRightsParams,
    ) -> harw_protocol::PortFuture<'_, harw_protocol::session_wire::GatewayToolRights> {
        null_port()
    }

    fn narrow_tools(
        &self,
        _params: harw_protocol::session_wire::GatewayToolRightsParams,
    ) -> harw_protocol::PortFuture<'_, harw_protocol::session_wire::GatewayToolRights> {
        null_port()
    }
}
