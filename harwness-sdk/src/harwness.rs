//! `Harwness`: der geprüfte Einstieg, aus dem Sitzungen entstehen.
//!
//! # Beschreibung
//! Eine [`Harwness`] hält nur geprüfte Angaben und den geteilten
//! Verlaufsspeicher. Jede Sitzung montiert ihre **eigene** Runtime: die
//! Montage vergibt ihre Wurzel-Registry genau einmal, deshalb ist „eine
//! Montage je Sitzung" die einzige Form, in der mehrere Sitzungen und das
//! Fortsetzen ([`Harwness::resume`]) nebeneinander funktionieren.
//!
//! # Einstieg und Rechte
//! Die Montage läuft mit dem interaktiven Einstiegsprofil der Runtime
//! (Lesen/Schreiben/Ausführen im Projekt, eingebaute Kind-Rollen,
//! Rückfragen werden beantwortet statt verworfen). Beantwortet werden sie von
//! [`crate::ApprovalHandler`] des Einbettenden; ohne eigenen Handler lehnt
//! [`crate::AutoDeny`] jede Rückfrage ab.
//!
//! # Nebenläufigkeit
//! `Clone` ist billig (`Arc`), `Send + Sync`. Sitzungen sind voneinander
//! unabhängig und dürfen parallel laufen.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_core::StateStore;
use harw_protocol::{SessionEvent, TurnEvent};
use harw_runtime::{
    EntryKind, ModelSource, RootSession, RuntimeAssembly, RuntimeSpec, RuntimeStores,
};
use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};

use crate::adapter::{SdkContributor, SdkToolProvider};
use crate::approval::{ApprovalHandler, ApprovalPolicy};
use crate::builder::{HarwnessBuilder, Mode, ReasoningEffort};
use crate::error::SdkError;
use crate::ids::SessionId;
use crate::session::Session;

/// Quelle des Wurzel-Modells.
#[derive(Clone)]
pub(crate) enum ModelChoice {
    /// Der konfigurierte HTTP-Provider.
    Configured,
    /// Der eingebaute Offline-Echo.
    Echo(String),
    /// Ein vom Einbettenden gebauter Provider (instabil).
    #[cfg(feature = "unstable-internals")]
    Override(Arc<dyn harw_core::ModelProvider>),
}

impl std::fmt::Debug for ModelChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configured => f.write_str("Configured"),
            Self::Echo(reply) => f
                .debug_struct("Echo")
                .field("reply_len", &reply.len())
                .finish(),
            #[cfg(feature = "unstable-internals")]
            Self::Override(_) => f.write_str("Override(<dyn ModelProvider>)"),
        }
    }
}

impl ModelChoice {
    fn source(&self) -> ModelSource {
        match self {
            Self::Configured => ModelSource::Configured,
            Self::Echo(reply) => ModelSource::Echo(reply.clone()),
            #[cfg(feature = "unstable-internals")]
            Self::Override(provider) => ModelSource::Override(Arc::clone(provider)),
        }
    }
}

/// Die Angaben, aus denen die Eingangsbeschreibung der Runtime entsteht.
#[derive(Debug, Clone)]
pub(crate) struct SpecInputs {
    pub(crate) home: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) principal_id: String,
    pub(crate) mode: Option<Mode>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) agent: Option<String>,
    /// Ein eingebettetes Agenten-Artefakt (#22 Welle 3A,
    /// [`HarwnessBuilder::embedded`]). `Some` wählt
    /// [`EntryKind::CompiledAgent`] statt [`EntryKind::Tui`] und reicht das
    /// Artefakt in die [`RuntimeSpec`] durch.
    pub(crate) embedded: Option<Arc<harw_runtime::EmbeddedAgent>>,
    /// Ein verdrahtetes [`harw_core::child_backend::ChildBackend`] (#22
    /// Welle 3C, [`HarwnessBuilder::child_backend`]). `Some` reicht es
    /// unverändert an [`harw_runtime::RuntimeSpec::child_backend`] durch;
    /// `None` lässt jeden Kind-Lauf in-process.
    pub(crate) child_backend: Option<harw_runtime::ChildBackendHandle>,
}

