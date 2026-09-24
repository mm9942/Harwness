//! Die eine Service-Fabrik aller `harw`-Einstiege.
//!
//! # Beschreibung
//! Heute baut jeder Einstieg seine [`ServiceMap`] selbst zusammen — die CLI im
//! One-Shot-Pfad (`harw-cli/src/chat.rs`), die TUI zweimal (`harw-tui/src/app.rs`
//! für Modell-Werkzeuge, `harw-tui/src/command_exec.rs` für Slash-Kommandos),
//! die Web-Oberfläche noch einmal (`harw-cli/src/web.rs`) und der Analyze-Pfad
//! (`harw-cli/src/main.rs`). Die fünf Montagen sind auseinandergelaufen
//! (Befunde G-060, G-061, G-098): der Slash-Pfad der TUI kennt weder
//! `ManagedAgentSpawner` (`/agent` → `OpError::NotAvailable`) noch die
//! Plan-Dienste (Plan-Werkzeuge in der TUI immer `NotAvailable`, G-024), und
//! die Web-Fläche kennt weder `JobStore` noch `StateStore`.
//!
//! Dieses Modul ersetzt die fünf Montagen durch **eine** Fabrik:
//! [`RuntimeServices::service_map`]. Unterschiede zwischen den Flächen gibt es
//! weiterhin, aber nur noch **deklariert** — siehe [`ServiceSurface`] und die
//! Tabelle in [`RuntimeServices::service_map`]. Was in den `Parts` fehlt
//! (`None`), fehlt auf jeder Fläche gleich; was vorhanden ist, landet überall
//! dort, wo die Tabelle es erlaubt.
//!
//! # Schlüsseltypen
//! - [`ServiceSurface`] — die vier Flächen, für die Services montiert werden
//! - [`PlanServices`] — die drei Plan-Speicher plus ihre Konfiguration
//! - [`RuntimeServicesParts`] — alle Zutaten der Komposition (Composition Root)
//! - [`RuntimeServices`] — die Fabrik selbst
//!
//! # Wissensfläche (L6)
//! `Arc<harw_knowledge::KnowledgeStore>` wird aus
//! `harw_home::knowledge_dir(profile_dir)` des gebundenen
//! [`harw_home::ResolvedHomeContext`] gebaut ([`RuntimeServices::with_home_context`])
//! und — dritte deklarierte Differenz, [`ServiceSurface::allows_knowledge_store`]
//! — nur auf Slash und Modell-Werkzeug gelegt. Damit finden `/workbench`,
//! `/kanban`, `/diary`, `/palace`, `/dream` und `/context-proposal` ihren Speicher.
//! Web und Job bekommen ihn bewusst noch nicht: beide haben keine Sitzung,
//! deren Workbench sie beschreiben dürften, und die Sichtbarkeitsprüfung der
//! Wissensfläche kennt noch keinen Web-/Job-Aufrufer.
//!
//! # Agenten-Event-Bus
//! Ist per [`RuntimeServices::with_agent_events`] ein `Arc<AgentEventHub>`
//! gebunden, legt die Fabrik ihn — vierte deklarierte Differenz,
//! [`ServiceSurface::allows_agent_events`] — auf Slash und Modell-Werkzeug.
//! Damit veröffentlicht `/matrix` Live-Ereignisse
//! ([`harw_core::AgentEventKind::Matrix`]) für die TUI-Matrix-Ansicht. Web und
//! Job haben keine laufende Sitzung mit Beobachter und bleiben ohne.
//!
//! # Kanban-Job-Ledger
//! Liegen ein Wissensspeicher **und** ein `JobStore` vor und trägt das
//! [`Principal`] einen Freigabe-Akteur (`Principal::approval_actor`), legt
//! die Fabrik auf denselben Flächen wie den Wissensspeicher (Slash,
//! Modell-Werkzeug) zusätzlich `Arc<dyn JobTransitions>` ab —
//! [`crate::job_ledger::JobStoreTransitions`] über den geteilten
//! `JobStore`. Damit bewegt `/kanban` Karten über den durablen Ledger.
//! Fehlt eine der drei Zutaten, fehlt der Ledger (fail closed: `/kanban`
//! meldet Übergänge dann `NotAvailable`).
//!
//! # Was hier bewusst NICHT registriert wird
//! - Web-eigene Dienste (`PeerCredentials`, `Arc<dyn ApprovalActorResolver>`,
//!   `Arc<ApprovalStore>`): sie stammen pro Verbindung aus dem Kernel bzw. aus
//!   dem Web-Server und gehören nicht in eine prozessweit gebaute
//!   Komposition. Der Web-Einstieg ergänzt sie nach [`RuntimeServices::service_map`].
//!
//! # Nebenläufigkeit
//! [`RuntimeServices`] ist `Send + Sync` und wird typischerweise hinter einem
//! `Arc` geteilt; [`RuntimeServices::service_map`] klont je Aufruf nur
//! `Arc`-Zeiger und baut eine frische [`ServiceMap`] — kein geteilter
//! veränderlicher Zustand. Die [`ApprovalModeCell`] ist die eine Ausnahme: sie
//! trägt ihren Zustand selbst, alle Klone teilen ihn. Das ist Absicht — der
//! Freigabemodus einer Sitzung muss über Slash-Kommando und Modell-Werkzeug
//! hinweg derselbe sein.
//!
//! # Fehlertypen
//! Dieses Modul erzeugt keine Fehler.
//!
//! # Spec
//! `docs/remediation/CONTRACTS.md` §runtime-spec (Reduktionstabelle je
//! [`crate::EntryKind`]) — die Spawner-Spalte dort begründet die einzige
//! Spawner-Differenz dieser Fabrik.

use std::any::{Any, type_name};
use std::sync::Arc;

use harw_config::ResolvedConfig;
use harw_core::{AgentEventHub, ManagedAgentSpawner, StateStore};
use harw_extension_api::allow_rules::AllowRuleSet;
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_job_runtime::JobScope;
use harw_knowledge::KnowledgeStore;
use harw_knowledge::kanban::lifecycle::JobTransitions;
use harw_memory::Memory;
use harw_operations::registry::OperationRegistry;
use harw_operations::{ServiceMap, SharedSessionController};
use harw_plan::{GoalStore, PlanStore, PlanToolConfig};
use harw_plan_bridge::{FindingStore, register_plan_services};
use harw_provider_http::ProviderLoadRegistry;
use harw_sandbox::ExtraRootsCell;
use harw_session_store::JobStore;
use harw_tool_shell::HostPermitHandles;
use harw_types::{Principal, TenantId, WorkspaceId};

use crate::job_ledger::JobStoreTransitions;

/// Mandant, unter dem die Fabrik Kanban-Jobs zulässt (lokaler Einzelbetrieb).
pub const KANBAN_TENANT: &str = "local";

/// Arbeitsbereich, unter dem die Fabrik Kanban-Jobs zulässt.
pub const KANBAN_WORKSPACE: &str = "kanban";

// ── ServiceSurface ────────────────────────────────────────────────────────────

/// Die Fläche, für die eine [`ServiceMap`] montiert wird.
///
/// # Beschreibung
/// Eine Fläche ist **kein** Einstieg: ein Einstieg ([`crate::EntryKind`]) kann
/// mehrere Flächen bedienen — die TUI etwa [`ServiceSurface::Slash`] und
/// [`ServiceSurface::ModelTool`]. Die Fläche sagt nur, *wer* die Operation
/// auslöst; welche Rechte dabei gelten, sagt weiterhin allein
/// [`crate::EntryKind::profile`].
///
/// # Beispiel
/// ```rust
/// use harw_runtime::services::ServiceSurface;
///
/// // Slash und Modell-Werkzeug sehen dieselben Dienste — darum ist `/agent`
/// // als Slash-Kommando nicht mehr `NotAvailable` (G-061).
/// assert_eq!(
///     ServiceSurface::Slash.allows_spawner(),
///     ServiceSurface::ModelTool.allows_spawner()
/// );
/// assert!(!ServiceSurface::Web.allows_spawner());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServiceSurface {
    /// Von einer Person getipptes Slash-Kommando (TUI, CLI).
    Slash,
    /// Vom Modell aufgerufenes Werkzeug.
    ModelTool,
    /// Anfrage der lokalen Web-Oberfläche.
    Web,
    /// Durabler Job-Lauf ohne anwesende Person.
    Job,
}

impl ServiceSurface {
    /// Alle Flächen in stabiler Reihenfolge.
    ///
    /// # Rückgabe
    /// Ein Array aller Varianten — gedacht für Tests und Rechte-Snapshots, die
    /// über jede Fläche iterieren müssen.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_runtime::services::ServiceSurface;
    /// assert_eq!(ServiceSurface::ALL.len(), 4);
    /// ```
    pub const ALL: [Self; 4] = [Self::Slash, Self::ModelTool, Self::Web, Self::Job];

