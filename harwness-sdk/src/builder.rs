//! `HarwnessBuilder`: Konfiguration und Prüfung einer [`crate::Harwness`].
//!
//! # Beschreibung
//! Der Builder sammelt nur Angaben; [`HarwnessBuilder::build`] prüft sie in
//! zwei Stufen:
//! 1. **Eingaben** (ohne Dateisystem-Schreibzugriff): Arbeitsverzeichnis,
//!    Home-Pfad, Namen, Werkzeuge und Kontextquellen.
//! 2. **Umgebung**: Root-Space auflösen (und optional anlegen),
//!    Konfiguration mit Repo-Vertrauensprüfung laden, aktive UIA und
//!    Modell-Provider prüfen, Verlaufsspeicher öffnen.
//!
//! Jede Sitzung ([`crate::Harwness::session`]) montiert danach ihre eigene
//! Runtime aus diesen geprüften Angaben.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::adapter::{self, SdkContextProvider, SdkToolProvider};
use crate::approval::{self, ApprovalHandler, ApprovalPolicy};
use crate::error::SdkError;
use crate::harwness::{Harwness, Inner, ModelChoice, SpecInputs};
use crate::tool::{ContextSource, Tool};

/// Vorgabe-Kennung des Aufrufers, wenn [`HarwnessBuilder::principal_id`]
/// fehlt.
pub const DEFAULT_PRINCIPAL_ID: &str = "sdk:local";

/// Interaktionsmodus der Wurzelsitzung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Mode {
    /// Gespräch ohne Moduseinschränkung.
    Chat,
    /// Planung: Plan-/Recherche-Werkzeuge, keine Mutation.
    Plan,
    /// Exploration: nur lesende Werkzeuge.
    Explore,
    /// Ausführung: voller Werkzeugsatz inkl. Schreiben und Shell.
    Work,
    /// Host-Arbeit wie `Work`, Host-Zugriffe nur über ausdrückliche Freigabe.
    Shell,
}

impl Mode {
    pub(crate) fn to_core(self) -> harw_core::InteractionMode {
        match self {
            Self::Chat => harw_core::InteractionMode::Chat,
            Self::Plan => harw_core::InteractionMode::Plan,
            Self::Explore => harw_core::InteractionMode::Explore,
            Self::Work => harw_core::InteractionMode::Work,
            Self::Shell => harw_core::InteractionMode::Shell,
        }
    }
}

/// Reasoning-Aufwand des Modells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReasoningEffort {
    /// Minimal.
    Minimal,
    /// Niedrig.
    Low,
    /// Mittel.
    Medium,
    /// Hoch.
    High,
    /// Sehr hoch.
    Xhigh,
    /// Maximal.
    Max,
}

impl ReasoningEffort {
    pub(crate) fn to_core(self) -> harw_types::ReasoningEffort {
        match self {
            Self::Minimal => harw_types::ReasoningEffort::Minimal,
            Self::Low => harw_types::ReasoningEffort::Low,
            Self::Medium => harw_types::ReasoningEffort::Medium,
            Self::High => harw_types::ReasoningEffort::High,
            Self::Xhigh => harw_types::ReasoningEffort::Xhigh,
            Self::Max => harw_types::ReasoningEffort::Max,
        }
    }
}

/// Rohe, **instabile** Erweiterungen (Feature `unstable-internals`).
#[cfg(feature = "unstable-internals")]
#[derive(Default)]
pub(crate) struct RawExtensions {
    pub(crate) tool_providers: Vec<Arc<dyn harw_extension_api::ToolProvider>>,
    pub(crate) context_providers: Vec<Arc<dyn harw_extension_api::ContextProvider>>,
    pub(crate) model_provider: Option<Arc<dyn harw_core::ModelProvider>>,
    pub(crate) secret_resolver: Option<Arc<dyn harw_provider_http::SecretResolver + Send + Sync>>,
}