impl SpecInputs {
    /// Die Eingangsbeschreibung eines Laufs.
    ///
    /// # Beschreibung
    /// Ohne [`Self::embedded`] ist [`EntryKind::Tui`] der einzige Einstieg,
    /// dessen Rückfragen jemand beantwortet
    /// ([`harw_runtime::AskResolution::Interactive`]) — hier der
    /// [`ApprovalHandler`] des Einbettenden. Der Principal ist ein lokaler
    /// Mensch an der Terminal-Fläche, damit die Runtime ihm einen
    /// Freigabe-Akteur zuordnet. Mit [`Self::embedded`] ist der Einstieg
    /// [`EntryKind::CompiledAgent`] (#22 Welle 3A): dessen Zeile entsteht aus
    /// den Manifest-Rechten des Artefakts
    /// (`harw_runtime::spec::EntryProfile::for_embedded`), nicht aus dieser
    /// Tabelle — der Principal bleibt trotzdem ein lokaler Mensch, denn ein
    /// eingebetteter Lauf startet wie die TUI direkt am Terminal.
    pub(crate) fn spec(&self) -> RuntimeSpec {
        let entry = if self.embedded.is_some() {
            EntryKind::CompiledAgent
        } else {
            EntryKind::Tui
        };
        RuntimeSpec {
            entry,
            home: self.home.clone(),
            cwd: self.cwd.clone(),
            principal: Principal::trusted_ingress(
                PrincipalKind::Human,
                self.principal_id.clone(),
                IngressSurface::Tui,
                PermissionTier::Operator,
            ),
            mode_override: self.mode.map(Mode::to_core),
            active_agent: self.agent.clone(),
            reasoning_effort: self.reasoning_effort.map(ReasoningEffort::to_core),
            // Freigabe und Modell setzt das SDK über seine eigenen Wege
            // (ApprovalPolicy bzw. Builder-Overrides), nicht über die Spec.
            approval_override: None,
            model_override: None,
            embedded: self.embedded.clone(),
            child_backend: self.child_backend.clone(),
        }
    }
}

/// Geteilter Zustand einer [`Harwness`].
pub(crate) struct Inner {
    pub(crate) spec: SpecInputs,
    pub(crate) model: ModelChoice,
    pub(crate) model_id: Option<String>,
    pub(crate) provider_id: Option<String>,
    pub(crate) approval_policy: Option<ApprovalPolicy>,
    pub(crate) approvals: Arc<dyn ApprovalHandler>,
    pub(crate) tools: Option<Arc<SdkToolProvider>>,
    pub(crate) tool_names: Vec<String>,
    pub(crate) contexts: Vec<Arc<dyn harw_extension_api::ContextProvider>>,
    pub(crate) state_store: Arc<dyn StateStore>,
    pub(crate) media: Arc<crate::media::MediaHandle>,
    #[cfg(feature = "unstable-internals")]
    pub(crate) raw_tools: Vec<Arc<dyn harw_extension_api::ToolProvider>>,
    #[cfg(feature = "unstable-internals")]
    pub(crate) secret_resolver: Option<Arc<dyn harw_provider_http::SecretResolver + Send + Sync>>,
}

/// Eine eingebettete Harwness-Instanz.
///
/// # Beispiel
/// ```rust,no_run
/// # async fn demo() -> Result<(), harwness_sdk::SdkError> {
/// let harwness = harwness_sdk::Harwness::builder().build()?;
/// let mut session = harwness.session()?;
/// let report = session.send("Hallo!").await?;
/// println!("{}", report.text.unwrap_or_default());
///
/// // Später, auch in einem anderen Prozess:
/// let id = session.id().clone();
/// drop(session);
/// let mut again = harwness.resume(&id).await?;
/// again.send("Weiter.").await?;
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct Harwness {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Harwness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Harwness")
            .field("home", &self.inner.spec.home)
            .field("cwd", &self.inner.spec.cwd)
            .field("model", &self.inner.model)
            .field("tools", &self.inner.tool_names)
            .finish_non_exhaustive()
    }
}

impl Harwness {
    /// Beginnt eine Konfiguration.
    #[must_use]
    pub fn builder() -> HarwnessBuilder {
        HarwnessBuilder::default()
    }