    /// Kurzname der Fläche für Protokolle und Snapshots.
    ///
    /// # Rückgabe
    /// `"slash"`, `"model-tool"`, `"web"` oder `"job"`.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_runtime::services::ServiceSurface;
    /// assert_eq!(ServiceSurface::ModelTool.as_str(), "model-tool");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Slash => "slash",
            Self::ModelTool => "model-tool",
            Self::Web => "web",
            Self::Job => "job",
        }
    }

    /// Ob diese Fläche den Kind-Agenten-Spawner erhält.
    ///
    /// # Beschreibung
    /// Erste der beiden **deklarierten** Differenzen der Fabrik. Begründung aus
    /// `CONTRACTS.md` §runtime-spec: die Spawner-Spalte gibt `BuiltinRoles` nur
    /// für `Tui`, `OneShot` und `Analyze` — also für Slash und Modell-Werkzeug;
    /// `Web`, `JobPrompt` und `JobPlanNode` stehen dort auf `SpawnerPolicy::None`.
    ///
    /// # Rückgabe
    /// `true` für [`Self::Slash`] und [`Self::ModelTool`], sonst `false`.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_runtime::services::ServiceSurface;
    /// assert!(ServiceSurface::Slash.allows_spawner());
    /// assert!(!ServiceSurface::Job.allows_spawner());
    /// ```
    #[must_use]
    pub const fn allows_spawner(self) -> bool {
        matches!(self, Self::Slash | Self::ModelTool)
    }

    /// Ob diese Fläche den Sitzungs-Controller erhält.
    ///
    /// # Beschreibung
    /// Zweite **deklarierte** Differenz (die dritte ist
    /// [`Self::allows_knowledge_store`]). Der
    /// [`SharedSessionController`] mutiert den Zustand *einer laufenden,
    /// interaktiven* Sitzung (`/model`, `/effort`, `/provider`, `/mode`). Eine
    /// Web-Anfrage und ein durabler Job haben keine solche Sitzung — dort wäre
    /// der Controller ein Verweis auf eine fremde Sitzung.
    ///
    /// # Rückgabe
    /// `true` für [`Self::Slash`] und [`Self::ModelTool`], sonst `false`.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_runtime::services::ServiceSurface;
    /// assert!(!ServiceSurface::Web.allows_session_controller());
    /// ```
    #[must_use]
    pub const fn allows_session_controller(self) -> bool {
        matches!(self, Self::Slash | Self::ModelTool)
    }

    /// Ob diese Fläche den Wissensspeicher (`Arc<KnowledgeStore>`) erhält.
    ///
    /// # Beschreibung
    /// Dritte **deklarierte** Differenz (L6): Workbench, Kanban, Diary und
    /// Palace sind Flächen einer laufenden Sitzung — Slash-Kommando und
    /// Modell-Werkzeug. Web und Job bleiben ohne, bis ihre Aufrufer eine
    /// eigene Sichtbarkeitsprüfung haben (siehe Moduldoku).
    ///
    /// # Rückgabe
    /// `true` für [`Self::Slash`] und [`Self::ModelTool`], sonst `false`.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_runtime::services::ServiceSurface;
    /// assert!(ServiceSurface::ModelTool.allows_knowledge_store());
    /// assert!(!ServiceSurface::Job.allows_knowledge_store());
    /// ```
    #[must_use]
    pub const fn allows_knowledge_store(self) -> bool {
        matches!(self, Self::Slash | Self::ModelTool)
    }

    /// Ob diese Fläche den Agenten-Event-Bus (`Arc<AgentEventHub>`) erhält.
    ///
    /// # Beschreibung
    /// Vierte **deklarierte** Differenz: Live-Ereignisse (etwa aus `/matrix`)
    /// haben nur in einer laufenden, beobachteten Sitzung einen Empfänger —
    /// Slash-Kommando und Modell-Werkzeug. Web und Job bleiben ohne.
    ///
    /// # Rückgabe
    /// `true` für [`Self::Slash`] und [`Self::ModelTool`], sonst `false`.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_runtime::services::ServiceSurface;
    /// assert!(ServiceSurface::Slash.allows_agent_events());
    /// assert!(!ServiceSurface::Web.allows_agent_events());
    /// ```
    #[must_use]
    pub const fn allows_agent_events(self) -> bool {
        matches!(self, Self::Slash | Self::ModelTool)
    }
}

// ── PlanServices ──────────────────────────────────────────────────────────────

/// Die geöffnete Planungsfläche: drei Speicher plus ihre Konfiguration.
///
/// # Beschreibung
/// Alle drei Speicher oder keiner — dieselbe Bedingung, die heute schon
/// `harw-cli/src/chat.rs` und `harw-cli/src/web.rs` erzwingen. Darum stehen sie
/// hier in **einem** Wert und nicht als drei einzelne `Option`s: ein halb
/// geöffneter Plan-Zustand lässt sich so gar nicht erst bilden.
///
/// # Nebenläufigkeit
/// Alle Felder sind `Arc` bzw. `Clone`; das Struct ist `Send + Sync + Clone`.
#[derive(Clone)]
pub struct PlanServices {
    /// Plan-Speicher.
    pub plan: Arc<dyn PlanStore>,
    /// Ziel-Speicher.
    pub goal: Arc<dyn GoalStore>,
    /// Befund-Speicher.
    pub findings: Arc<FindingStore>,
    /// Konfiguration, mit der die Plan-Operationen registriert wurden.
    pub plan_config: PlanToolConfig,
}

// ── RuntimeServicesParts ──────────────────────────────────────────────────────

/// Alle Zutaten, aus denen [`RuntimeServices`] Service-Maps baut.
///
/// # Beschreibung
/// Wird **ausschließlich** in der Composition Root eines Einstiegs gefüllt.
/// Kein Feld darf je aus Modell-Ausgabe, Werkzeug-Argumenten oder einem
/// HTTP-Rumpf stammen — die Fabrik prüft das nicht, sie kann es nicht prüfen.
///
/// # Felder
/// Ein `None`-Feld heißt „dieser Einstieg hat den Dienst nicht"; es heißt nie
/// „diese Fläche bekommt ihn nicht". Letzteres entscheidet allein
/// [`ServiceSurface`].
pub struct RuntimeServicesParts {
    /// Vorlage der Operations-Registry. Je Fläche wird daraus eine frische
    /// [`OperationRegistry`] aus `Arc`-Klonen gebaut, weil die Registry selbst
    /// nicht `Clone` ist und Operationen sie per
    /// `ctx.service::<OperationRegistry>()` als konkreten Typ nachschlagen.
    ///
    /// # Verhältnis zu [`crate::spec::OperationSurface`]
    /// Diese Vorlage ist **bereits** die Fläche des Einstiegs: die Montage
    /// füllt sie über `assembly::build_operations` nach
    /// [`crate::spec::EntryProfile::operations`], und
    /// [`crate::spec::OperationSurface::None`] liefert eine leere Registry.
    /// Diese Fabrik verengt daher nichts mehr und darf es auch nicht — sie
    /// legt jeder [`ServiceSurface`] dieselbe Vorlage hin.
    ///
    /// Die beiden Achsen sind verschieden und dürfen nicht verwechselt werden:
    /// [`crate::spec::OperationSurface`] sagt, **welche Operationen es in
    /// diesem Lauf überhaupt gibt** (und ob sie dem Modell als Werkzeuge
    /// angeboten werden — das entscheidet allein die Montage über den
    /// `ModelToolProvider`, nicht diese Fabrik); [`ServiceSurface`] sagt,
    /// **welche Dienste** eine bereits vorhandene Operation vorfindet.
    pub operations: Arc<OperationRegistry>,
    /// Transkript-/Zustandsspeicher der Sitzung.
    pub state_store: Arc<dyn StateStore>,
    /// Durabler Job-Speicher; `None` ohne HARW-Home.
    pub job_store: Option<Arc<JobStore>>,
    /// Spawner für Kind-Agenten; `None`, wenn der Einstieg keine Kinder erlaubt.
    pub spawner: Option<Arc<ManagedAgentSpawner>>,
    /// Gedächtnis; `None`, wenn nicht konfiguriert.
    pub memory: Option<Arc<dyn Memory>>,
    /// Aufgelöste Konfiguration.
    pub config: Arc<ResolvedConfig>,
    /// Geöffnete Planungsfläche; `None`, wenn `tools.plan.enabled = false`.
    pub plan: Option<PlanServices>,
    /// Freigabemodus dieser Sitzung. Kein `Option`: `/permissions` meldet ohne
    /// Zelle `NotAvailable` (W2A-05), darum trägt jede Fläche eine.
    pub approval_mode: ApprovalModeCell,
    /// Geteilte Freigaberegeln dieser Sitzung (Contract §2/§4). Kein
    /// `Option`, aus demselben Grund wie [`Self::approval_mode`]: `/permissions`
    /// und `/add-workdir` müssen die Regelmenge über `ctx.service::<AllowRuleSet>()`
    /// unabhängig davon finden, ob überhaupt eine Regel gesät wurde — eine
    /// leere [`AllowRuleSet`] ist ein gültiger, „noch nichts gemerkt"-Zustand,
    /// keine Abwesenheit des Dienstes.
    pub allow_rules: AllowRuleSet,
    /// Zusätzliche Workspace-Wurzeln dieser Sitzung (`/add-workdir`, Contract
    /// §5 Slice A8). Kein `Option`, aus demselben Grund wie
    /// [`Self::allow_rules`].
    pub extra_roots: ExtraRootsCell,
    /// Das an der Eingangsgrenze authentifizierte Subjekt.
    pub principal: Principal,
    /// Sitzungs-Controller einer laufenden interaktiven Sitzung; `None` außerhalb.
    pub session_controller: Option<SharedSessionController>,
    /// [`ProviderLoadControl`](harw_provider_http::ProviderLoadControl)-Handles
    /// je Provider-Name (Auslastungskanal, additiv zu allem oben). Kein
    /// `Option`, aus demselben Grund wie [`Self::allow_rules`]: `/status` und
    /// `/provider` müssen die Registry über
    /// `ctx.service::<ProviderLoadRegistry>()` unabhängig davon finden, ob
    /// dieser Lauf überhaupt einen Provider mit Auslastungs-Handle gebaut hat
    /// — eine leere [`ProviderLoadRegistry`] ist ein gültiger Zustand
    /// (`ModelSource::Echo`, Loopback-Provider ohne
    /// [`harw_provider_http::ProviderLoadControl`]-Impl), keine Abwesenheit
    /// des Dienstes. Gebaut von
    /// [`crate::model::build_root_model_with_registry_and_resolver`] und
    /// [`crate::model::build_uia_model_with_registry_and_resolver`],
    /// zusammengeführt in `RuntimeAssemblyBuilder::build`.
    pub provider_load_registry: ProviderLoadRegistry,
    /// Gebündelte Host-Permit-Handles dieses Laufs (Ledger, Sitzungs-Registry,
    /// Fragekanal-Sender — Plan Teil B3). `None`, wenn dieser Einstieg keine
    /// Host-Freigabe-Verdrahtung trägt (etwa Tests oder fremde Kompositionen,
    /// die dieses Feld noch nicht setzen). Gebaut aus `assembly.rs`s einmal
    /// je Montage instanziiertem `host_permit_ledger`/`host_permit_registry`/
    /// `host_permit_prompt_sender` (siehe dort, ~1770).
    pub host_permit_handles: Option<Arc<HostPermitHandles>>,
}