/// Baut eine [`Harwness`].
///
/// # Beispiel
/// ```rust,no_run
/// # async fn demo() -> Result<(), harwness_sdk::SdkError> {
/// use harwness_sdk::prelude::*;
///
/// let harwness = Harwness::builder()
///     .cwd("/srv/project")
///     .model("claude-sonnet-4-5")
///     .approval_policy(ApprovalPolicy::Delegated)
///     .build()?;
/// let mut session = harwness.session()?;
/// let report = session.send("Fasse die README zusammen.").await?;
/// println!("{}", report.text.unwrap_or_default());
/// # Ok(()) }
/// ```
pub struct HarwnessBuilder {
    home: Option<PathBuf>,
    cwd: Option<PathBuf>,
    scaffold_home: bool,
    ephemeral: bool,
    principal_id: String,
    model: ModelChoice,
    model_id: Option<String>,
    provider_id: Option<String>,
    mode: Option<Mode>,
    reasoning_effort: Option<ReasoningEffort>,
    agent: Option<String>,
    approval_policy: Option<ApprovalPolicy>,
    approvals: Arc<dyn ApprovalHandler>,
    tools: Vec<Arc<dyn Tool>>,
    contexts: Vec<Arc<dyn ContextSource>>,
    embedded: Option<Arc<harw_runtime::EmbeddedAgent>>,
    child_backend: Option<Arc<dyn harw_core::child_backend::ChildBackend>>,
    #[cfg(feature = "unstable-internals")]
    raw: RawExtensions,
}

impl Default for HarwnessBuilder {
    fn default() -> Self {
        Self {
            home: None,
            cwd: None,
            scaffold_home: true,
            ephemeral: false,
            principal_id: DEFAULT_PRINCIPAL_ID.to_owned(),
            model: ModelChoice::Configured,
            model_id: None,
            provider_id: None,
            mode: None,
            reasoning_effort: None,
            agent: None,
            approval_policy: None,
            approvals: approval::default_handler(),
            tools: Vec::new(),
            contexts: Vec::new(),
            embedded: None,
            child_backend: None,
            #[cfg(feature = "unstable-internals")]
            raw: RawExtensions::default(),
        }
    }
}

impl std::fmt::Debug for HarwnessBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarwnessBuilder")
            .field("home", &self.home)
            .field("cwd", &self.cwd)
            .field("scaffold_home", &self.scaffold_home)
            .field("ephemeral", &self.ephemeral)
            .field("model", &self.model)
            .field("model_id", &self.model_id)
            .field("provider_id", &self.provider_id)
            .field("mode", &self.mode)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("agent", &self.agent)
            .field("approval_policy", &self.approval_policy)
            .field("tools", &self.tools.len())
            .field("contexts", &self.contexts.len())
            .field("embedded", &self.embedded.is_some())
            .field("child_backend", &self.child_backend.is_some())
            .finish_non_exhaustive()
    }
}

/// Die in Stufe 1 geprüften Eingaben.
#[derive(Debug)]
pub(crate) struct ValidatedInputs {
    pub(crate) home: Option<PathBuf>,
    pub(crate) cwd: PathBuf,
    pub(crate) tools: Option<Arc<SdkToolProvider>>,
}

impl HarwnessBuilder {
    /// Root-Space (Vorgabe: `HARW_HOME`, sonst `~/.harw`).
    #[must_use]
    pub fn home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// Projektverzeichnis bzw. Arbeitsverzeichnis (Vorgabe: das des
    /// Prozesses). Bestimmt Projekterkennung, Sandbox-Wurzel und
    /// Projektkonfiguration.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Ob [`Self::build`] fehlende Verzeichnisse und Vorgabedateien im
    /// Root-Space anlegt (Vorgabe: `true`). Mit `false` muss der Root-Space
    /// existieren.
    #[must_use]
    pub fn scaffold_home(mut self, scaffold: bool) -> Self {
        self.scaffold_home = scaffold;
        self
    }

