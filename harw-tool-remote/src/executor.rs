//! [`RemoteToolExecutor`]: ein Werkzeug, dessen Aufrufe im Gateway laufen.

use std::sync::Arc;

use harw_protocol::items::{ToolCallResult, ToolPlacement};
use harw_protocol::session_port::{PortError, ToolPort, ToolRefusal};
use harw_protocol::session_wire::{ToolCallParams, ToolCallResultFrame, ToolCancelParams};
use harw_tools::executor::{
    ExecutionPlacement, PlacedToolExecutorFuture, PlacedToolOutput, ToolExecutionContext,
    ToolExecutor, ToolExecutorFuture,
};
use harw_tools::{ToolCall, ToolOutput, ToolsError};
use harw_types::SessionId;

/// Leitet die Aufrufe **eines** Werkzeugs als `tool.call` an das Gateway.
///
/// # Beschreibung
/// Die Sitzung ist die Gateway-Sitzung, an die der Agenten-Principal gebunden
/// ist (nicht die lokale Sitzungs-Id des Turns); `turn_id` und `call_id`
/// kommen aus Kontext und Aufruf. Identität trägt kein Feld: das Gateway
/// liest Principal, Rolle und Grant aus der Verbindung.
///
/// # Nebenläufigkeit
/// Zustandslos bis auf geteilte `Arc`s; beliebig viele Aufrufe parallel.
pub struct RemoteToolExecutor {
    port: Arc<dyn ToolPort>,
    session: SessionId,
    tool: String,
    node: Option<String>,
}

impl RemoteToolExecutor {
    /// Baut den Proxy für `tool` in der Gateway-Sitzung `session`.
    ///
    /// # Argumente
    /// - `port`: der Werkzeug-Port der Gateway-Verbindung.
    /// - `session`: die an den Principal gebundene Gateway-Sitzung.
    /// - `tool`: exakter Werkzeugname aus `tool.list`.
    /// - `node`: Gateway-Knoten aus der Deskriptor-Platzierung, falls bekannt.
    #[must_use]
    pub fn new(
        port: Arc<dyn ToolPort>,
        session: SessionId,
        tool: impl Into<String>,
        node: Option<String>,
    ) -> Self {
        Self {
            port,
            session,
            tool: tool.into(),
            node,
        }
    }

    /// Der Werkzeugname, den dieser Proxy weiterleitet.
    #[must_use]
    pub fn tool_name(&self) -> &str {
        &self.tool
    }

    /// Ein vollständiger Aufruf: `tool.call`, Abbruch über den `CancelToken`
    /// des Kontexts, Abbildung der Antwort.
    async fn forward(&self, context: &ToolExecutionContext, call: &ToolCall) -> PlacedToolOutput {
        // Ein Proxy ist an genau ein Werkzeug gebunden; ein fremder Name wäre
        // ein Verdrahtungsfehler, nie ein Grund, etwas anderes aufzurufen.
        if call.name.as_str() != self.tool {
            return unplaced(ToolOutput::error(format!(
                "remote tool proxy for '{}' refused a call for '{}'; nothing was run",
                self.tool, call.name
            )));
        }
        // Schon abgebrochen: gar nicht erst senden (dann gibt es auch nichts
        // abzubrechen).
        if context
            .cancel()
            .is_some_and(harw_types::cancel::CancelToken::is_cancelled)
        {
            return PlacedToolOutput {
                output: Err(ToolsError::Cancelled),
                placement: None,
            };
        }
        let params = ToolCallParams {
            session_id: self.session.clone(),
            turn_id: context.turn_id().clone(),
            call_id: call.id.clone(),
            tool_name: self.tool.clone(),
            arguments: call.arguments.clone(),
            parent_call_id: None,
        };
        let mut guard = CancelOnDrop::arm(
            Arc::clone(&self.port),
            ToolCancelParams {
                session_id: self.session.clone(),
                call_id: call.id.clone(),
            },
        );
        let answer = match context.cancel() {
            Some(token) => {
                tokio::select! {
                    biased;
                    () = token.cancelled() => None,
                    answer = self.port.call_tool(params) => Some(answer),
                }
            }
            None => Some(self.port.call_tool(params).await),
        };
        let Some(answer) = answer else {
            // Der Ausführer hat den Abbruch selbst gesehen: `tool.cancel`
            // genau hier, nicht zusätzlich im `Drop` des Wächters.
            if let Some(cancel) = guard.disarm() {
                send_cancel(self.port.as_ref(), cancel).await;
            }
            return PlacedToolOutput {
                output: Err(ToolsError::Cancelled),
                placement: None,
            };
        };
        // Der Aufruf ist beendet (Ergebnis oder Ablehnung): nichts abzubrechen.
        let _finished = guard.disarm();
        match answer {
            Ok(frame) => frame_output(call, frame),
            Err(error) => unplaced(ToolOutput::error(refusal_message(&self.tool, &error))),
        }
    }
}

impl ToolExecutor for RemoteToolExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { self.forward(context, call).await.output })
    }

    fn placement(&self) -> Option<ExecutionPlacement> {
        Some(ExecutionPlacement::Gateway {
            node: self.node.clone(),
        })
    }

    fn execute_placed<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> PlacedToolExecutorFuture<'a> {
        Box::pin(self.forward(context, call))
    }
}