// ── RuntimeServices ───────────────────────────────────────────────────────────

/// Die eine Service-Fabrik: baut zu jeder [`ServiceSurface`] eine [`ServiceMap`].
///
/// # Beschreibung
/// Siehe [`Self::service_map`] für die Tabelle, welcher Dienst auf welche
/// Fläche geht.
///
/// # Nebenläufigkeit
/// `Send + Sync`; typischerweise hinter einem `Arc` geteilt.
pub struct RuntimeServices {
    parts: RuntimeServicesParts,
    home_context: Option<Arc<harw_home::ResolvedHomeContext>>,
    knowledge: Option<Arc<KnowledgeStore>>,
    agent_events: Option<Arc<AgentEventHub>>,
    dream_launcher: Option<Arc<dyn harw_ops::dream_run::DreamLauncher>>,
}

/// Legt `service` in `map` ab und merkt sich seinen Typnamen in `names`.
///
/// # Beschreibung
/// Der einzige Weg, wie diese Fabrik etwas in eine [`ServiceMap`] legt. Damit
/// können `RuntimeServices::service_map` und `RuntimeServices::registered`
/// nicht auseinanderlaufen: beide gehen durch dieselbe Montage.
///
/// # Argumente
/// - `map` (`&mut ServiceMap`): Ziel-Map.
/// - `names` (`&mut Vec<&'static str>`): Protokoll der abgelegten Typen.
/// - `service` (`S`): der Dienst; der Schlüssel ist sein Rust-Typ.
fn insert_service<S: Any + Send + Sync>(
    map: &mut ServiceMap,
    names: &mut Vec<&'static str>,
    service: S,
) {
    names.push(type_name::<S>());
    map.insert(service);
}

impl RuntimeServices {
    /// Nimmt die Zutaten der Composition Root entgegen.
    ///
    /// # Argumente
    /// - `parts` ([`RuntimeServicesParts`]): siehe dort; wird verschoben, nicht geklont.
    ///
    /// # Rückgabe
    /// Die Fabrik.
    ///
    /// # Nebenläufigkeit
    /// Rein synchron; keine Sperren, keine Ein-/Ausgabe.
    #[must_use]
    pub fn new(parts: RuntimeServicesParts) -> Self {
        Self {
            parts,
            home_context: None,
            knowledge: None,
            agent_events: None,
            dream_launcher: None,
        }
    }

    /// Bindet den Traum-Starter für `/dream run` (Plan D5).
    ///
    /// # Beschreibung
    /// Die Montage baut ihn aus ihrem Modell, dem Wissensspeicher, dem
    /// `JobStore` und der Session-Wurzel
    /// (`crate::dream_run::RuntimeDreamLauncher`). Er liegt nur auf der
    /// Slash-Fläche — `/dream` ist ein reines Operator-Kommando ohne
    /// Modell-Werkzeug. Ohne Aufruf meldet `/dream run` `NotAvailable`.
    #[must_use]
    pub fn with_dream_launcher(
        mut self,
        launcher: Arc<dyn harw_ops::dream_run::DreamLauncher>,
    ) -> Self {
        self.dream_launcher = Some(launcher);
        self
    }

    /// Bindet den Agenten-Event-Bus der Montage.
    ///
    /// # Beschreibung
    /// Derselbe Hub, den die Montage an Sitzungen und Spawner hängt; die
    /// Fabrik legt ihn nur auf Flächen mit
    /// [`ServiceSurface::allows_agent_events`]. Ohne Aufruf fehlt der Dienst
    /// überall (Operationen melden dann keine Live-Ereignisse).
    ///
    /// # Argumente
    /// - `hub` (`Arc<AgentEventHub>`): der geteilte Bus.
    #[must_use]
    pub fn with_agent_events(mut self, hub: Arc<AgentEventHub>) -> Self {
        self.agent_events = Some(hub);
        self
    }

    /// Der gebundene Agenten-Event-Bus dieser Komposition.
    ///
    /// # Rückgabe
    /// `Some`, sobald [`Self::with_agent_events`] aufgerufen wurde.
    #[must_use]
    pub fn agent_events(&self) -> Option<&Arc<AgentEventHub>> {
        self.agent_events.as_ref()
    }

    /// Bindet den Dateisystem-Scope, den jede Operationsfläche erbt.
    ///
    /// # Beschreibung
    /// Baut zugleich den Wissensspeicher unter
    /// `harw_home::knowledge_dir(&context.profile_dir)`, sofern nicht schon
    /// über [`Self::with_knowledge_store`] einer gesetzt ist. Der Speicher
    /// legt nichts an; Verzeichnisse entstehen erst beim ersten Schreiben.
    #[must_use]
    pub fn with_home_context(mut self, context: Arc<harw_home::ResolvedHomeContext>) -> Self {
        if self.knowledge.is_none() {
            self.knowledge = Some(Arc::new(KnowledgeStore::new(&harw_home::knowledge_dir(
                &context.profile_dir,
            ))));
        }
        self.home_context = Some(context);
        self
    }

    /// Setzt den Wissensspeicher ausdrücklich (Tests, fremde Kompositionen).
    ///
    /// # Beschreibung
    /// Überschreibt einen aus [`Self::with_home_context`] abgeleiteten
    /// Speicher; ein späteres `with_home_context` ersetzt ihn nicht mehr.
    #[must_use]
    pub fn with_knowledge_store(mut self, store: Arc<KnowledgeStore>) -> Self {
        self.knowledge = Some(store);
        self
    }

    /// Der Wissensspeicher dieser Komposition.
    ///
    /// # Beschreibung
    /// Dieselbe `Arc`-Instanz, die [`Self::service_map`] auf Slash und
    /// Modell-Werkzeug legt — die Montage reicht sie an Werkzeug-Provider
    /// (`workbench.note`/`workbench.hypothesis`) weiter, statt einen zweiten
    /// Speicher zu öffnen.
    ///
    /// # Rückgabe
    /// `Some`, sobald ein Home-Kontext oder ein Speicher gebunden ist.
    #[must_use]
    pub fn knowledge_store(&self) -> Option<&Arc<KnowledgeStore>> {
        self.knowledge.as_ref()
    }

    /// Das authentifizierte Subjekt dieser Komposition.
    ///
    /// # Rückgabe
    /// Referenz auf den [`Principal`], der in jede Service-Map gelegt wird.
    #[must_use]
    pub fn principal(&self) -> &Principal {
        &self.parts.principal
    }

    /// Die Freigabemodus-Zelle dieser Komposition.
    ///
    /// # Beschreibung
    /// Alle Flächen bekommen **Klone derselben** Zelle — `/permissions set full`
    /// im Slash-Pfad gilt damit auch für das nächste Modell-Werkzeug derselben
    /// Sitzung.
    ///
    /// # Rückgabe
    /// Referenz auf die geteilte [`ApprovalModeCell`].
    #[must_use]
    pub fn approval_mode(&self) -> &ApprovalModeCell {
        &self.parts.approval_mode
    }

    /// Die geteilten Freigaberegeln dieser Komposition (Contract §2/§4).
    ///
    /// # Beschreibung
    /// Alle Flächen bekommen **Klone derselben** [`AllowRuleSet`] — eine über
    /// `/permissions` „nicht mehr fragen“ angelegte Regel gilt damit sofort
    /// auch für das nächste Modell-Werkzeug derselben Sitzung.
    ///
    /// # Rückgabe
    /// Referenz auf die geteilte [`AllowRuleSet`].
    #[must_use]
    pub fn allow_rules(&self) -> &AllowRuleSet {
        &self.parts.allow_rules
    }

    /// Die zusätzlichen Workspace-Wurzeln dieser Komposition (`/add-workdir`).
    ///
    /// # Rückgabe
    /// Referenz auf die geteilte [`ExtraRootsCell`].
    #[must_use]
    pub fn extra_roots(&self) -> &ExtraRootsCell {
        &self.parts.extra_roots
    }