    /// Flüchtiger Verlauf im Speicher statt im Profil (Vorgabe: `false`).
    /// Fortsetzen ([`Harwness::resume`]) klappt dann nur im selben Prozess.
    #[must_use]
    pub fn ephemeral(mut self, ephemeral: bool) -> Self {
        self.ephemeral = ephemeral;
        self
    }

    /// Kennung des einbettenden Aufrufers für Protokoll und Audit
    /// (Vorgabe: [`DEFAULT_PRINCIPAL_ID`]).
    #[must_use]
    pub fn principal_id(mut self, id: impl Into<String>) -> Self {
        self.principal_id = id.into();
        self
    }

    /// Modell-ID für jeden Turn (Vorgabe: `default_model` der Konfiguration).
    #[must_use]
    pub fn model(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }

    /// Provider-ID für jeden Turn (Vorgabe: `default_provider`).
    #[must_use]
    pub fn provider(mut self, provider_id: impl Into<String>) -> Self {
        self.provider_id = Some(provider_id.into());
        self
    }

    /// Ersetzt den konfigurierten Provider durch den eingebauten
    /// Offline-Echo, der stets `reply` antwortet. Kein Netz, keine
    /// Zugangsdaten — für Tests und Beispiele.
    #[must_use]
    pub fn offline_echo(mut self, reply: impl Into<String>) -> Self {
        self.model = ModelChoice::Echo(reply.into());
        self
    }

    /// Interaktionsmodus der Wurzelsitzung.
    #[must_use]
    pub fn mode(mut self, mode: Mode) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Reasoning-Aufwand (Vorgabe: aus Provider/Modell/Agent-Konfiguration).
    #[must_use]
    pub fn reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Benannter Wurzel-Agent. Hinweis: an diesem Einstieg bestimmt die
    /// konfigurierte UIA die Wurzel; der Name wird durchgereicht, hat aber
    /// nur Wirkung, wo die Runtime ihn auswertet.
    #[must_use]
    pub fn agent(mut self, name: impl Into<String>) -> Self {
        self.agent = Some(name.into());
        self
    }

    /// Freigabepolitik (Vorgabe: [`ApprovalPolicy::Delegated`]).
    #[must_use]
    pub fn approval_policy(mut self, policy: ApprovalPolicy) -> Self {
        self.approval_policy = Some(policy);
        self
    }

    /// Wer zurückgehaltene Werkzeugaufrufe freigibt (Vorgabe:
    /// [`crate::AutoDeny`]).
    #[must_use]
    pub fn approval_handler(mut self, handler: impl ApprovalHandler) -> Self {
        self.approvals = Arc::new(handler);
        self
    }

    /// Wie [`Self::approval_handler`], für einen bereits geteilten Handler.
    #[must_use]
    pub fn approval_handler_arc(mut self, handler: Arc<dyn ApprovalHandler>) -> Self {
        self.approvals = handler;
        self
    }

    /// Fügt ein eigenes Werkzeug hinzu.
    #[must_use]
    pub fn tool(mut self, tool: impl Tool) -> Self {
        self.tools.push(Arc::new(tool));
        self
    }