    pub(crate) fn from_inner(inner: Inner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Der aufgelöste Root-Space.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.inner.spec.home
    }

    /// Das Arbeitsverzeichnis der Sitzungen.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.inner.spec.cwd
    }

    /// Die Namen der SDK-Werkzeuge in Registrierungsreihenfolge.
    #[must_use]
    pub fn tool_names(&self) -> &[String] {
        &self.inner.tool_names
    }

    /// Öffnet eine neue Sitzung mit frischer Kennung.
    ///
    /// # Fehler
    /// [`SdkError::Config`], [`SdkError::Provider`], [`SdkError::Setup`] oder
    /// [`SdkError::Session`], wenn die Runtime nicht montiert werden kann.
    pub fn session(&self) -> Result<Session, SdkError> {
        self.open(None)
    }

    /// Setzt eine gespeicherte Sitzung fort.
    ///
    /// # Fehler
    /// - [`SdkError::SessionNotFound`], wenn unter `id` kein Verlauf liegt.
    /// - [`SdkError::Session`], wenn der Verlauf nicht lesbar ist.
    /// - alle Fehler von [`Self::session`].
    pub async fn resume(&self, id: &SessionId) -> Result<Session, SdkError> {
        let mut session = self.open(Some(id.to_core()?))?;
        session.hydrate().await?;
        Ok(session)
    }

    /// Montiert eine Runtime und eröffnet deren Wurzelsitzung.
    fn open(&self, id: Option<harw_types::SessionId>) -> Result<Session, SdkError> {
        let inner = &*self.inner;
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (turn_tx, turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();

        let contributor = SdkContributor {
            tools: inner.tools.clone(),
            #[cfg(feature = "unstable-internals")]
            raw_tools: inner.raw_tools.clone(),
            #[cfg(not(feature = "unstable-internals"))]
            raw_tools: Vec::new(),
            contexts: inner.contexts.clone(),
        };
        let mut builder = RuntimeAssembly::builder(inner.spec.spec())
            .model(inner.model.source())
            .stores(RuntimeStores {
                state_store: Arc::clone(&inner.state_store),
                job_store: None,
                approval_store: None,
            })
            // Pflicht für den Kind-Spawner des Profils; derselbe Sender geht
            // unten an `new_root_session`.
            .session_events(event_tx.clone())
            .contributor(Arc::new(contributor));
        #[cfg(feature = "unstable-internals")]
        if let Some(resolver) = inner.secret_resolver.as_ref() {
            builder = builder.secret_resolver(Arc::clone(resolver));
        }
        if let Some(id) = id {
            builder = builder.root_session_id(id);
        }
        let assembly = builder.build().map_err(SdkError::from_runtime)?;

        let root_id = assembly.root_session_id().clone();
        // Kein Responder: Rückfragen pausieren den Turn, und `Session::send`
        // beantwortet sie über den `ApprovalHandler` des Einbettenden.
        let RootSession {
            mut session,
            approval_mode,
        } = assembly
            .new_root_session(root_id, event_tx, turn_tx, None)
            .map_err(SdkError::from_runtime)?;

        if let Some(policy) = inner.approval_policy {
            approval_mode.set(policy.to_core());
        }
        if let Some(model) = inner.model_id.as_deref() {
            session.set_active_model(Some(harw_types::ModelId::from(model)));
        }
        if let Some(provider) = inner.provider_id.as_deref() {
            session.set_active_provider(Some(harw_types::ProviderId::from(provider)));
        }
        // Die UIA bestimmt das Werkzeugprofil der Wurzel; SDK-Werkzeuge
        // werden ausdrücklich sichtbar geschaltet.
        for name in &inner.tool_names {
            session
                .activation_mut()
                .enable_tool(harw_extension_api::ToolName::new(name.clone()));
        }

        // A resumed session may hold images: attach an existing media store
        // (creates nothing) so they can be sent again.
        inner.media.attach_existing();
        Ok(Session::new(
            assembly,
            session,
            Arc::clone(&inner.approvals),
            turn_rx,
            event_rx,
            Arc::clone(&inner.media),
        ))
    }
}