    /// Die geöffnete Planungsfläche dieser Komposition.
    ///
    /// # Beschreibung
    /// Lesezugriff auf genau den Wert aus [`RuntimeServicesParts::plan`]; die
    /// Fabrik klont nichts. Einstiege, die Plan-Speicher außerhalb einer
    /// [`ServiceMap`] brauchen (etwa die TUI für ihren Planbaum), lesen sie
    /// hier, statt eine zweite Planungsfläche zu öffnen (CONTRACTS-W2d2 §1.1).
    ///
    /// # Rückgabe
    /// `Some(&PlanServices)`, wenn die Planungsfläche offen ist, sonst `None`.
    ///
    /// # Nebenläufigkeit
    /// Reiner Lesezugriff; keine Sperren.
    #[must_use]
    pub fn plan(&self) -> Option<&PlanServices> {
        self.parts.plan.as_ref()
    }

    /// Das Gedächtnis dieser Komposition.
    ///
    /// # Beschreibung
    /// Lesezugriff auf genau den Wert aus [`RuntimeServicesParts::memory`]
    /// (CONTRACTS-W2d2 §1.1). Wer ein eigenes `Arc` braucht, klont den Zeiger
    /// mit `Arc::clone`, nie das Gedächtnis selbst.
    ///
    /// # Rückgabe
    /// `Some(&Arc<dyn Memory>)`, wenn ein Gedächtnis konfiguriert ist, sonst `None`.
    ///
    /// # Nebenläufigkeit
    /// Reiner Lesezugriff; keine Sperren.
    #[must_use]
    pub fn memory(&self) -> Option<&Arc<dyn Memory>> {
        self.parts.memory.as_ref()
    }

    /// Baut die [`ServiceMap`] einer Fläche.
    ///
    /// # Beschreibung
    /// Jede Fläche bekommt dieselbe Menge, bis auf vier deklarierte
    /// Unterschiede ([`ServiceSurface::allows_spawner`],
    /// [`ServiceSurface::allows_session_controller`],
    /// [`ServiceSurface::allows_knowledge_store`],
    /// [`ServiceSurface::allows_agent_events`]):
    ///
    /// | Dienst | Slash | ModelTool | Web | Job |
    /// |---|---|---|---|---|
    /// | [`OperationRegistry`] (frisch je Aufruf) | ✓ | ✓ | ✓ | ✓ |
    /// | `Arc<dyn StateStore>` | ✓ | ✓ | ✓ | ✓ |
    /// | `Arc<ResolvedConfig>` | ✓ | ✓ | ✓ | ✓ |
    /// | [`ApprovalModeCell`] | ✓ | ✓ | ✓ | ✓ |
    /// | [`AllowRuleSet`] | ✓ | ✓ | ✓ | ✓ |
    /// | [`ExtraRootsCell`] | ✓ | ✓ | ✓ | ✓ |
    /// | [`Principal`] | ✓ | ✓ | ✓ | ✓ |
    /// | [`ProviderLoadRegistry`] | ✓ | ✓ | ✓ | ✓ |
    /// | `SharedProviderConnectionCheck` (`/provider test`) | ✓ | ✓ | ✓ | ✓ |
    /// | `Arc<dyn Memory>` (falls vorhanden) | ✓ | ✓ | ✓ | ✓ |
    /// | `Arc<JobStore>` (falls vorhanden) | ✓ | ✓ | ✓ | ✓ |
    /// | `Arc<`[`HostPermitHandles`]`>` (falls vorhanden, Plan Teil B3) | ✓ | ✓ | ✓ | ✓ |
    /// | Plan-Dienste (falls vorhanden) | ✓ | ✓ | ✓ | ✓ |
    /// | `Arc<ManagedAgentSpawner>` (falls vorhanden) | ✓ | ✓ | — | — |
    /// | [`SharedSessionController`] (falls vorhanden) | ✓ | ✓ | — | — |
    /// | `Arc<KnowledgeStore>` (falls gebunden, L6) | ✓ | ✓ | — | — |
    /// | `Arc<dyn JobTransitions>` (Speicher + `JobStore` + Freigabe-Akteur) | ✓ | ✓ | — | — |
    /// | `Arc<AgentEventHub>` (falls gebunden, `/matrix`) | ✓ | ✓ | — | — |
    /// | `Arc<dyn DreamLauncher>` (falls gebunden, `/dream run`, Plan D5) | ✓ | — | — | — |
    ///
    /// Die Zeile [`OperationRegistry`] trägt in jeder Fläche dieselbe Menge —
    /// nämlich die, die [`crate::spec::EntryProfile::operations`] dem Einstieg
    /// zuspricht (siehe [`RuntimeServicesParts::operations`]). Ein Einstieg
    /// mit [`crate::spec::OperationSurface::None`] bekommt hier überall eine
    /// leere Registry, einer mit
    /// [`crate::spec::OperationSurface::CommandsOnly`] überall dieselbe
    /// Command-Menge; die Modell-Tool-Fläche entsteht **nicht** hier, sondern
    /// als `ToolProvider` in der Extension-Registry der Montage.
    ///
    /// Drei Zeilen dieser Tabelle sind gegenüber dem Alt-Stand Korrekturen:
    /// der Spawner im Slash-Pfad (G-061, `/agent` war dort `NotAvailable`), die
    /// Plan-Dienste in Slash und Modell-Werkzeug der TUI (G-024/G-098) und der
    /// `JobStore` samt `StateStore` auf der Web-Fläche (G-060).
    ///
    /// [`AllowRuleSet`] und [`ExtraRootsCell`] sind neu (Contract §2/§4/§5,
    /// Plan Schritt 4/6): sie stehen genau wie [`ApprovalModeCell`]
    /// unbedingt auf jeder Fläche, damit `/permissions` und `/add-workdir`
    /// (beide `Surface::Command` auf der Slash-Fläche) sie über
    /// `ctx.service::<AllowRuleSet>()` bzw. `ctx.service::<ExtraRootsCell>()`
    /// finden — ohne Rücksicht darauf, ob ein Lauf überhaupt schon Regeln
    /// oder Extra-Roots gesät hat.
    ///
    /// # Argumente
    /// - `surface` ([`ServiceSurface`]): die Fläche, für die montiert wird.
    ///
    /// # Rückgabe
    /// Eine frische [`ServiceMap`]; sie teilt mit allen anderen Maps dieser
    /// Fabrik die `Arc`-Ziele und die [`ApprovalModeCell`], aber keine Map.
    ///
    /// # Panics
    /// Wenn die Vorlage aus `parts.operations` Namens- oder Alias-Kollisionen
    /// enthielte (`OperationRegistry::register`). Eine bereits gebaute Registry
    /// ist kollisionsfrei, das Umhängen ihrer `Arc`s kann daher nicht
    /// kollidieren — die Bedingung ist konstruktionsbedingt unerreichbar.
    ///
    /// # Nebenläufigkeit
    /// Klont je Aufruf nur `Arc`-Zeiger; kein geteilter veränderlicher Zustand.
    #[must_use]
    pub fn service_map(&self, surface: ServiceSurface) -> ServiceMap {
        self.assemble(surface).0
    }