/// Der Text, mit dem der Proxy eine Ablehnung an das Modell meldet.
///
/// # Beschreibung
/// Benennt Werkzeug, Ablehnungsart (stabiler Name plus Wire-Code) und das
/// Detail des Gateways, und sagt ausdrücklich, dass nichts ausgeführt wurde
/// und es keinen lokalen Rückfall gibt (Vertrag R18 §4.2).
#[must_use]
pub fn refusal_message(tool: &str, error: &PortError) -> String {
    let kind = match error {
        PortError::ToolRefused { refusal, .. } => match refusal {
            ToolRefusal::UnknownTool => "tool_unknown",
            ToolRefusal::NotGranted => "tool_not_granted",
            ToolRefusal::SandboxUnavailable => "tool_sandbox_unavailable",
            ToolRefusal::Draining => "host_draining",
            ToolRefusal::DuplicateCall => "tool_call_duplicate",
        },
        PortError::Denied(_) => "denied",
        PortError::NotFound => "not_found",
        PortError::Revoked => "revoked",
        PortError::Protocol(_) => "protocol",
        PortError::Transport(_) => "transport",
    };
    format!(
        "gateway refused tool '{tool}' ({kind}, code {}): {error}. The call did not run; \
         there is no local fallback.",
        error.code()
    )
}

/// Bildet eine Ergebnis-Frame auf Ausgabe und Platzierung ab.
fn frame_output(call: &ToolCall, frame: ToolCallResultFrame) -> PlacedToolOutput {
    // Eine Frame für einen anderen Aufruf ist eine Protokollverletzung des
    // Gateways; ihr Ergebnis gehört nicht zu diesem Aufruf.
    if frame.call_id != call.id {
        return unplaced(ToolOutput::error(format!(
            "gateway answered tool '{}' with a result for another call; the result was discarded",
            call.name
        )));
    }
    let output = match frame.result {
        ToolCallResult::Success {
            value: serde_json::Value::String(text),
        } => ToolOutput::text(text),
        ToolCallResult::Success { value } => ToolOutput::json(value),
        ToolCallResult::Error { message } => ToolOutput::error(message),
    };
    PlacedToolOutput {
        output: Ok(output),
        placement: execution_placement(frame.placement),
    }
}

/// Die Wire-Platzierung einer Frame als Platzierung dieses Crates;
/// `Unknown` bleibt unbekannt.
fn execution_placement(placement: ToolPlacement) -> Option<ExecutionPlacement> {
    match placement {
        ToolPlacement::Host => Some(ExecutionPlacement::Host),
        ToolPlacement::Sandbox => Some(ExecutionPlacement::Sandbox),
        ToolPlacement::Gateway { node } => Some(ExecutionPlacement::Gateway { node }),
        ToolPlacement::Unknown => None,
    }
}

/// Eine Ausgabe ohne Platzierung: der Aufruf lief nirgends.
fn unplaced(output: ToolOutput) -> PlacedToolOutput {
    PlacedToolOutput {
        output: Ok(output),
        placement: None,
    }
}

/// Sendet `tool.cancel`; ein Fehler wird nur protokolliert (das Gateway
/// bricht bei Verbindungsverlust ohnehin ab, `tool.cancel` ist idempotent).
async fn send_cancel(port: &dyn ToolPort, params: ToolCancelParams) {
    let call_id = params.call_id.clone();
    if let Err(error) = port.cancel_tool(params).await {
        tracing::warn!(call_id = %call_id, %error, "tool_remote.cancel_failed");
    }
}

/// Sendet `tool.cancel`, wenn ein laufender Aufruf verworfen wird.
///
/// # Beschreibung
/// Der Turn-Loop racet jeden Aufruf gegen seinen `CancelToken` und verwirft
/// bei einem Treffer das Future des Ausführers, bevor dieser den Abbruch
/// selbst sehen kann. Solange der Wächter scharf ist, schickt sein `Drop`
/// deshalb `tool.cancel` über die aktuelle Tokio-Runtime. `disarm` macht ihn
/// unscharf, sobald die Antwort da ist oder der Ausführer selbst abbricht —
/// so geht `tool.cancel` höchstens einmal hinaus.
struct CancelOnDrop {
    port: Arc<dyn ToolPort>,
    params: Option<ToolCancelParams>,
}

impl CancelOnDrop {
    fn arm(port: Arc<dyn ToolPort>, params: ToolCancelParams) -> Self {
        Self {
            port,
            params: Some(params),
        }
    }

    fn disarm(&mut self) -> Option<ToolCancelParams> {
        self.params.take()
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let Some(params) = self.params.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let port = Arc::clone(&self.port);
                drop(handle.spawn(async move {
                    send_cancel(port.as_ref(), params).await;
                }));
            }
            Err(error) => {
                tracing::warn!(
                    call_id = %params.call_id,
                    %error,
                    "tool_remote.cancel_without_runtime"
                );
            }
        }
    }
}
