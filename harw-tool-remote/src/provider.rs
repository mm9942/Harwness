//! [`RemoteToolProvider`]: der Werkzeugsatz eines an ein Gateway
//! angebundenen Agenten, gebaut aus `tool.list`.

use std::sync::Arc;

use harw_extension_api::contributors::ToolProvider;
use harw_protocol::items::ToolPlacement;
use harw_protocol::session_port::ToolPort;
use harw_protocol::session_wire::{ToolDescriptor, ToolListParams};
use harw_tools::{FunctionToolSpec, JsonSchema, ToolExecutor, ToolName, ToolSpec};
use harw_types::SessionId;

use crate::error::RemoteToolError;
use crate::executor::RemoteToolExecutor;

/// Ein vom Gateway angebotenes Werkzeug: Spezifikation für das Modell plus
/// der Proxy, der es aufruft.
struct RemoteTool {
    spec: ToolSpec,
    parallel_safe: bool,
    executor: Arc<RemoteToolExecutor>,
}

/// Bietet exakt die Werkzeuge an, die das Gateway dem Agenten-Principal
/// gewährt (R18 D-A, §10 P2).
///
/// # Beschreibung
/// Entsteht aus den Deskriptoren von `tool.list`; jedes Werkzeug wird von
/// einem [`RemoteToolExecutor`] bedient. Der Satz ist nach dem Bau fest: ein
/// späteres `gateway.tools.narrow` greift im Gateway beim nächsten Aufruf
/// (`TOOL_NOT_GRANTED`), ohne dass der Proxy etwas lokal ausführt.
///
/// # Nebenläufigkeit
/// Unveränderlich nach dem Bau; `Send + Sync`.
pub struct RemoteToolProvider {
    session: SessionId,
    tools: Vec<RemoteTool>,
}

impl RemoteToolProvider {
    /// Fragt `tool.list` für `session` ab und baut daraus den Werkzeugsatz.
    ///
    /// # Fehler
    /// - [`RemoteToolError::List`], wenn das Gateway `tool.list` ablehnt
    ///   (fehlende Rechte, fremde Sitzung, Verbindungsfehler).
    /// - [`RemoteToolError::InvalidDescriptor`] wie bei
    ///   [`Self::from_descriptors`].
    pub async fn connect(
        port: Arc<dyn ToolPort>,
        session: SessionId,
    ) -> Result<Self, RemoteToolError> {
        let listed = port
            .list_tools(ToolListParams {
                session_id: session.clone(),
            })
            .await
            .map_err(RemoteToolError::List)?;
        Self::from_descriptors(port, session, listed.tools)
    }

    /// Baut den Werkzeugsatz aus bereits geholten Deskriptoren.
    ///
    /// # Beschreibung
    /// Fail closed: ein einziger unbrauchbarer Deskriptor verwirft den ganzen
    /// Satz, statt still ein Werkzeug auszulassen.
    ///
    /// # Fehler
    /// [`RemoteToolError::InvalidDescriptor`], wenn ein Name leer oder doppelt
    /// ist, das Eingabeschema kein JSON-Schema-Objekt ist oder die
    /// Platzierung nicht `gateway` lautet (ein vom Gateway bedientes Werkzeug
    /// läuft immer dort, Vertrag §2.2).
    pub fn from_descriptors(
        port: Arc<dyn ToolPort>,
        session: SessionId,
        descriptors: Vec<ToolDescriptor>,
    ) -> Result<Self, RemoteToolError> {
        let mut tools: Vec<RemoteTool> = Vec::with_capacity(descriptors.len());
        for descriptor in descriptors {
            let invalid = |reason: &str| RemoteToolError::InvalidDescriptor {
                tool: descriptor.name.clone(),
                reason: reason.to_owned(),
            };
            if descriptor.name.trim().is_empty() {
                return Err(invalid("the tool name is empty"));
            }
            if tools.iter().any(|tool| tool.spec.name() == descriptor.name) {
                return Err(invalid("the tool name is listed twice"));
            }
            let ToolPlacement::Gateway { node } = descriptor.placement.clone() else {
                return Err(invalid(
                    "a gateway-served tool must have placement 'gateway'",
                ));
            };
            if !descriptor.input_schema.is_object() {
                return Err(invalid("the input schema is not a JSON object"));
            }
            let parameters: JsonSchema = serde_json::from_value(descriptor.input_schema.clone())
                .map_err(|error| RemoteToolError::InvalidDescriptor {
                    tool: descriptor.name.clone(),
                    reason: format!("the input schema does not decode: {error}"),
                })?;
            let executor = Arc::new(RemoteToolExecutor::new(
                Arc::clone(&port),
                session.clone(),
                descriptor.name.clone(),
                node,
            ));
            tools.push(RemoteTool {
                spec: ToolSpec::Function(FunctionToolSpec {
                    name: ToolName::new(descriptor.name),
                    description: descriptor.description,
                    parameters,
                    strict: false,
                }),
                parallel_safe: descriptor.parallel_safe,
                executor,
            });
        }
        Ok(Self { session, tools })
    }

    /// Die Gateway-Sitzung, an die alle Aufrufe gehen.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session
    }

    /// Die angebotenen Werkzeugnamen in der Reihenfolge von `tool.list`.
    #[must_use]
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|tool| tool.spec.name()).collect()
    }
}

impl ToolProvider for RemoteToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|tool| tool.spec.clone()).collect()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        self.tools
            .iter()
            .find(|tool| tool.spec.name() == name.as_str())
            .map(|tool| Arc::clone(&tool.executor) as Arc<dyn ToolExecutor>)
    }

    fn parallel_safe(&self, name: &ToolName) -> bool {
        self.tools
            .iter()
            .any(|tool| tool.spec.name() == name.as_str() && tool.parallel_safe)
    }
}