    /// Die Typnamen aller Dienste, die [`Self::service_map`] auf `surface` legt.
    ///
    /// # Beschreibung
    /// Für Rechte-Snapshots und Tests. Die Liste entsteht aus derselben
    /// Montage wie die Map selbst (`insert_service`) und kann nicht von
    /// ihr abweichen. Sortiert und dedupliziert, damit ein Snapshot
    /// unabhängig von der Montagereihenfolge stabil bleibt.
    ///
    /// # Rückgabe
    /// Aufsteigend sortierte Typnamen — `std::any::type_name`-Ausgabe, also
    /// rein diagnostisch und nicht zum Parsen gedacht.
    ///
    /// # Argumente
    /// - `surface` ([`ServiceSurface`]): die Fläche.
    #[must_use]
    pub fn registered(&self, surface: ServiceSurface) -> Vec<&'static str> {
        let (_, mut names) = self.assemble(surface);
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Die gemeinsame Montage hinter [`Self::service_map`] und [`Self::registered`].
    ///
    /// # Rückgabe
    /// Die Map und das Protokoll der abgelegten Typen, in Montagereihenfolge.
    fn assemble(&self, surface: ServiceSurface) -> (ServiceMap, Vec<&'static str>) {
        let mut map = ServiceMap::new();
        let mut names = Vec::new();
        if let Some(context) = &self.home_context {
            insert_service(&mut map, &mut names, Arc::clone(context));
        }

        // Frische Registry aus Arc-Klonen: der Typ in der Map ist der konkrete
        // `OperationRegistry`, den `/help` & Co. nachschlagen.
        let mut registry = OperationRegistry::new();
        for operation in self.parts.operations.iter() {
            registry.register(Arc::clone(operation));
        }
        insert_service(&mut map, &mut names, registry);

        insert_service(&mut map, &mut names, Arc::clone(&self.parts.state_store));
        insert_service(&mut map, &mut names, Arc::clone(&self.parts.config));
        insert_service(&mut map, &mut names, self.parts.approval_mode.clone());
        insert_service(&mut map, &mut names, self.parts.allow_rules.clone());
        insert_service(&mut map, &mut names, self.parts.extra_roots.clone());
        insert_service(&mut map, &mut names, self.parts.principal.clone());
        insert_service(
            &mut map,
            &mut names,
            self.parts.provider_load_registry.clone(),
        );
        // Live-Verbindungstest für `/provider test` und `/uia-provider test`
        // (`GET /models`); `secrets:`-Schlüssel brauchen einen Resolver, den
        // diese Montage nicht hält — sie werden dann als nicht auflösbar
        // gemeldet, alle anderen Referenzen (env/file/keyring) funktionieren.
        let connection_check: harw_ops::provider::SharedProviderConnectionCheck = Arc::new(
            harw_ops::provider::DiscoveryConnectionCheck::new(harw_home::home_dir().ok(), None),
        );
        insert_service(&mut map, &mut names, connection_check);

        if let Some(memory) = &self.parts.memory {
            insert_service(&mut map, &mut names, Arc::clone(memory));
        }
        if let Some(job_store) = &self.parts.job_store {
            insert_service(&mut map, &mut names, Arc::clone(job_store));
        }
        if let Some(host_permit_handles) = &self.parts.host_permit_handles {
            insert_service(&mut map, &mut names, Arc::clone(host_permit_handles));
        }
        // Die vier deklarierten Differenzen — und nur sie.
        if let Some(spawner) = self
            .parts
            .spawner
            .as_ref()
            .filter(|_| surface.allows_spawner())
        {
            insert_service(&mut map, &mut names, Arc::clone(spawner));
        }
        if let Some(controller) = self
            .parts
            .session_controller
            .as_ref()
            .filter(|_| surface.allows_session_controller())
        {
            insert_service(&mut map, &mut names, Arc::clone(controller));
        }
        if let Some(knowledge) = self
            .knowledge
            .as_ref()
            .filter(|_| surface.allows_knowledge_store())
        {
            insert_service(&mut map, &mut names, Arc::clone(knowledge));
        }
        if let Some(ledger) = self.kanban_ledger(surface) {
            insert_service(&mut map, &mut names, ledger);
        }
        if let Some(launcher) = self
            .dream_launcher
            .as_ref()
            .filter(|_| surface == ServiceSurface::Slash)
        {
            insert_service(&mut map, &mut names, Arc::clone(launcher));
        }
        if let Some(hub) = self
            .agent_events
            .as_ref()
            .filter(|_| surface.allows_agent_events())
        {
            insert_service(&mut map, &mut names, Arc::clone(hub));
        }
        if let Some(plan) = &self.parts.plan {
            // `register_plan_services` legt genau diese vier Typen ab
            // (harw-plan-bridge/src/context_ext.rs:202-205). Der Test
            // `plan_names_match_what_the_bridge_registers` hält die Liste
            // gegen die Map gegen, damit sie nicht auseinanderlaufen kann.
            names.push(type_name::<Arc<dyn PlanStore>>());
            names.push(type_name::<Arc<dyn GoalStore>>());
            names.push(type_name::<Arc<FindingStore>>());
            names.push(type_name::<PlanToolConfig>());
            register_plan_services(
                &mut map,
                Arc::clone(&plan.plan),
                Arc::clone(&plan.goal),
                Arc::clone(&plan.findings),
                plan.plan_config.clone(),
            );
        }

        (map, names)
    }

    /// Das Kanban-Job-Ledger einer Fläche.
    ///
    /// # Beschreibung
    /// Nur auf Flächen mit Wissensspeicher ([`ServiceSurface::allows_knowledge_store`]),
    /// nur mit gebundenem Speicher, vorhandenem `JobStore` und einem
    /// Freigabe-Akteur des [`Principal`] — dieser wird `submitter` der
    /// zugelassenen Jobs und Akteur von `unblock`/`retry`/`cancel`.
    ///
    /// # Rückgabe
    /// `Some(Arc<dyn JobTransitions>)` über den geteilten `JobStore`, sonst `None`.
    pub(crate) fn kanban_ledger(&self, surface: ServiceSurface) -> Option<Arc<dyn JobTransitions>> {
        if !surface.allows_knowledge_store() || self.knowledge.is_none() {
            return None;
        }
        kanban_transitions(self.parts.job_store.as_ref(), &self.parts.principal)
    }
}

/// Baut das Kanban-Job-Ledger über `job_store` für `principal`.
///
/// # Beschreibung
/// Gemeinsamer Kern von [`RuntimeServices::kanban_ledger`] und der
/// Kind-Registry-Fabrik (lesende `kanban.*`-Werkzeuge der Kinder, Plan D2),
/// die vor den Diensten entsteht: derselbe Mandant/Workspace
/// ([`KANBAN_TENANT`]/[`KANBAN_WORKSPACE`]) und derselbe Freigabe-Akteur.
///
/// # Rückgabe
/// `None` ohne `JobStore` oder ohne Freigabe-Akteur des [`Principal`].
pub(crate) fn kanban_transitions(
    job_store: Option<&Arc<JobStore>>,
    principal: &Principal,
) -> Option<Arc<dyn JobTransitions>> {
    let job_store = job_store?;
    let actor = principal.approval_actor()?;
    let scope = JobScope::new(
        TenantId::from_str(KANBAN_TENANT),
        WorkspaceId::from_str(KANBAN_WORKSPACE),
        actor,
    );
    Some(Arc::new(JobStoreTransitions::new(
        Arc::clone(job_store),
        scope,
    )))
}