    /// Wie [`Self::tool`], für ein bereits geteiltes Werkzeug.
    #[must_use]
    pub fn tool_arc(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Fügt eine Kontextquelle hinzu.
    #[must_use]
    pub fn context_source(mut self, source: impl ContextSource) -> Self {
        self.contexts.push(Arc::new(source));
        self
    }

    /// Bettet ein kompiliertes Agenten-Artefakt ein (#22 Welle 3A):
    /// Konfiguration, Agent, Skills und Wissen kommen dann ausschließlich aus
    /// `agent` (`harw_runtime::config::load_config_embedded`) statt aus
    /// `~/.harw`. [`Self::build`] überspringt dafür die Pflicht einer
    /// konfigurierten UIA ([`harw_config::HarnessConfig::active_uia_definition`])
    /// und eines konfigurierten Standard-Providers — beides ersetzt das
    /// Manifest des Artefakts (`[models]` seiner Wurzel-IR). [`Self::home`]
    /// bleibt dabei ausdrücklich nutzbar: nur für Sitzungen und Protokolle,
    /// nie für Agenten, Skills oder Profil.
    #[must_use]
    pub fn embedded(mut self, agent: Arc<harw_runtime::EmbeddedAgent>) -> Self {
        self.embedded = Some(agent);
        self
    }

    /// Verdrahtet ein [`harw_core::child_backend::ChildBackend`] (#22 Welle
    /// 3C): jedes Kind, das die Wurzelmontage über ihren
    /// `ManagedAgentSpawner` admittiert, läuft dann über dieses Backend
    /// (typischerweise `harw-agent-runner`s `JobChildBackend`) statt
    /// in-process. Ohne diesen Aufruf bleibt jeder Kind-Lauf in-process — der
    /// bestehende Pfad.
    #[must_use]
    pub fn child_backend(
        mut self,
        backend: Arc<dyn harw_core::child_backend::ChildBackend>,
    ) -> Self {
        self.child_backend = Some(backend);
        self
    }

    /// **Instabil.** Hängt einen rohen internen Tool-Provider an.
    #[cfg(feature = "unstable-internals")]
    #[must_use]
    pub fn raw_tool_provider(
        mut self,
        provider: Arc<dyn harw_extension_api::ToolProvider>,
    ) -> Self {
        self.raw.tool_providers.push(provider);
        self
    }

    /// **Instabil.** Hängt einen rohen internen Kontext-Provider an.
    #[cfg(feature = "unstable-internals")]
    #[must_use]
    pub fn raw_context_provider(
        mut self,
        provider: Arc<dyn harw_extension_api::ContextProvider>,
    ) -> Self {
        self.raw.context_providers.push(provider);
        self
    }

    /// **Instabil.** Ersetzt den konfigurierten Provider durch einen eigenen.
    #[cfg(feature = "unstable-internals")]
    #[must_use]
    pub fn raw_model_provider(mut self, provider: Arc<dyn harw_core::ModelProvider>) -> Self {
        self.raw.model_provider = Some(provider);
        self
    }

    /// **Instabil.** Resolver für `auth = "secrets:…"` des konfigurierten
    /// Providers (ohne ihn schlägt jede solche Referenz fehl).
    #[cfg(feature = "unstable-internals")]
    #[must_use]
    pub fn raw_secret_resolver(
        mut self,
        resolver: Arc<dyn harw_provider_http::SecretResolver + Send + Sync>,
    ) -> Self {
        self.raw.secret_resolver = Some(resolver);
        self
    }

    /// Stufe 1: prüft alle Eingaben ohne Schreibzugriff.
    ///
    /// # Fehler
    /// [`SdkError::InvalidInput`] mit dem Namen der ersten ungültigen Angabe.
    pub(crate) fn validate(&self) -> Result<ValidatedInputs, SdkError> {
        let cwd = match &self.cwd {
            Some(cwd) => absolute("cwd", cwd)?,
            None => std::env::current_dir().map_err(|error| {
                SdkError::invalid("cwd", format!("process cwd is unavailable: {error}"))
            })?,
        };
        if !cwd.is_dir() {
            return Err(SdkError::invalid(
                "cwd",
                format!("'{}' is not an existing directory", cwd.display()),
            ));
        }
        let home = match &self.home {
            Some(home) => {
                let home = absolute("home", home)?;
                if home.exists() && !home.is_dir() {
                    return Err(SdkError::invalid(
                        "home",
                        format!("'{}' exists but is not a directory", home.display()),
                    ));
                }
                Some(home)
            }
            None => None,
        };
        non_blank("principal_id", Some(&self.principal_id))?;
        non_blank("model", self.model_id.as_ref())?;
        non_blank("provider", self.provider_id.as_ref())?;
        non_blank("agent", self.agent.as_ref())?;
        adapter::validate_context_sources(&self.contexts)?;
        let tools = if self.tools.is_empty() {
            None
        } else {
            Some(Arc::new(SdkToolProvider::new(self.tools.clone())?))
        };
        Ok(ValidatedInputs { home, cwd, tools })
    }

    /// Prüft alles und baut die [`Harwness`].
    ///
    /// # Fehler
    /// - [`SdkError::InvalidInput`]: eine Builder-Angabe ist ungültig.
    /// - [`SdkError::Home`]: Root-Space nicht auflösbar/anlegbar.
    /// - [`SdkError::Config`]: Konfiguration nicht ladbar oder nicht
    ///   vertrauenswürdig, oder keine aktive UIA
    ///   (`harness.active_uia_definition`; `harw` legt sie beim ersten Start an).
    /// - [`SdkError::Provider`]: weder `default_provider` konfiguriert noch
    ///   [`Self::provider`] / [`Self::offline_echo`] gesetzt.
    /// - [`SdkError::Session`]: Verlaufsspeicher nicht anlegbar.
    pub fn build(self) -> Result<Harwness, SdkError> {
        let validated = self.validate()?;
        let home = match validated.home {
            Some(home) => home,
            None => harw_home::home_dir().map_err(|error| SdkError::Home {
                detail: error.to_string(),
            })?,
        };
        if self.scaffold_home {
            harw_home::ensure_home(&home).map_err(|error| SdkError::Home {
                detail: format!("could not prepare '{}': {error}", home.display()),
            })?;
        } else if !home.is_dir() {
            return Err(SdkError::Home {
                detail: format!(
                    "'{}' does not exist and scaffolding is disabled",
                    home.display()
                ),
            });
        }

        #[cfg(feature = "unstable-internals")]
        let RawExtensions {
            tool_providers: raw_tools,
            context_providers: raw_contexts,
            model_provider: raw_model,
            secret_resolver,
        } = self.raw;
        #[cfg(feature = "unstable-internals")]
        let model = match raw_model {
            Some(provider) => ModelChoice::Override(provider),
            None => self.model,
        };
        #[cfg(not(feature = "unstable-internals"))]
        let model = self.model;

        // #22 Welle 3A: ein eingebettetes Artefakt bestimmt seinen
        // Wurzel-Agenten selbst (dieselbe Rolle wie `--agent`); ein
        // ausdrücklich gesetzter `agent()` behält Vorrang.
        let agent = self.agent.or_else(|| {
            self.embedded
                .as_ref()
                .map(|agent| agent.root_id().to_owned())
        });
        let spec_inputs = SpecInputs {
            home,
            cwd: validated.cwd,
            principal_id: self.principal_id,
            mode: self.mode,
            reasoning_effort: self.reasoning_effort,
            agent,
            embedded: self.embedded.clone(),
            child_backend: self
                .child_backend
                .clone()
                .map(harw_runtime::ChildBackendHandle),
        };
        let config = if self.embedded.is_some() {
            harw_runtime::load_config_embedded(&spec_inputs.spec())
                .map_err(SdkError::from_runtime)?
        } else {
            let (config, _trust) =
                harw_runtime::load_config(&spec_inputs.spec()).map_err(SdkError::from_runtime)?;
            config
        };
        // Ein explizit gewählter Agent ersetzt die UIA als Wurzel; nur ohne ihn
        // ist eine aktive UIA Pflicht. Ein eingebettetes Artefakt bringt seine
        // eigene Wurzel mit (oben gesetzt) und braucht daher nie eine UIA.
        if self.embedded.is_none()
            && spec_inputs.agent.is_none()
            && config.harness.active_uia_definition.is_none()
        {
            return Err(SdkError::Config {
                detail: "no active UIA is configured (harness.active_uia_definition); \
                         run `harw` once interactively or set it in the profile config"
                    .to_owned(),
            });
        }
        // Ebenso ersetzt das Manifest des eingebetteten Artefakts
        // (`[models]` seiner Wurzel-IR, bereits von `load_config_embedded`
        // geprüft) die Pflicht eines konfigurierten Standard-Providers.
        if self.embedded.is_none()
            && matches!(model, ModelChoice::Configured)
            && self.provider_id.is_none()
            && config.harness.default_provider.is_none()
        {
            return Err(SdkError::Provider {
                detail: "no default provider is configured; run `harw onboard`, call \
                         HarwnessBuilder::provider, or use HarwnessBuilder::offline_echo"
                    .to_owned(),
            });
        }
        let state_store = adapter::open_state_store(&spec_inputs.home, self.ephemeral)?;

        #[cfg_attr(not(feature = "unstable-internals"), allow(unused_mut))]
        let mut contexts: Vec<Arc<dyn harw_extension_api::ContextProvider>> = self
            .contexts
            .into_iter()
            .map(|source| {
                Arc::new(SdkContextProvider::new(source))
                    as Arc<dyn harw_extension_api::ContextProvider>
            })
            .collect();
        #[cfg(feature = "unstable-internals")]
        contexts.extend(raw_contexts);

        let media = Arc::new(crate::media::MediaHandle::new(&spec_inputs.home));
        Ok(Harwness::from_inner(Inner {
            media,
            spec: spec_inputs,
            model,
            model_id: self.model_id,
            provider_id: self.provider_id,
            approval_policy: self.approval_policy,
            approvals: self.approvals,
            tool_names: validated
                .tools
                .as_ref()
                .map(|tools| tools.names())
                .unwrap_or_default(),
            tools: validated.tools,
            contexts,
            state_store,
            #[cfg(feature = "unstable-internals")]
            raw_tools,
            #[cfg(feature = "unstable-internals")]
            secret_resolver,
        }))
    }
}

/// Macht einen Pfad absolut (ohne Symlinks aufzulösen).
fn absolute(field: &'static str, path: &Path) -> Result<PathBuf, SdkError> {
    std::path::absolute(path)
        .map_err(|error| SdkError::invalid(field, format!("'{}': {error}", path.display())))
}

/// Verlangt, dass eine gesetzte Angabe nicht leer ist.
fn non_blank(field: &'static str, value: Option<&String>) -> Result<(), SdkError> {
    match value {
        Some(value) if value.trim().is_empty() => {
            Err(SdkError::invalid(field, "must not be empty"))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{FnTool, ToolError};
    use serde_json::json;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn field_of(result: Result<ValidatedInputs, SdkError>) -> Option<&'static str> {
        match result {
            Err(SdkError::InvalidInput { field, .. }) => Some(field),
            _ => None,
        }
    }

    fn noop_tool(name: &str) -> Arc<dyn Tool> {
        Arc::new(FnTool::new(
            name,
            "noop",
            json!({"type": "object"}),
            |_args: serde_json::Value| {
                std::future::ready(Ok::<_, ToolError>(serde_json::Value::Null))
            },
        ))
    }

    #[test]
    fn a_missing_cwd_is_rejected() -> TestResult {
        let dir = tempfile::tempdir()?;
        let builder = HarwnessBuilder::default().cwd(dir.path().join("missing"));
        assert_eq!(field_of(builder.validate()), Some("cwd"));
        Ok(())
    }

    #[test]
    fn a_file_is_neither_cwd_nor_home() -> TestResult {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("file.txt");
        std::fs::write(&file, "x")?;
        let as_cwd = HarwnessBuilder::default().cwd(&file);
        assert_eq!(field_of(as_cwd.validate()), Some("cwd"));
        let as_home = HarwnessBuilder::default().cwd(dir.path()).home(&file);
        assert_eq!(field_of(as_home.validate()), Some("home"));
        Ok(())
    }

    #[test]
    fn blank_names_are_rejected() -> TestResult {
        let dir = tempfile::tempdir()?;
        let base = || HarwnessBuilder::default().cwd(dir.path());
        assert_eq!(field_of(base().model(" ").validate()), Some("model"));
        assert_eq!(field_of(base().provider("").validate()), Some("provider"));
        assert_eq!(field_of(base().agent("\t").validate()), Some("agent"));
        assert_eq!(
            field_of(base().principal_id("").validate()),
            Some("principal_id")
        );
        Ok(())
    }

    #[test]
    fn tools_are_validated_before_any_assembly() -> TestResult {
        let dir = tempfile::tempdir()?;
        let base = || HarwnessBuilder::default().cwd(dir.path());
        assert_eq!(
            field_of(base().tool_arc(noop_tool("bad name")).validate()),
            Some("tool.name")
        );
        assert_eq!(
            field_of(
                base()
                    .tool_arc(noop_tool("a"))
                    .tool_arc(noop_tool("a"))
                    .validate()
            ),
            Some("tool.name")
        );
        let valid = base()
            .tool_arc(noop_tool("host.a"))
            .tool_arc(noop_tool("host.b"))
            .validate()?;
        let names = valid.tools.as_ref().map(|tools| tools.names());
        assert_eq!(names, Some(vec!["host.a".to_owned(), "host.b".to_owned()]));
        Ok(())
    }

    #[test]
    fn a_valid_builder_resolves_an_absolute_cwd() -> TestResult {
        let dir = tempfile::tempdir()?;
        let valid = HarwnessBuilder::default()
            .cwd(dir.path())
            .home(dir.path().join("home-not-yet-created"))
            .validate()?;
        assert!(valid.cwd.is_absolute());
        assert!(valid.home.as_ref().is_some_and(|home| home.is_absolute()));
        assert!(valid.tools.is_none());
        Ok(())
    }

    #[test]
    fn build_without_scaffolding_requires_an_existing_home() -> TestResult {
        let dir = tempfile::tempdir()?;
        let result = HarwnessBuilder::default()
            .cwd(dir.path())
            .home(dir.path().join("absent"))
            .scaffold_home(false)
            .build();
        assert!(matches!(result, Err(SdkError::Home { .. })));
        Ok(())
    }

    #[test]
    fn modes_and_efforts_map_to_the_runtime() {
        assert_eq!(Mode::Work.to_core(), harw_core::InteractionMode::Work);
        assert_eq!(Mode::Explore.to_core(), harw_core::InteractionMode::Explore);
        assert_eq!(
            ReasoningEffort::High.to_core(),
            harw_types::ReasoningEffort::High
        );
    }

    /// #22 Welle 3C: [`HarwnessBuilder::child_backend`] legt genau die
    /// übergebene `Arc`-Instanz im Feld ab, das [`HarwnessBuilder::build`]
    /// in [`SpecInputs::child_backend`] weiterreicht.
    #[test]
    fn child_backend_sets_the_field() {
        struct FakeBackend;
        impl harw_core::child_backend::ChildBackend for FakeBackend {
            fn run<'a>(
                &'a self,
                _spec: harw_core::child_backend::ChildRunSpec,
                _io: &'a dyn harw_core::child_backend::ChildIo,
            ) -> harw_core::child_backend::ChildBackendFuture<'a> {
                Box::pin(async {
                    harw_core::child_backend::ChildRunOutcome {
                        status: harw_core::child_backend::ChildRunStatus::Completed,
                        text: None,
                        usage: harw_core::ChildUsage::default(),
                        continuation: None,
                    }
                })
            }
        }

        let backend: Arc<dyn harw_core::child_backend::ChildBackend> = Arc::new(FakeBackend);
        let builder = HarwnessBuilder::default().child_backend(Arc::clone(&backend));
        assert!(
            builder
                .child_backend
                .as_ref()
                .is_some_and(|installed| Arc::ptr_eq(installed, &backend)),
            "child_backend() muss die übergebene Arc-Instanz im Feld ablegen"
        );
    }
}