#[cfg(test)]
mod tests {
    use super::{
        PlanServices, RuntimeServices, RuntimeServicesParts, ServiceSurface, insert_service,
    };
    use crate::test_support::{TestError, TestResult};
    use harw_config::ResolvedConfig;
    use harw_core::{
        AgentEventHub, ChildLimits, InMemoryStateStore, ManagedAgentSpawner, SessionManager,
    };
    use harw_extension_api::allow_rules::AllowRuleSet;
    use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
    use harw_knowledge::KnowledgeStore;
    use harw_knowledge::kanban::lifecycle::JobTransitions;
    use harw_memory::{Entry, MaintenanceReport, Memory, MemoryResult, RecallQuery, Signal, Stats};
    use harw_operations::registry::OperationRegistry;
    use harw_operations::session_control::NullSessionController;
    use harw_operations::{ServiceMap, SharedSessionController};
    use harw_plan::{GoalStore, InMemoryGoalStore, InMemoryPlanStore, PlanStore, PlanToolConfig};
    use harw_plan_bridge::FindingStore;
    use harw_provider_http::ProviderLoadRegistry;
    use harw_sandbox::{ExtraRootsCell, HostPermitSessionRegistry, ProcessPermitLedger};
    use harw_session_store::JobStore;
    use harw_tool_shell::HostPermitHandles;
    use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};
    use std::any::type_name;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    /// Gedächtnis-Attrappe: wird nur als Typ in der Map gebraucht, nie benutzt.
    struct StubMemory;

    impl Memory for StubMemory {
        fn hot(&self) -> MemoryResult<String> {
            Ok(String::new())
        }

        fn recall<'a>(&self, _query: RecallQuery<'a>) -> MemoryResult<Vec<Entry>> {
            Ok(Vec::new())
        }

        fn record(&self, _signal: Signal) -> MemoryResult<()> {
            Ok(())
        }

        fn maintain(&self) -> MemoryResult<MaintenanceReport> {
            Ok(MaintenanceReport::default())
        }

        fn stats(&self) -> MemoryResult<Stats> {
            Ok(Stats::default())
        }
    }

    fn test_principal() -> Principal {
        Principal::trusted_ingress(
            PrincipalKind::Human,
            "w2b-04",
            IngressSurface::Tui,
            PermissionTier::Owner,
        )
    }

    fn test_spawner() -> Arc<ManagedAgentSpawner> {
        // Der Sender wird nie benutzt; der Empfänger darf sofort fallen.
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(event_tx)));
        Arc::new(ManagedAgentSpawner::new(
            manager,
            ChildLimits::conservative(),
        ))
    }

    fn test_plan_services() -> PlanServices {
        PlanServices {
            plan: Arc::new(InMemoryPlanStore::new()),
            goal: Arc::new(InMemoryGoalStore::new()),
            findings: Arc::new(FindingStore::new("/nonexistent/w2b-04/plans")),
            plan_config: PlanToolConfig::enabled_defaults(),
        }
    }

    /// Host-Permit-Handles-Attrappe (Plan Teil B3): eigener Ledger/eigene
    /// Sitzungs-Registry je Aufruf, kein Fragekanal (nie benutzt).
    fn test_host_permit_handles() -> Arc<HostPermitHandles> {
        Arc::new(HostPermitHandles {
            ledger: Arc::new(ProcessPermitLedger::default()),
            registry: Arc::new(HostPermitSessionRegistry::default()),
            prompts: None,
        })
    }

    /// Komposition mit jedem optionalen Dienst gesetzt.
    fn full_parts() -> RuntimeServicesParts {
        RuntimeServicesParts {
            operations: Arc::new(OperationRegistry::new()),
            state_store: Arc::new(InMemoryStateStore::default()),
            job_store: Some(Arc::new(JobStore::new(Path::new("/nonexistent/w2b-04")))),
            spawner: Some(test_spawner()),
            memory: Some(Arc::new(StubMemory)),
            config: Arc::new(ResolvedConfig::default()),
            plan: Some(test_plan_services()),
            approval_mode: ApprovalModeCell::new(ApprovalMode::AlwaysAsk),
            allow_rules: AllowRuleSet::new(),
            extra_roots: ExtraRootsCell::new(),
            principal: test_principal(),
            session_controller: Some(Arc::new(NullSessionController::new())),
            provider_load_registry: ProviderLoadRegistry::new(),
            host_permit_handles: Some(test_host_permit_handles()),
        }
    }

    /// Komposition ohne jeden optionalen Dienst.
    fn minimal_parts() -> RuntimeServicesParts {
        RuntimeServicesParts {
            operations: Arc::new(OperationRegistry::new()),
            state_store: Arc::new(InMemoryStateStore::default()),
            job_store: None,
            spawner: None,
            memory: None,
            config: Arc::new(ResolvedConfig::default()),
            plan: None,
            approval_mode: ApprovalModeCell::new(ApprovalMode::AlwaysAsk),
            allow_rules: AllowRuleSet::new(),
            extra_roots: ExtraRootsCell::new(),
            principal: test_principal(),
            session_controller: None,
            provider_load_registry: ProviderLoadRegistry::new(),
            host_permit_handles: None,
        }
    }

    /// Die Namen der acht immer vorhandenen Dienste.
    fn always_present() -> Vec<&'static str> {
        vec![
            type_name::<OperationRegistry>(),
            type_name::<Arc<dyn harw_core::StateStore>>(),
            type_name::<Arc<ResolvedConfig>>(),
            type_name::<ApprovalModeCell>(),
            type_name::<AllowRuleSet>(),
            type_name::<ExtraRootsCell>(),
            type_name::<Principal>(),
            type_name::<ProviderLoadRegistry>(),
            type_name::<harw_ops::provider::SharedProviderConnectionCheck>(),
        ]
    }

    fn sorted(mut names: Vec<&'static str>) -> Vec<&'static str> {
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Die erwartete Tabelle: was `full_parts()` auf `surface` ergeben MUSS.
    fn expected_full(surface: ServiceSurface) -> Vec<&'static str> {
        let mut names = always_present();
        names.push(type_name::<Arc<dyn Memory>>());
        names.push(type_name::<Arc<JobStore>>());
        names.push(type_name::<Arc<HostPermitHandles>>());
        names.push(type_name::<Arc<dyn PlanStore>>());
        names.push(type_name::<Arc<dyn GoalStore>>());
        names.push(type_name::<Arc<FindingStore>>());
        names.push(type_name::<PlanToolConfig>());
        if surface.allows_spawner() {
            names.push(type_name::<Arc<ManagedAgentSpawner>>());
        }
        if surface.allows_session_controller() {
            names.push(type_name::<SharedSessionController>());
        }
        sorted(names)
    }

    // ── Tabelle ───────────────────────────────────────────────────────────────

    #[test]
    fn every_surface_matches_the_declared_table() {
        let services = RuntimeServices::new(full_parts());
        for surface in ServiceSurface::ALL {
            assert_eq!(
                services.registered(surface),
                expected_full(surface),
                "Fläche {} weicht von der deklarierten Tabelle ab",
                surface.as_str()
            );
        }
    }

    #[test]
    fn slash_and_model_tool_register_exactly_the_same_services() {
        let services = RuntimeServices::new(full_parts());
        assert_eq!(
            services.registered(ServiceSurface::Slash),
            services.registered(ServiceSurface::ModelTool),
            "G-061: /agent als Slash-Kommando darf nicht NotAvailable sein"
        );
        assert!(
            services
                .service_map(ServiceSurface::Slash)
                .get::<Arc<ManagedAgentSpawner>>()
                .is_some(),
            "G-061: der Slash-Pfad braucht den Spawner"
        );
    }

    #[test]
    fn surfaces_differ_only_in_the_two_declared_entries() {
        let services = RuntimeServices::new(full_parts());
        let slash = services.registered(ServiceSurface::Slash);
        for surface in [ServiceSurface::Web, ServiceSurface::Job] {
            let here = services.registered(surface);
            let missing: Vec<&'static str> = slash
                .iter()
                .filter(|name| !here.contains(*name))
                .copied()
                .collect();
            assert_eq!(
                sorted(missing),
                sorted(vec![
                    type_name::<Arc<ManagedAgentSpawner>>(),
                    type_name::<SharedSessionController>(),
                ]),
                "Fläche {} darf sich nur um Spawner und Sitzungs-Controller unterscheiden",
                surface.as_str()
            );
            assert!(
                here.iter().all(|name| slash.contains(name)),
                "Fläche {} hat einen Dienst, den der Slash-Pfad nicht hat",
                surface.as_str()
            );
        }
    }

    #[test]
    fn web_and_job_receive_the_job_store_and_state_store() {
        let services = RuntimeServices::new(full_parts());
        for surface in [ServiceSurface::Web, ServiceSurface::Job] {
            let map = services.service_map(surface);
            assert!(
                map.get::<Arc<JobStore>>().is_some(),
                "G-060: {} braucht den JobStore",
                surface.as_str()
            );
            assert!(
                map.get::<Arc<dyn harw_core::StateStore>>().is_some(),
                "G-060: {} braucht den StateStore",
                surface.as_str()
            );
            assert!(
                map.get::<Arc<ManagedAgentSpawner>>().is_none(),
                "CONTRACTS.md §runtime-spec: {} hat SpawnerPolicy::None",
                surface.as_str()
            );
            assert!(
                map.get::<SharedSessionController>().is_none(),
                "{} führt keine interaktive Sitzung",
                surface.as_str()
            );
        }
    }

    // ── Plan ──────────────────────────────────────────────────────────────────

    #[test]
    fn plan_services_reach_every_surface() {
        let services = RuntimeServices::new(full_parts());
        for surface in ServiceSurface::ALL {
            let map = services.service_map(surface);
            assert!(
                map.get::<Arc<dyn PlanStore>>().is_some(),
                "G-024/G-098: {} braucht den PlanStore",
                surface.as_str()
            );
            assert!(map.get::<Arc<dyn GoalStore>>().is_some());
            assert!(map.get::<Arc<FindingStore>>().is_some());
            assert!(map.get::<PlanToolConfig>().is_some());
        }
    }

    #[test]
    fn plan_names_match_what_the_bridge_registers() {
        let services = RuntimeServices::new(full_parts());
        let names = services.registered(ServiceSurface::Slash);
        let map = services.service_map(ServiceSurface::Slash);
        // Jeder von der Fabrik gemeldete Plan-Name muss auch wirklich in der
        // Map liegen — sonst hat `register_plan_services` sich geändert.
        assert!(names.contains(&type_name::<Arc<dyn PlanStore>>()));
        assert!(map.get::<Arc<dyn PlanStore>>().is_some());
        assert!(names.contains(&type_name::<Arc<dyn GoalStore>>()));
        assert!(map.get::<Arc<dyn GoalStore>>().is_some());
        assert!(names.contains(&type_name::<Arc<FindingStore>>()));
        assert!(map.get::<Arc<FindingStore>>().is_some());
        assert!(names.contains(&type_name::<PlanToolConfig>()));
        assert!(map.get::<PlanToolConfig>().is_some());
    }

    #[test]
    fn without_plan_no_surface_registers_plan_services() {
        let services = RuntimeServices::new(minimal_parts());
        for surface in ServiceSurface::ALL {
            let map = services.service_map(surface);
            assert!(map.get::<Arc<dyn PlanStore>>().is_none());
            assert!(map.get::<PlanToolConfig>().is_none());
        }
    }

    // ── Immer vorhanden ───────────────────────────────────────────────────────

    #[test]
    fn cell_and_principal_are_present_on_every_surface() {
        for parts in [full_parts(), minimal_parts()] {
            let services = RuntimeServices::new(parts);
            for surface in ServiceSurface::ALL {
                let map = services.service_map(surface);
                assert!(
                    map.get::<ApprovalModeCell>().is_some(),
                    "W2A-05: /permissions braucht die Cell auf {}",
                    surface.as_str()
                );
                assert!(
                    map.get::<Principal>().is_some(),
                    "Principal fehlt auf {}",
                    surface.as_str()
                );
            }
        }
    }

    /// Contract §2/§4/§5: `/permissions` und `/add-workdir` müssen die
    /// Freigaberegeln bzw. Extra-Roots auf jeder Fläche finden — dieselbe
    /// Zusicherung wie [`cell_and_principal_are_present_on_every_surface`]
    /// für die [`ApprovalModeCell`].
    #[test]
    fn allow_rules_and_extra_roots_are_present_on_every_surface() {
        for parts in [full_parts(), minimal_parts()] {
            let services = RuntimeServices::new(parts);
            for surface in ServiceSurface::ALL {
                let map = services.service_map(surface);
                assert!(
                    map.get::<AllowRuleSet>().is_some(),
                    "/permissions braucht die AllowRuleSet auf {}",
                    surface.as_str()
                );
                assert!(
                    map.get::<ExtraRootsCell>().is_some(),
                    "/add-workdir braucht die ExtraRootsCell auf {}",
                    surface.as_str()
                );
            }
        }
    }

    #[test]
    fn the_cell_is_shared_across_surfaces() -> TestResult {
        let services = RuntimeServices::new(full_parts());
        let slash = services.service_map(ServiceSurface::Slash);
        let model_tool = services.service_map(ServiceSurface::ModelTool);
        let Some(cell) = slash.get::<ApprovalModeCell>() else {
            return Err(TestError::Missing("Slash-Fläche ohne ApprovalModeCell"));
        };
        cell.set(ApprovalMode::FullAccess);
        let Some(other) = model_tool.get::<ApprovalModeCell>() else {
            return Err(TestError::Missing("ModelTool-Fläche ohne ApprovalModeCell"));
        };
        assert_eq!(
            other.get(),
            ApprovalMode::FullAccess,
            "/permissions im Slash-Pfad muss für das nächste Modell-Werkzeug gelten"
        );
        assert_eq!(services.approval_mode().get(), ApprovalMode::FullAccess);
        Ok(())
    }

    /// Wie [`the_cell_is_shared_across_surfaces`], für die geteilte
    /// [`AllowRuleSet`]: eine über `/permissions` (Slash) angelegte Regel
    /// muss dem nächsten Modell-Werkzeug derselben Sitzung sofort vorliegen.
    #[test]
    fn the_allow_rule_set_is_shared_across_surfaces() -> TestResult {
        use harw_extension_api::allow_rules::{ApprovalRule, RuleDecision, RuleScope};

        let services = RuntimeServices::new(full_parts());
        let slash = services.service_map(ServiceSurface::Slash);
        let model_tool = services.service_map(ServiceSurface::ModelTool);
        let Some(rules) = slash.get::<AllowRuleSet>() else {
            return Err(TestError::Missing("Slash-Fläche ohne AllowRuleSet"));
        };
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: Some("git status".to_owned()),
            decision: RuleDecision::Allow,
            scope: RuleScope::Session,
        });
        let Some(other) = model_tool.get::<AllowRuleSet>() else {
            return Err(TestError::Missing("ModelTool-Fläche ohne AllowRuleSet"));
        };
        assert_eq!(
            other.snapshot().len(),
            1,
            "/permissions im Slash-Pfad muss für das nächste Modell-Werkzeug gelten"
        );
        assert_eq!(services.allow_rules().snapshot().len(), 1);
        Ok(())
    }

    #[test]
    fn minimal_parts_register_only_the_mandatory_eight() {
        let services = RuntimeServices::new(minimal_parts());
        for surface in ServiceSurface::ALL {
            assert_eq!(
                services.registered(surface),
                sorted(always_present()),
                "Fläche {} ohne optionale Dienste",
                surface.as_str()
            );
        }
    }

    /// [`ProviderLoadRegistry`] muss wie [`AllowRuleSet`]/[`ExtraRootsCell`]
    /// bedingungslos auf jeder Fläche liegen — auch leer (`minimal_parts`
    /// setzt sie nie), damit `ctx.service::<ProviderLoadRegistry>()` in
    /// `harw-ops` nie an einem fehlenden Dienst scheitert (siehe
    /// `harw-ops/src/status.rs`, `harw-ops/src/provider.rs`).
    #[test]
    fn provider_load_registry_is_present_on_every_surface_even_when_empty() {
        for parts in [full_parts(), minimal_parts()] {
            let services = RuntimeServices::new(parts);
            for surface in ServiceSurface::ALL {
                let map = services.service_map(surface);
                assert!(
                    map.get::<ProviderLoadRegistry>().is_some(),
                    "ProviderLoadRegistry fehlt auf {}",
                    surface.as_str()
                );
            }
        }
    }

    /// Plan Teil B3: [`HostPermitHandles`] geht — sofern in den `Parts`
    /// gesetzt — auf jede Fläche, genau wie [`Arc<JobStore>`]/`Arc<dyn
    /// Memory>` (Muster nach `provider_load_registry`, hier aber `Option`,
    /// weil nicht jede Komposition eine Host-Permit-Verdrahtung trägt).
    #[test]
    fn host_permit_handles_present_on_every_surface_when_set() {
        let services = RuntimeServices::new(full_parts());
        for surface in ServiceSurface::ALL {
            let map = services.service_map(surface);
            assert!(
                map.get::<Arc<HostPermitHandles>>().is_some(),
                "HostPermitHandles fehlt auf {} (Plan Teil B3)",
                surface.as_str()
            );
        }
    }

    /// Ohne gesetzte Handles (`minimal_parts`) darf keine Fläche einen
    /// `HostPermitHandles`-Eintrag erfinden.
    #[test]
    fn host_permit_handles_absent_on_every_surface_without_parts() {
        let services = RuntimeServices::new(minimal_parts());
        for surface in ServiceSurface::ALL {
            let map = services.service_map(surface);
            assert!(
                map.get::<Arc<HostPermitHandles>>().is_none(),
                "HostPermitHandles darf ohne Parts-Eintrag nicht auf {} erscheinen",
                surface.as_str()
            );
        }
    }

    /// Dieselbe [`Arc`]-Instanz erreicht jede Fläche — kein Klon des inneren
    /// Ledgers/der Sitzungs-Registry (Plan Teil B3: nur der Zeiger wird
    /// geklont, siehe [`RuntimeServices::service_map`]-Doku).
    #[test]
    fn host_permit_handles_share_the_same_ledger_arc_across_surfaces() -> TestResult {
        let handles = test_host_permit_handles();
        let mut parts = full_parts();
        parts.host_permit_handles = Some(Arc::clone(&handles));
        let services = RuntimeServices::new(parts);

        let slash = services.service_map(ServiceSurface::Slash);
        let web = services.service_map(ServiceSurface::Web);
        let (Some(via_slash), Some(via_web)) = (
            slash.get::<Arc<HostPermitHandles>>(),
            web.get::<Arc<HostPermitHandles>>(),
        ) else {
            return Err(TestError::Missing(
                "beide Flächen brauchen HostPermitHandles",
            ));
        };
        assert!(Arc::ptr_eq(&via_slash.ledger, &handles.ledger));
        assert!(Arc::ptr_eq(&via_web.ledger, &handles.ledger));
        Ok(())
    }

    // ── Abgrenzung ────────────────────────────────────────────────────────────

    /// L6: ohne gebundenen Home-Kontext/Speicher erfindet keine Fläche einen
    /// Wissensspeicher.
    #[test]
    fn without_a_bound_store_no_surface_registers_a_knowledge_store() {
        let services = RuntimeServices::new(full_parts());
        assert!(services.knowledge_store().is_none());
        for surface in ServiceSurface::ALL {
            assert!(
                !services
                    .registered(surface)
                    .iter()
                    .any(|name| name.contains("KnowledgeStore")),
                "{} darf ohne gebundenen Speicher keinen KnowledgeStore tragen",
                surface.as_str()
            );
        }
    }

    /// Plan D5: der Traum-Starter liegt nur auf der Slash-Fläche.
    #[test]
    fn dream_launcher_reaches_the_slash_surface_only() {
        struct NeverLauncher;
        impl harw_ops::dream_run::DreamLauncher for NeverLauncher {
            fn launch(
                &self,
                _trigger: harw_ops::dream_run::DreamTrigger,
            ) -> harw_ops::dream_run::DreamFuture<
                '_,
                Result<harw_ops::dream_run::DreamRunOutcome, harw_ops::dream_run::DreamRunError>,
            > {
                Box::pin(async { Err(harw_ops::dream_run::DreamRunError::Busy) })
            }
        }
        let launcher: Arc<dyn harw_ops::dream_run::DreamLauncher> = Arc::new(NeverLauncher);
        let without = RuntimeServices::new(full_parts());
        let with = RuntimeServices::new(full_parts()).with_dream_launcher(launcher);
        for surface in ServiceSurface::ALL {
            assert!(
                without
                    .service_map(surface)
                    .get::<Arc<dyn harw_ops::dream_run::DreamLauncher>>()
                    .is_none()
            );
            assert_eq!(
                with.service_map(surface)
                    .get::<Arc<dyn harw_ops::dream_run::DreamLauncher>>()
                    .is_some(),
                surface == ServiceSurface::Slash,
                "{}",
                surface.as_str()
            );
        }
    }

    /// L6 umgedreht: ein gebundener Speicher liegt auf Slash und
    /// Modell-Werkzeug — dieselbe `Arc`-Instanz —, nicht auf Web und Job.
    #[test]
    fn knowledge_store_reaches_slash_and_model_tool_only() -> TestResult {
        let store = Arc::new(KnowledgeStore::new(Path::new("/nonexistent/l6/knowledge")));
        let services = RuntimeServices::new(full_parts()).with_knowledge_store(Arc::clone(&store));
        for surface in ServiceSurface::ALL {
            let map = services.service_map(surface);
            let found = map.get::<Arc<KnowledgeStore>>();
            if surface.allows_knowledge_store() {
                let Some(found) = found else {
                    return Err(TestError::Missing("Slash/ModelTool ohne KnowledgeStore"));
                };
                assert!(Arc::ptr_eq(found, &store), "{}", surface.as_str());
                assert!(
                    services
                        .registered(surface)
                        .contains(&type_name::<Arc<KnowledgeStore>>())
                );
            } else {
                assert!(
                    found.is_none(),
                    "{} darf keinen KnowledgeStore tragen",
                    surface.as_str()
                );
            }
        }
        assert_eq!(
            services.registered(ServiceSurface::Slash),
            services.registered(ServiceSurface::ModelTool)
        );
        Ok(())
    }

    /// Ohne [`RuntimeServices::with_agent_events`] trägt keine Fläche einen Hub.
    #[test]
    fn without_a_bound_hub_no_surface_registers_agent_events() {
        let services = RuntimeServices::new(full_parts());
        assert!(services.agent_events().is_none());
        for surface in ServiceSurface::ALL {
            assert!(
                services
                    .service_map(surface)
                    .get::<Arc<AgentEventHub>>()
                    .is_none(),
                "{} darf ohne gebundenen Hub keinen AgentEventHub tragen",
                surface.as_str()
            );
        }
    }

    /// Ein gebundener Hub liegt auf Slash und Modell-Werkzeug — dieselbe
    /// `Arc`-Instanz —, nicht auf Web und Job.
    #[test]
    fn agent_events_reach_slash_and_model_tool_only() -> TestResult {
        let hub = Arc::new(AgentEventHub::new(4));
        let services = RuntimeServices::new(full_parts()).with_agent_events(Arc::clone(&hub));
        for surface in ServiceSurface::ALL {
            let map = services.service_map(surface);
            let found = map.get::<Arc<AgentEventHub>>();
            if surface.allows_agent_events() {
                let Some(found) = found else {
                    return Err(TestError::Missing("Slash/ModelTool ohne AgentEventHub"));
                };
                assert!(Arc::ptr_eq(found, &hub), "{}", surface.as_str());
                assert!(
                    services
                        .registered(surface)
                        .contains(&type_name::<Arc<AgentEventHub>>())
                );
            } else {
                assert!(
                    found.is_none(),
                    "{} darf keinen AgentEventHub tragen",
                    surface.as_str()
                );
            }
        }
        assert_eq!(
            services.registered(ServiceSurface::Slash),
            services.registered(ServiceSurface::ModelTool)
        );
        let slash = services.registered(ServiceSurface::Slash);
        for surface in [ServiceSurface::Web, ServiceSurface::Job] {
            let here = services.registered(surface);
            let missing: Vec<&'static str> = slash
                .iter()
                .filter(|name| !here.contains(*name))
                .copied()
                .collect();
            assert_eq!(
                sorted(missing),
                sorted(vec![
                    type_name::<Arc<ManagedAgentSpawner>>(),
                    type_name::<SharedSessionController>(),
                    type_name::<Arc<AgentEventHub>>(),
                ]),
                "Fläche {}",
                surface.as_str()
            );
        }
        Ok(())
    }

    /// Mit Speicher (und `JobStore` samt Freigabe-Akteur) unterscheiden sich
    /// Web/Job vom Slash-Pfad um genau die drei deklarierten Einträge plus das
    /// davon abgeleitete Kanban-Ledger.
    #[test]
    fn with_a_store_surfaces_differ_only_in_the_three_declared_entries() {
        let store = Arc::new(KnowledgeStore::new(Path::new("/nonexistent/l6/knowledge")));
        let services = RuntimeServices::new(full_parts()).with_knowledge_store(store);
        let slash = services.registered(ServiceSurface::Slash);
        for surface in [ServiceSurface::Web, ServiceSurface::Job] {
            let here = services.registered(surface);
            let missing: Vec<&'static str> = slash
                .iter()
                .filter(|name| !here.contains(*name))
                .copied()
                .collect();
            assert_eq!(
                sorted(missing),
                sorted(vec![
                    type_name::<Arc<ManagedAgentSpawner>>(),
                    type_name::<SharedSessionController>(),
                    type_name::<Arc<KnowledgeStore>>(),
                    type_name::<Arc<dyn JobTransitions>>(),
                ]),
                "Fläche {}",
                surface.as_str()
            );
        }
    }

    /// Kanban-Ledger: mit Speicher, `JobStore` und Freigabe-Akteur liegt
    /// `Arc<dyn JobTransitions>` auf genau den Flächen des Wissensspeichers.
    #[test]
    fn job_transitions_follow_the_knowledge_store_surfaces() {
        let store = Arc::new(KnowledgeStore::new(Path::new("/nonexistent/l6/knowledge")));
        let services = RuntimeServices::new(full_parts()).with_knowledge_store(store);
        for surface in ServiceSurface::ALL {
            let map = services.service_map(surface);
            assert_eq!(
                map.get::<Arc<dyn JobTransitions>>().is_some(),
                surface.allows_knowledge_store(),
                "Kanban-Ledger auf {}",
                surface.as_str()
            );
            assert_eq!(
                services
                    .registered(surface)
                    .contains(&type_name::<Arc<dyn JobTransitions>>()),
                surface.allows_knowledge_store(),
                "{}",
                surface.as_str()
            );
        }
    }

    /// Ohne `JobStore`, ohne Wissensspeicher oder ohne Freigabe-Akteur gibt
    /// es kein Kanban-Ledger (fail closed).
    #[test]
    fn job_transitions_need_store_job_store_and_an_approval_actor() {
        let knowledge = || Arc::new(KnowledgeStore::new(Path::new("/nonexistent/l6/knowledge")));

        let without_knowledge = RuntimeServices::new(full_parts());
        let mut no_job_store = full_parts();
        no_job_store.job_store = None;
        let without_job_store =
            RuntimeServices::new(no_job_store).with_knowledge_store(knowledge());
        let mut model_principal = full_parts();
        model_principal.principal = Principal::trusted_ingress(
            PrincipalKind::Model,
            "w2b-04-model",
            IngressSurface::Tui,
            PermissionTier::Owner,
        );
        let without_actor = RuntimeServices::new(model_principal).with_knowledge_store(knowledge());

        for services in [without_knowledge, without_job_store, without_actor] {
            for surface in ServiceSurface::ALL {
                assert!(
                    services
                        .service_map(surface)
                        .get::<Arc<dyn JobTransitions>>()
                        .is_none(),
                    "{} darf ohne alle Zutaten kein Kanban-Ledger tragen",
                    surface.as_str()
                );
            }
        }
    }

    /// Der Speicher liegt unter `harw_home::knowledge_dir(profile_dir)` des
    /// gebundenen Home-Kontexts; ein ausdrücklich gesetzter Speicher gewinnt.
    #[test]
    fn home_context_derives_the_store_from_the_profile_dir() -> TestResult {
        let home = tempfile::tempdir().map_err(|error| TestError::Unexpected(error.to_string()))?;
        let project_dir = home.path().join("project");
        let project = harw_home::ProjectRoot {
            root: project_dir.clone(),
            trust_key: project_dir,
            kind: harw_home::ProjectKind::Directory,
        };
        let context = Arc::new(
            harw_home::ResolvedHomeContext::new(home.path(), "default".to_owned(), project)
                .map_err(|error| TestError::Unexpected(error.to_string()))?,
        );
        let services =
            RuntimeServices::new(minimal_parts()).with_home_context(Arc::clone(&context));
        let Some(store) = services.knowledge_store() else {
            return Err(TestError::Missing(
                "with_home_context muss den Speicher binden",
            ));
        };
        assert_eq!(
            store.root(),
            harw_home::knowledge_dir(&context.profile_dir).as_path()
        );

        let explicit = Arc::new(KnowledgeStore::new(Path::new("/nonexistent/explicit")));
        let services = RuntimeServices::new(minimal_parts())
            .with_knowledge_store(Arc::clone(&explicit))
            .with_home_context(context);
        let Some(kept) = services.knowledge_store() else {
            return Err(TestError::Missing("ausdrücklicher Speicher fehlt"));
        };
        assert!(Arc::ptr_eq(kept, &explicit));
        Ok(())
    }

    #[test]
    fn registered_is_sorted_deduplicated_and_map_sized() {
        let services = RuntimeServices::new(full_parts());
        let names = services.registered(ServiceSurface::Slash);
        assert_eq!(names, sorted(names.clone()), "Snapshot muss stabil sein");
        assert_eq!(
            names.len(),
            expected_full(ServiceSurface::Slash).len(),
            "kein Dienst doppelt gezählt"
        );
    }

    #[test]
    fn insert_service_records_exactly_the_inserted_type() {
        let mut map = ServiceMap::new();
        let mut names = Vec::new();
        insert_service(&mut map, &mut names, 7_u32);
        assert_eq!(names, vec![type_name::<u32>()]);
        assert_eq!(map.get::<u32>().copied(), Some(7));
    }

    #[test]
    fn the_registry_of_two_surfaces_is_not_the_same_instance() -> TestResult {
        let services = RuntimeServices::new(full_parts());
        let slash = services.service_map(ServiceSurface::Slash);
        let web = services.service_map(ServiceSurface::Web);
        let (Some(first), Some(second)) = (
            slash.get::<OperationRegistry>(),
            web.get::<OperationRegistry>(),
        ) else {
            return Err(TestError::Missing(
                "beide Flächen brauchen eine OperationRegistry",
            ));
        };
        assert!(
            !std::ptr::eq(first, second),
            "jede Fläche bekommt ihre eigene Registry-Instanz"
        );
        assert_eq!(first.len(), second.len());
        Ok(())
    }

    #[test]
    fn principal_survives_the_round_trip() -> TestResult {
        let services = RuntimeServices::new(full_parts());
        let map = services.service_map(ServiceSurface::Job);
        let Some(principal) = map.get::<Principal>() else {
            return Err(TestError::Missing("Job-Fläche ohne Principal"));
        };
        assert_eq!(principal, services.principal());
        assert_eq!(principal.id(), "w2b-04");
        Ok(())
    }

    // ── Accessoren (CONTRACTS-W2d2 §1.1) ─────────────────────────────────────

    #[test]
    fn test_plan_returns_the_parts_plan_services() -> TestResult {
        let parts = full_parts();
        let expected_findings = parts.plan.as_ref().map(|plan| Arc::clone(&plan.findings));
        let services = RuntimeServices::new(parts);
        let (Some(plan), Some(expected)) = (services.plan(), expected_findings) else {
            return Err(TestError::Missing(
                "full_parts() setzt Plan-Dienste, plan() muss sie liefern",
            ));
        };
        assert!(
            Arc::ptr_eq(&plan.findings, &expected),
            "plan() liefert genau den übergebenen Wert, keine Kopie"
        );
        assert!(RuntimeServices::new(minimal_parts()).plan().is_none());
        Ok(())
    }

    #[test]
    fn test_memory_none_without_memory() {
        assert!(RuntimeServices::new(minimal_parts()).memory().is_none());
        assert!(RuntimeServices::new(full_parts()).memory().is_some());
    }
}
