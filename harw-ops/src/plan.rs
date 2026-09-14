//! `/plan` — Plan-Operation über `harw-plan` und `harw-plan-bridge`.
//!
//! # Verantwortungsbereich
//! Exponiert den in [`OpContext`] registrierten `PlanStore` als `/plan`-Command
//! (`channel_parity`) **und** als Modell-Tool. Die Operation besitzt selbst
//! keinen Zustand: jede Mutation läuft über die einzige Mutationsstelle
//! [`PlanStore::apply`], jede Auswertung über die reinen Funktionen in
//! [`harw_plan::graph`] bzw. [`PlanController`].
//!
//! # Schlüsseltypen
//! - [`PlanArgs`] — Subcommand-Enum; parst sowohl rohe `/plan`-Tokens
//!   ([`harw_operations::FromRawArgs`]) als auch das JSON der Modell-Tool-Fläche
//!   (`serde`) und liefert über [`harw_macros::OpArgs`] deren JSON-Schema.
//! - [`CallSurface`] — aus welcher Fläche der Aufruf kam (siehe unten).
//! - [`PlanCall`] — Hülle aus [`CallSurface`] + [`PlanArgs`]; der eigentliche
//!   Argumenttyp der Operation.
//! - `PlanOperation` — vom `#[operation]`-Makro erzeugt.
//!
//! # Warum es eine Hülle gibt: die Aufruf-Fläche ist Autoritätsinformation
//! [`harw_operations::OpInvocation`] unterscheidet `Command`, `ModelTool` und
//! `AgentTool` — die Information *existiert* also. Das `#[operation]`-Makro
//! reicht die [`harw_operations::OpInput`] aber **nicht** an den Rumpf durch
//! (es akzeptiert exakt `(ctx: &OpContext, args: ArgsType)`, siehe
//! `harw-macros/src/operation.rs`). Der Rumpf kann `input.invocation` deshalb
//! nicht direkt lesen.
//!
//! Er kann die Fläche aber **verlässlich** aus dem Parse-Pfad ableiten, denn
//! das Makro dispatcht genau auf `OpInvocation`:
//!
//! | `OpInvocation` | Parse-Pfad im generierten `run` | ⇒ [`CallSurface`] |
//! |---|---|---|
//! | `Command { args }` | `FromRawArgs::from_raw_args(args)` — **immer**, auch bei leerem Slice | [`CallSurface::Command`] |
//! | `ModelTool { args }` / `AgentTool { args }` | `serde_json::from_value` bzw. `Default::default()` bei `Null` | [`CallSurface::Model`] |
//!
//! [`PlanCall`] implementiert deshalb `FromRawArgs` (⇒ `Command`), `Deserialize`
//! (⇒ `Model`) und `Default` (⇒ `Model`, weil `Default` ausschließlich im
//! Tool-Zweig erreicht wird). Das ist keine Heuristik über Feldinhalte, sondern
//! die Umkehrung des Makro-Dispatchs.
//!
//! # Akteur (W4a/A-OPSPLAN, F-049, G-031)
//! Der `actor` jeder Plan-/Goal-Mutation entsteht **ausschließlich** aus dem
//! an der Eingangsgrenze authentifizierten [`Principal`] im [`OpContext`]
//! (`ctx.service::<Principal>()`, siehe [`require_principal`]) zusammen mit der
//! Aufruf-Fläche ([`CallSurface::actor_for_principal`]):
//!
//! | Fläche | `Principal::kind()` | Akteur |
//! |---|---|---|
//! | [`CallSurface::Command`] | `Human` | `human:<principal-id>@<session>` |
//! | [`CallSurface::Command`] | `Model`/`Operation`/`Channel` | `model:<kind>/<principal-id>@<session>` |
//! | [`CallSurface::Model`] | beliebig | `model:<kind>/<principal-id>@<session>` |
//!
//! `human:` gibt es also nur, wenn **beide** Quellen übereinstimmen: ein
//! menschlicher Principal **und** die Command-Fläche. Jede andere Kombination
//! bekommt fail-closed das Präfix `model:`, an dem
//! `harw_plan::goal::validate_goal_action` `Achieved`/`Abandoned` sperrt. Fehlt
//! der Principal, endet die Operation mit [`OpError::NotAvailable`] — es gibt
//! **keinen** Default-Akteur mehr (vorher `human:<session>`/`model:<session>`
//! allein aus dem Parse-Pfad).
//!
//! Die Fläche selbst stammt weiterhin aus dem Parse-Pfad des Makros (Tabelle
//! oben); das bleibt eine Makro-Eigenschaft, die ein Principal allein nicht
//! ersetzen kann, weil die Runtime auf Slash- und ModelTool-Fläche denselben
//! Principal ablegt (`harw-runtime/src/services.rs`, Tabelle in
//! `service_map`). Der Principal verhindert aber, dass eine falsch erkannte
//! Fläche einem Nicht-Menschen `human:` verschafft. Der generische
//! [`SurfaceCall`] trägt dieselbe Ableitung für `explore`, `research_*` und
//! `analyze`.
//!
//! # Approval-Politik
//! Deklariert ist `model_tool(approval = "always")`. Weil `plan` nicht in
//! `harw_registry_defaults::AUTO_APPROVED_TOOLS` steht (W1-05), liefert die
//! `DefaultApprovalPolicy` für jeden Modell-Aufruf im Modus `Delegated`
//! `ApprovalDecision::AskUser` — geprüft in
//! `harw-ops/tests/plan_authority.rs`. **Gewollt** wäre
//! [`harw_operations::ApprovalPolicy::RequireForScope`]: die Lesezugriffe
//! `inspect`, `ready`, `waves` und `reconcile` verändern den Plan nicht (auch
//! `reconcile` wendet nur Runtime-Schritte an und gibt jede Entscheidung als
//! Vorschlag zurück) und bräuchten keine Freigabe, während jede Mutation
//! (`create`, `add`, `patch`, `dep`, `status`, `evidence`, `expand`,
//! `condense`, `reopen`, `supersede`, `bind-goal`) eine braucht.
//!
//! Das `#[operation]`-Makro kann das derzeit nicht ausdrücken: `map_approval`
//! in `harw-macros/src/operation.rs` akzeptiert ausschließlich `"none"` und
//! `"always"`. Deshalb steht hier `"always"` — auch lesende Subcommands fragen
//! auf der Modell-Fläche nach.
//!
//! **Nötige Erweiterung** (eine Zeile pro Variante in `map_approval`):
//! ```text
//! "require_for_scope"      => ApprovalPolicy::RequireForScope,
//! "require_for_effect"     => ApprovalPolicy::RequireForEffect,
//! "require_for_risk_class" => ApprovalPolicy::RequireForRiskClass,
//! ```
//! Danach kann diese Operation auf `approval = "require_for_scope"` wechseln,
//! ohne dass sich am Rumpf etwas ändert.
//!
//! # Gate
//! Vor jedem Subcommand wird [`PlanToolConfig::require_enabled`] geprüft. Fehlt
//! die Konfiguration oder ist `[tools.plan] enabled = false`, antwortet die
//! Operation mit [`OpError::NotAvailable`] — ein deaktiviertes Werkzeug soll
//! nicht durch die Hintertür eines Model-Tool-Calls wiederauferstehen.
//!
//! # Nebenläufigkeit
//! `PlanOperation` ist ein Unit-Struct ohne inneren Zustand (`Send + Sync`).
//! Die Serialisierung der Mutationen liegt beim `PlanStore` (`RwLock`).
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein Plan-Store registriert, Tool deaktiviert
//!   oder kein [`Principal`] im Kontext.
//! - [`OpError::InvalidArguments`] — Argumentgrammatik verletzt, unbekannte
//!   Knotenart/Status/Nachweisart, Plan-ID verletzt die `PlanId`-Grammatik.
//! - [`OpError::Execution`] — der Plan-Store oder die Bridge lehnt ab.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::plan::{CallSurface, PlanArgs, PlanCall};
//!
//! let call = PlanCall::from_command(PlanArgs::Ready);
//! assert_eq!(call.surface, CallSurface::Command);
//! ```

use harw_macros::operation;
use harw_operations::require_service;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_plan::actions::{NodePatch, PlanAction, PlanEvent};
use harw_plan::error::PlanError;
// Kein `use harw_plan::goal::GoalStore`: `run_reconcile` arbeitet auf einem
// `Arc<dyn GoalStore>` aus `ctx.goal_store()`. Bei `dyn Trait` ist der Trait Teil
// des Typs und die Methode wird über die Vtable aufgelöst — ein Import wäre
// redundant (nur konkrete Typen brauchen den Trait im Scope).
use harw_plan::graph;
use harw_plan::ids::{PathOrSymbol, PlanId, RevisionId, TaskId};
use harw_plan::types::{EvidenceKind, EvidenceRef, Plan, PlanNode, PlanNodeKind, PlanNodeStatus};
use harw_plan::{PlanStore, PlanToolConfig};
use harw_plan_bridge::{
    OpContextPlanExt, PlanBridgeError, PlanController, ReconcileInput, ReconcileStep,
};
use harw_types::{Principal, PrincipalKind, SessionId};
use time::OffsetDateTime;

// ── Aufruf-Fläche ────────────────────────────────────────────────────────────

/// Die Fläche, über die eine Operation aufgerufen wurde.
///
/// # Beschreibung
/// Verdichtung von [`harw_operations::OpInvocation`] auf die einzige
/// Unterscheidung, die für die Autoritätsgrenze zählt: hat ein **Mensch** über
/// eine Command-Zeile gehandelt, oder ein **Modell** über eine Tool-Fläche?
/// `AgentTool` zählt bewusst zu [`Self::Model`] — ein Child-Agent ist ein
/// Modell-Akteur, kein menschlicher.
///
/// Wie der Wert zustande kommt, steht im Modulkopf ("Warum es eine Hülle gibt").
///
/// # Nebenläufigkeit
/// `Copy` + `Send + Sync`; reiner Werttyp ohne inneren Zustand.
///
/// # Beispiel
/// ```rust
/// use harw_ops::plan::CallSurface;
///
/// assert_eq!(CallSurface::Command.actor_prefix(), "human");
/// assert_eq!(CallSurface::Model.actor_prefix(), "model");
/// assert!(CallSurface::Model.is_model());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallSurface {
    /// Aufruf aus einer `/command`-Zeile — ein menschlicher Akteur bedient die
    /// Runtime (TUI oder Channel mit `channel_parity`).
    Command,
    /// Aufruf über die Modell-Tool- oder Agent-Tool-Fläche — ein Modell schlägt
    /// vor, entscheidet aber nicht.
    Model,
}

/// Fehlertext, wenn der [`OpContext`] keinen [`Principal`] trägt.
///
/// Öffentlich, damit Tests und Aufrufer den Fall eindeutig erkennen, statt
/// ihn mit „kein Plan-Store" zu verwechseln.
pub const MISSING_PRINCIPAL: &str = "kein authentifizierter Principal im Kontext; \
     Plan-, Ziel- und Recherche-Mutationen brauchen einen an der Eingangsgrenze \
     ermittelten Aufrufer (kein Default-Akteur)";

impl CallSurface {
    /// Gibt das Flächen-Präfix zurück (ohne Principal).
    ///
    /// # Beschreibung
    /// `"human"` für [`Self::Command`], `"model"` für [`Self::Model`]. Das ist
    /// nur die **Obergrenze** dieser Fläche; der tatsächliche Akteur entsteht
    /// in [`Self::actor_for_principal`] und bekommt `human` nur zusätzlich mit
    /// einem menschlichen Principal.
    ///
    /// # Rückgabe
    /// Ein statischer String ohne Doppelpunkt.
    #[must_use]
    pub fn actor_prefix(self) -> &'static str {
        match self {
            Self::Command => "human",
            Self::Model => "model",
        }
    }

    /// Baut den Akteur-Bezeichner aus Principal, Fläche und Session.
    ///
    /// # Description
    /// `human:<id>@<session>` genau dann, wenn die Fläche [`Self::Command`]
    /// **und** `principal.kind()` [`PrincipalKind::Human`] ist; sonst
    /// `model:<kind>/<id>@<session>`. Das Präfix `model:` ist der Schalter, an
    /// dem `harw_plan::goal::validate_goal_action` `Achieved`/`Abandoned`
    /// verweigert — ein Nicht-Mensch kann es über keine Fläche verlieren.
    ///
    /// # Arguments
    /// - `principal` (`&Principal`): der authentifizierte Aufrufer aus dem
    ///   [`OpContext`], nie aus Modell-Argumenten.
    /// - `session` (`&SessionId`): Session des Kontexts (Audit-Zuordnung).
    ///
    /// # Returns
    /// Der Akteur als `String`.
    ///
    /// # Concurrency
    /// Reine Funktion.
    ///
    /// # Examples
    /// ```rust
    /// use harw_ops::plan::CallSurface;
    /// use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind, SessionId};
    ///
    /// let human = Principal::trusted_ingress(
    ///     PrincipalKind::Human,
    ///     "uid:1000",
    ///     IngressSurface::Cli,
    ///     PermissionTier::Operator,
    /// );
    /// let session = SessionId::new();
    /// assert!(CallSurface::Command.actor_for_principal(&human, &session).starts_with("human:"));
    /// assert!(CallSurface::Model.actor_for_principal(&human, &session).starts_with("model:"));
    /// ```
    #[must_use]
    pub fn actor_for_principal(self, principal: &Principal, session: &SessionId) -> String {
        match (self, principal.kind()) {
            (Self::Command, PrincipalKind::Human) => {
                format!("human:{}@{session}", principal.id())
            }
            (Self::Command | Self::Model, kind) => {
                format!("model:{}/{}@{session}", principal_kind_label(kind), principal.id())
            }
        }
    }

    /// `true`, wenn der Aufruf von einem Modell (Model-Tool oder Agent-Tool) kam.
    #[must_use]
    pub fn is_model(self) -> bool {
        matches!(self, Self::Model)
    }
}

/// Kleinschreibiges Etikett einer [`PrincipalKind`] für Akteur-Bezeichner.
///
/// Heißt bewusst nicht `kind_label`: weiter unten gibt es dasselbe Etikett für
/// [`PlanNodeKind`], und zwei gleichnamige Funktionen im selben Modul sind ein
/// Fehler, kein Überladen.
fn principal_kind_label(kind: PrincipalKind) -> &'static str {
    match kind {
        PrincipalKind::Human => "human",
        PrincipalKind::Model => "model",
        PrincipalKind::Operation => "operation",
        PrincipalKind::Channel => "channel",
    }
}

/// Liest den authentifizierten [`Principal`] aus dem Kontext.
///
/// # Description
/// Die Runtime legt den Principal als Dienst in jede Service-Map
/// (`harw-runtime/src/services.rs`, `insert_service(.., principal.clone())`;
/// Web überschreibt ihn je Peer in `harw-cli/src/web.rs`). Diese Funktion ist
/// der einzige Zugriffspunkt der Planungsfläche — fehlt er, gibt es keinen
/// Rückfall.
///
/// # Arguments
/// - `ctx` (`&OpContext`): Ausführungskontext.
///
/// # Returns
/// Referenz auf den Principal.
///
/// # Errors
/// - [`OpError::NotAvailable`] mit [`MISSING_PRINCIPAL`]: kein Principal im Kontext.
///
/// # Concurrency
/// Reiner Lesezugriff auf die unveränderliche Service-Map.
///
/// # Examples
/// ```rust,no_run
/// # fn run(ctx: &harw_operations::OpContext) -> Result<(), harw_operations::OpError> {
/// let principal = harw_ops::plan::require_principal(ctx)?;
/// # let _ = principal;
/// # Ok(())
/// # }
/// ```
pub fn require_principal(ctx: &OpContext) -> Result<&Principal, OpError> {
    ctx.service::<Principal>()
        .ok_or_else(|| OpError::NotAvailable(MISSING_PRINCIPAL.to_owned()))
}

/// Liest den Principal und bildet den Akteur für eine Fläche.
///
/// # Errors
/// - [`OpError::NotAvailable`]: kein Principal im Kontext.
pub(crate) fn require_actor(ctx: &OpContext, surface: CallSurface) -> Result<String, OpError> {
    let principal = require_principal(ctx)?;
    Ok(surface.actor_for_principal(principal, ctx.session_id()))
}

/// Parst eine vom Aufrufer gelieferte Plan-ID gegen die `PlanId`-Grammatik.
///
/// # Description
/// Die Fehlermeldung wiederholt den Rohwert bewusst **nicht** und nennt keinen
/// Speicherpfad: eine Traversal-Eingabe (`../..`) soll weder im Chat-Verlauf
/// noch im Log als Pfad erscheinen.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: Grammatik verletzt.
pub(crate) fn parse_plan_id(raw: &str) -> Result<PlanId, OpError> {
    PlanId::parse(raw.trim()).map_err(|_| {
        OpError::InvalidArguments(
            "ungültige Plan-ID: erlaubt sind 1 bis 64 Zeichen aus a-z, 0-9 und '-', \
             beginnend mit einem Buchstaben oder einer Ziffer"
                .to_owned(),
        )
    })
}

/// Argument-Hülle mit Aufruf-Fläche für Operationen ohne eigene Hülle.
///
/// # Description
/// Dieselbe Ableitung wie [`PlanCall`], generisch für `explore`, `research_*`
/// und `analyze`: [`harw_operations::FromRawArgs`] ⇒ [`CallSurface::Command`],
/// `Deserialize` und `Default` ⇒ [`CallSurface::Model`]. Das Schema der
/// Modell-Fläche ist das von `A`.
///
/// # Concurrency
/// `Send + Sync`, wenn `A` es ist.
///
/// # Examples
/// ```rust
/// use harw_ops::plan::{CallSurface, SurfaceCall};
///
/// let call = SurfaceCall::from_model(harw_ops::explore::ExploreArgs::default());
/// assert_eq!(call.surface, CallSurface::Model);
/// ```
#[derive(Debug)]
pub struct SurfaceCall<A> {
    /// Fläche, über die der Aufruf kam.
    pub surface: CallSurface,
    /// Die eigentlichen Argumente.
    pub args: A,
}

impl<A> SurfaceCall<A> {
    /// Baut einen Aufruf der Command-Fläche.
    #[must_use]
    pub fn from_command(args: A) -> Self {
        Self {
            surface: CallSurface::Command,
            args,
        }
    }

    /// Baut einen Aufruf der Modell-Tool-Fläche.
    #[must_use]
    pub fn from_model(args: A) -> Self {
        Self {
            surface: CallSurface::Model,
            args,
        }
    }
}

impl<A: Default> Default for SurfaceCall<A> {
    /// Nur im Tool-Zweig des Makros erreicht (`Null`-Argumente) ⇒ Modell.
    fn default() -> Self {
        Self::from_model(A::default())
    }
}

impl<'de, A: serde::Deserialize<'de>> serde::Deserialize<'de> for SurfaceCall<A> {
    /// Serde-Pfad: nur `ModelTool`/`AgentTool` ⇒ Modell.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        A::deserialize(deserializer).map(Self::from_model)
    }
}

impl<A: harw_operations::FromRawArgs> harw_operations::FromRawArgs for SurfaceCall<A> {
    /// Command-Pfad ⇒ Command-Fläche.
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        A::from_raw_args(tokens).map(Self::from_command)
    }
}

impl<A: harw_operations::OpArgsSchema> harw_operations::OpArgsSchema for SurfaceCall<A> {
    /// Reicht das Schema von `A` durch; die Hülle ist auf der Modell-Fläche unsichtbar.
    fn json_schema() -> harw_tools::JsonSchema {
        A::json_schema()
    }
}

// ── Argumente ────────────────────────────────────────────────────────────────

/// Subcommands der `/plan`-Operation.
///
/// # Beschreibung
/// Ein Enum für beide Flächen: `#[raw(subcommand)]` verteilt die rohen Tokens
/// der Command-Zeile (erstes Token = Subcommand, `nth`/`join_from` **0-basiert
/// relativ zum Rest**), `#[serde(tag = "action")]` deserialisiert dasselbe
/// Vokabular aus dem JSON der Modell-Tool-Fläche, und
/// [`harw_macros::OpArgs`] erzeugt daraus das geschlossene JSON-Schema.
///
/// Jedes Feld trägt `#[serde(default)]`, weil ein intern getaggtes Enum sonst
/// jedes Feld der gewählten Variante als Pflichtfeld verlangen würde — die
/// Modell-Tool-Fläche soll Optionales weglassen dürfen.
///
/// # Spec-Referenz
/// AP W4-01; Design-Doc `planning-tool-v1.md` §3 (Aktionsvokabular).
#[derive(Debug, Default, serde::Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
#[raw(subcommand)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum PlanArgs {
    /// `plan create <plan-id> <ziel…>` — legt einen Plan an.
    Create {
        /// Bezeichner des neuen Plans.
        #[serde(default)]
        #[raw(nth = 0)]
        id: Option<String>,
        /// Ziel-Statement (Kurzform; die belastbare Zielbeschreibung lebt im Goal).
        #[serde(default)]
        #[raw(join_from = 1)]
        goal: Option<String>,
    },
    /// `plan inspect` — Plan als Übersicht (Knoten, Status, Wellen).
    #[default]
    #[raw(default_subcommand)]
    Inspect,
    /// `plan add <task-id> <kind> <ziel…>` — fügt einen Knoten hinzu.
    Add {
        /// Bezeichner des neuen Knotens.
        #[serde(default)]
        #[raw(nth = 0)]
        id: Option<String>,
        /// Knotenart: research, explore, analysis, synthesis, contract, coding,
        /// integration, verification, docs, composite.
        #[serde(default)]
        #[raw(nth = 1)]
        kind: Option<String>,
        /// Ziel des Knotens.
        #[serde(default)]
        #[raw(join_from = 2)]
        objective: Option<String>,
    },
    /// `plan patch <task-id> <feld> <wert…>` — ändert ein Feld
    /// (objective|kind|wave|write-scope|read-scope).
    Patch {
        /// Bezeichner des zu ändernden Knotens.
        #[serde(default)]
        #[raw(nth = 0)]
        id: Option<String>,
        /// Zu änderndes Feld.
        #[serde(default)]
        #[raw(nth = 1)]
        field: Option<String>,
        /// Neuer Wert; bei Scope-Feldern eine Komma- oder Leerzeichen-Liste.
        #[serde(default)]
        #[raw(join_from = 2)]
        value: Option<String>,
    },
    /// `plan dep <child> <parent>` — Abhängigkeitskante.
    Dep {
        /// Abhängiger Knoten.
        #[serde(default)]
        #[raw(nth = 0)]
        child: Option<String>,
        /// Knoten, von dem `child` abhängt.
        #[serde(default)]
        #[raw(nth = 1)]
        parent: Option<String>,
    },
    /// `plan status <task-id> <status>` — Statuswechsel.
    Status {
        /// Bezeichner des Knotens.
        #[serde(default)]
        #[raw(nth = 0)]
        id: Option<String>,
        /// Neuer Status: draft, ready, in_progress, blocked, completed,
        /// superseded, invalidated.
        #[serde(default)]
        #[raw(nth = 1)]
        status: Option<String>,
    },
    /// `plan evidence <task-id> <kind> <locator…>` — Nachweis anhängen.
    Evidence {
        /// Bezeichner des Knotens.
        #[serde(default)]
        #[raw(nth = 0)]
        id: Option<String>,
        /// Nachweisart: finding, cargo_test, clippy, diff, trace_span, manual,
        /// job, other.
        #[serde(default)]
        #[raw(nth = 1)]
        kind: Option<String>,
        /// Lokator des Nachweises (Pfad, Kommando, URL oder ID).
        #[serde(default)]
        #[raw(join_from = 2)]
        locator: Option<String>,
    },
    /// `plan ready` — ausführbare Knoten.
    Ready,
    /// `plan waves` — topologische Ausführungswellen mit WriteSet-Batches.
    Waves,
    /// `plan expand <parent> <kind:id-1,kind:id-2,…>` — Knoten in Kinder zerlegen.
    Expand {
        /// Zu zerlegender Elternknoten.
        #[serde(default)]
        #[raw(nth = 0)]
        parent: Option<String>,
        /// Kommaliste der Kinder, je Eintrag `kind:id` oder nur `id`
        /// (dann erbt das Kind die Art des Elternknotens).
        #[serde(default)]
        #[raw(join_from = 1)]
        children: Option<String>,
    },
    /// `plan condense <id-1,id-2,…> <ersatz-id> <zusammenfassung…>`
    Condense {
        /// Kommaliste der zu verdichtenden, abgeschlossenen Knoten.
        #[serde(default)]
        #[raw(nth = 0)]
        group: Option<String>,
        /// Bezeichner des neuen Contract-Knotens.
        #[serde(default)]
        #[raw(nth = 1)]
        replacement: Option<String>,
        /// Verdichtete Zusammenfassung der Ergebnisse.
        #[serde(default)]
        #[raw(join_from = 2)]
        summary: Option<String>,
    },
    /// `plan reopen <task-id> <grund…>` — invalidierten Knoten neu öffnen.
    Reopen {
        /// Bezeichner des erneut zu öffnenden Knotens.
        #[serde(default)]
        #[raw(nth = 0)]
        id: Option<String>,
        /// Grund der Wiedereröffnung.
        #[serde(default)]
        #[raw(join_from = 1)]
        reason: Option<String>,
    },
    /// `plan reconcile` — Controller-Lauf: Evidenz, Explorationen, Vorschläge.
    Reconcile,
    /// `plan supersede <neue-revision>` — Plan-Revision ablösen.
    Supersede {
        /// Neue Eltern-Revision (muss größer als die aktuelle sein).
        #[serde(default)]
        #[raw(nth = 0)]
        revision: Option<String>,
    },
    /// `plan bind-goal <goal-id>` — Plan an ein Goal binden.
    BindGoal {
        /// Bezeichner des zu bindenden Goals.
        #[serde(default)]
        #[raw(nth = 0)]
        goal_id: Option<String>,
    },
}

/// Argumenttyp der `/plan`-Operation: Subcommand **plus** Aufruf-Fläche.
///
/// # Beschreibung
/// Siehe Modulkopf. Die drei Konstruktionswege bestimmen die Fläche:
///
/// - [`harw_operations::FromRawArgs`] ⇒ [`CallSurface::Command`]
/// - `serde::Deserialize` ⇒ [`CallSurface::Model`]
/// - `Default` ⇒ [`CallSurface::Model`] (nur im Tool-Zweig des Makros erreicht)
///
/// Für direkte Konstruktion (Tests, Aufrufer außerhalb des Makros) gibt es
/// [`PlanCall::from_command`] und [`PlanCall::from_model`]; `Default` ist
/// bewusst *nicht* der bequeme Weg, weil er die Fläche implizit setzt.
#[derive(Debug, serde::Deserialize)]
#[serde(from = "PlanArgs")]
pub struct PlanCall {
    /// Fläche, über die der Aufruf kam.
    pub surface: CallSurface,
    /// Der gewählte Subcommand mit seinen Argumenten.
    pub args: PlanArgs,
}

impl PlanCall {
    /// Baut einen Aufruf der Command-Fläche (menschlicher Akteur).
    #[must_use]
    pub fn from_command(args: PlanArgs) -> Self {
        Self {
            surface: CallSurface::Command,
            args,
        }
    }

    /// Baut einen Aufruf der Modell-Tool-Fläche (Modell-Akteur).
    #[must_use]
    pub fn from_model(args: PlanArgs) -> Self {
        Self {
            surface: CallSurface::Model,
            args,
        }
    }
}

impl From<PlanArgs> for PlanCall {
    /// Serde-Pfad: erreicht wird er nur aus `OpInvocation::ModelTool` bzw.
    /// `AgentTool` — beide sind Modell-Flächen.
    fn from(args: PlanArgs) -> Self {
        Self::from_model(args)
    }
}

impl Default for PlanCall {
    /// Default-Pfad: das `#[operation]`-Makro ruft `Default::default()`
    /// ausschließlich für `ModelTool`/`AgentTool` mit `Null`-Argumenten auf.
    fn default() -> Self {
        Self::from_model(PlanArgs::default())
    }
}

impl harw_operations::FromRawArgs for PlanCall {
    /// Command-Pfad: das Makro ruft `from_raw_args` **nur** für
    /// `OpInvocation::Command` auf — auch bei leerem Token-Slice.
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self::from_command(
            <PlanArgs as harw_operations::FromRawArgs>::from_raw_args(tokens)?,
        ))
    }
}

impl harw_operations::OpArgsSchema for PlanCall {
    /// Reicht das von [`PlanArgs`] abgeleitete Subcommand-Schema durch — die
    /// Hülle ist ein Laufzeit-Detail und darf auf der Modell-Fläche nicht
    /// sichtbar werden.
    fn json_schema() -> harw_tools::JsonSchema {
        <PlanArgs as harw_operations::OpArgsSchema>::json_schema()
    }
}

// ── Operation ────────────────────────────────────────────────────────────────

/// Führt einen `/plan`-Subcommand gegen den registrierten Plan-Store aus.
///
/// # Beschreibung
/// Auflösungsreihenfolge (fail-closed):
/// 1. [`OpContextPlanExt::plan_config`] — fehlt sie, ist das Werkzeug in dieser
///    Laufzeit nicht komponiert.
/// 2. [`PlanToolConfig::require_enabled`] — `[tools.plan] enabled = false`
///    beendet hier.
/// 3. [`OpContextPlanExt::plan_store`] — ohne Store keine Aktion.
///
/// Danach wird der Subcommand ausgeführt. Lesende Subcommands (`inspect`,
/// `ready`, `waves`) rühren den Store nicht an; `reconcile` wendet ausschließlich
/// Runtime-Schritte an und gibt jede Entscheidung als Vorschlag zurück.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext; liefert Session-Identität und
///   die Plan-Dienste.
/// - `call` (`PlanCall`): Subcommand plus Aufruf-Fläche.
///
/// # Rückgabe
/// `Ok(OpOutput::from(text))` mit kompaktem, für ein Modell lesbarem Text.
///
/// # Fehler
/// - [`OpError::NotAvailable`]: kein Plan-Store, Werkzeug deaktiviert oder kein
///   [`Principal`] im Kontext ([`MISSING_PRINCIPAL`]).
/// - [`OpError::InvalidArguments`]: fehlende oder unverständliche Argumente.
/// - [`OpError::Execution`]: `harw-plan` oder `harw-plan-bridge` lehnt ab.
///
/// # Nebenläufigkeit
/// Zustandslos; die Serialisierung liegt beim `PlanStore`.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über `Operation::run` — direkte Nutzung nur im Test.
/// ```
#[operation(
    name = "plan",
    summary = "Plan verwalten: anlegen, Knoten pflegen, Wellen und Bereitschaft lesen, abgleichen.",
    domain = "execution",
    permission = "operator",
    command(path = "/plan", visibility = "channel_parity"),
    model_tool(approval = "always"),
    // Web-Fläche übernimmt dieselbe Achse wie das ModelTool: die Operation
    // deckt sowohl lesende (Wellen/Bereitschaft anzeigen) als auch
    // schreibende Sub-Kommandos (Knoten anlegen/pflegen, abgleichen) über
    // denselben Aufrufpfad ab — `approval = "always"` behandelt jeden
    // Aufruf konservativ als bestätigungspflichtig, statt eine neue,
    // sub-kommando-genaue Autoritätsachse zu erfinden.
    web(path = "/api/plan", method = "post", approval = "always")
)]
async fn plan(ctx: &OpContext, call: PlanCall) -> Result<OpOutput, OpError> {
    let config = require_service!(ctx.plan_config(), "Plan-Store");
    config.require_enabled().map_err(|error| {
        OpError::NotAvailable(format!(
            "Plan-Werkzeug ist nicht nutzbar ({error}); aktiviere es über `[tools.plan] enabled = true`"
        ))
    })?;
    // Autorität vor jedem Store-Zugriff: ohne Principal kein Akteur, ohne
    // Akteur keine Aktion (auch keine lesende — fail-closed, F-049).
    let actor = require_actor(ctx, call.surface)?;
    let store_handle = require_service!(ctx.plan_store(), "Plan-Store");
    let store: &dyn PlanStore = store_handle.as_ref();

    let text = match call.args {
        PlanArgs::Inspect => render_inspect(&current_plan(store)?),
        PlanArgs::Ready => render_ready(&current_plan(store)?),
        PlanArgs::Waves => render_waves(&current_plan(store)?)?,
        PlanArgs::Reconcile => run_reconcile(ctx, store, &config, &actor)?,

        PlanArgs::Create { id, goal } => {
            let plan_id = parse_plan_id(&require_arg(id, "plan create <plan-id> <ziel…>")?)?;
            let goal = require_arg(goal, "plan create <plan-id> <ziel…>")?;
            let event = apply(
                store,
                PlanAction::Create {
                    plan_id: plan_id.clone(),
                    goal,
                },
                &actor,
            )?;
            format!("Plan '{plan_id}' angelegt (Revision {}).", event.revision)
        }

        PlanArgs::Add {
            id,
            kind,
            objective,
        } => {
            let usage = "plan add <task-id> <kind> <ziel…>";
            let id = require_arg(id, usage)?;
            let kind = parse_kind(&require_arg(kind, usage)?)?;
            let objective = require_arg(objective, usage)?;
            let event = apply(
                store,
                PlanAction::AddNode {
                    node: new_node(&id, kind, objective),
                },
                &actor,
            )?;
            format!(
                "Knoten '{id}' [{}] hinzugefügt (Revision {}).",
                kind_label(kind),
                event.revision
            )
        }

        PlanArgs::Patch { id, field, value } => {
            let usage = "plan patch <task-id> <objective|kind|wave|write-scope|read-scope> <wert…>";
            let id = require_arg(id, usage)?;
            let field = require_arg(field, usage)?;
            let value = require_arg(value, usage)?;
            let patch = build_patch(&field, &value)?;
            let event = apply(
                store,
                PlanAction::UpdateNode {
                    id: TaskId::new(id.as_str()),
                    patch,
                },
                &actor,
            )?;
            format!(
                "Knoten '{id}': Feld '{field}' gesetzt (Revision {}).",
                event.revision
            )
        }

        PlanArgs::Dep { child, parent } => {
            let usage = "plan dep <child> <parent>";
            let child = require_arg(child, usage)?;
            let parent = require_arg(parent, usage)?;
            let event = apply(
                store,
                PlanAction::AddDependency {
                    child: TaskId::new(child.as_str()),
                    parent: TaskId::new(parent.as_str()),
                },
                &actor,
            )?;
            format!(
                "Kante '{child}' → '{parent}' gesetzt (Revision {}).",
                event.revision
            )
        }

        PlanArgs::Status { id, status } => {
            let usage = "plan status <task-id> <status>";
            let id = require_arg(id, usage)?;
            let status = parse_status(&require_arg(status, usage)?)?;
            let event = apply(
                store,
                PlanAction::SetStatus {
                    id: TaskId::new(id.as_str()),
                    status,
                    reason: None,
                },
                &actor,
            )?;
            format!(
                "Knoten '{id}' ist jetzt {} (Revision {}).",
                status_label(status),
                event.revision
            )
        }

        PlanArgs::Evidence { id, kind, locator } => {
            let usage = "plan evidence <task-id> <kind> <locator…>";
            let id = require_arg(id, usage)?;
            let kind = parse_evidence_kind(&require_arg(kind, usage)?)?;
            let locator = require_arg(locator, usage)?;
            let event = apply(
                store,
                PlanAction::AttachEvidence {
                    id: TaskId::new(id.as_str()),
                    evidence: EvidenceRef {
                        kind,
                        locator: locator.clone(),
                        // Runtime-eigenes Feld: der Store überschreibt es mit
                        // seiner eigenen Uhr (harw-plan/src/mutation.rs).
                        attached_at: OffsetDateTime::UNIX_EPOCH,
                        actor: actor.clone(),
                        // CLI-Argument: nur ein vom Aufrufer getippter Lokator
                        // (Pfad/URL/ID) liegt vor, kein gelesener Inhalt.
                        digest: None,
                    },
                },
                &actor,
            )?;
            format!(
                "Nachweis [{kind}] '{locator}' an '{id}' angehängt (Revision {}).",
                event.revision
            )
        }

        PlanArgs::Expand { parent, children } => {
            let usage = "plan expand <parent> <kind:id-1,kind:id-2,…>";
            let parent_id = require_arg(parent, usage)?;
            let spec = require_arg(children, usage)?;
            let plan = current_plan(store)?;
            let parent_node = find_node(&plan, &parent_id)?;
            let children = parse_children(&spec, parent_node, usage)?;
            let names = children
                .iter()
                .map(|child| child.id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let event = apply(
                store,
                PlanAction::Expand {
                    parent: TaskId::new(parent_id.as_str()),
                    children,
                },
                &actor,
            )?;
            format!(
                "Knoten '{parent_id}' in {names} zerlegt (Revision {}).\n\
                 Hinweis: Kinder starten ohne Schreibbereich — weise ihn mit \
                 `plan patch <id> write-scope …` zu.",
                event.revision
            )
        }

        PlanArgs::Condense {
            group,
            replacement,
            summary,
        } => {
            let usage = "plan condense <id-1,id-2,…> <ersatz-id> <zusammenfassung…>";
            let group = split_list(&require_arg(group, usage)?);
            if group.is_empty() {
                return Err(OpError::InvalidArguments(format!(
                    "keine Knoten zum Verdichten angegeben; Aufruf: {usage}"
                )));
            }
            let replacement_id = require_arg(replacement, usage)?;
            let summary = require_arg(summary, usage)?;
            let event = apply(
                store,
                PlanAction::Condense {
                    superseded: group.iter().map(|id| TaskId::new(id.as_str())).collect(),
                    replacement: new_node(&replacement_id, PlanNodeKind::Contract, summary.clone()),
                    summary,
                },
                &actor,
            )?;
            format!(
                "{} Knoten zu Contract-Knoten '{replacement_id}' verdichtet (Revision {}).",
                group.len(),
                event.revision
            )
        }

        PlanArgs::Reopen { id, reason } => {
            let usage = "plan reopen <task-id> <grund…>";
            let id = require_arg(id, usage)?;
            let reason = require_arg(reason, usage)?;
            let event = apply(
                store,
                PlanAction::Reopen {
                    id: TaskId::new(id.as_str()),
                    reason,
                },
                &actor,
            )?;
            format!(
                "Knoten '{id}' erneut geöffnet (Revision {}).",
                event.revision
            )
        }

        PlanArgs::Supersede { revision } => {
            let usage = "plan supersede <neue-revision>";
            let raw = require_arg(revision, usage)?;
            let parsed: u64 = raw.trim().parse().map_err(|_| {
                OpError::InvalidArguments(format!(
                    "'{raw}' ist keine Revisionsnummer; Aufruf: {usage}"
                ))
            })?;
            let event = apply(
                store,
                PlanAction::Supersede {
                    new_parent_revision: RevisionId::new(parsed),
                },
                &actor,
            )?;
            format!(
                "Plan auf Eltern-Revision {parsed} abgelöst (Revision {}).",
                event.revision
            )
        }

        PlanArgs::BindGoal { goal_id } => {
            let goal_id = require_arg(goal_id, "plan bind-goal <goal-id>")?;
            let event = apply(
                store,
                PlanAction::BindGoal {
                    goal_id: goal_id.clone(),
                },
                &actor,
            )?;
            format!(
                "Plan an Goal '{goal_id}' gebunden (Revision {}).",
                event.revision
            )
        }
    };

    Ok(OpOutput::from(text))
}

// ── Store-Zugriff ────────────────────────────────────────────────────────────

/// Liest den aktuellen Plan-Snapshot.
fn current_plan(store: &dyn PlanStore) -> Result<Plan, OpError> {
    store.current().map_err(map_plan_error)
}

/// Wendet eine Aktion an und übersetzt den Fehler in einen [`OpError`].
fn apply(store: &dyn PlanStore, action: PlanAction, actor: &str) -> Result<PlanEvent, OpError> {
    store.apply(action, actor).map_err(map_plan_error)
}

/// Sucht einen Knoten im Snapshot oder meldet ihn als ungültiges Argument.
fn find_node<'a>(plan: &'a Plan, id: &str) -> Result<&'a PlanNode, OpError> {
    plan.nodes
        .iter()
        .find(|node| node.id.as_str() == id)
        .ok_or_else(|| OpError::InvalidArguments(format!("Plan-Knoten '{id}' existiert nicht")))
}

/// Übersetzt einen [`PlanError`] in den passenden [`OpError`].
///
/// `PlanNotFound` ist kein Ausführungsfehler, sondern eine Aussage über den
/// Zustand: es gibt noch keinen Plan. Das gehört zu `InvalidArguments`, damit
/// ein Modell den nächsten Schritt (`plan create`) ableiten kann.
fn map_plan_error(error: PlanError) -> OpError {
    match error {
        PlanError::PlanNotFound => OpError::InvalidArguments(
            "es existiert noch kein Plan; lege ihn mit `plan create <plan-id> <ziel…>` an"
                .to_owned(),
        ),
        other => OpError::Execution(format!("Plan-Store lehnt ab: {other}")),
    }
}

/// Übersetzt einen [`PlanBridgeError`] in den passenden [`OpError`].
fn map_bridge_error(error: PlanBridgeError) -> OpError {
    match error {
        PlanBridgeError::ServiceMissing { service } => {
            OpError::NotAvailable(format!("Dienst '{service}' ist nicht konfiguriert"))
        }
        other => OpError::Execution(format!("Plan-Abgleich lehnt ab: {other}")),
    }
}

// ── Argument-Helfer ──────────────────────────────────────────────────────────

/// Fordert ein Pflichtargument oder meldet die Aufrufform.
fn require_arg(value: Option<String>, usage: &str) -> Result<String, OpError> {
    match value {
        Some(raw) if !raw.trim().is_empty() => Ok(raw.trim().to_owned()),
        _ => Err(OpError::InvalidArguments(format!(
            "fehlendes Argument; Aufruf: {usage}"
        ))),
    }
}

/// Normalisiert einen Bezeichner auf kleingeschriebenes snake_case.
fn normalize(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace('-', "_")
}

/// Zerlegt eine Komma- oder Leerzeichen-Liste in nicht-leere Einträge.
fn split_list(raw: &str) -> Vec<String> {
    raw.split([',', ' ', '\t'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Parst eine Knotenart.
///
/// # Errors
/// [`OpError::InvalidArguments`] mit der vollständigen Werteliste, wenn der
/// Name unbekannt ist.
fn parse_kind(raw: &str) -> Result<PlanNodeKind, OpError> {
    match normalize(raw).as_str() {
        "research" => Ok(PlanNodeKind::Research),
        "explore" => Ok(PlanNodeKind::Explore),
        "analysis" => Ok(PlanNodeKind::Analysis),
        "synthesis" => Ok(PlanNodeKind::Synthesis),
        "contract" => Ok(PlanNodeKind::Contract),
        "coding" => Ok(PlanNodeKind::Coding),
        "integration" => Ok(PlanNodeKind::Integration),
        "verification" => Ok(PlanNodeKind::Verification),
        "docs" => Ok(PlanNodeKind::Docs),
        "composite" => Ok(PlanNodeKind::Composite),
        other => Err(OpError::InvalidArguments(format!(
            "unbekannte Knotenart '{other}'; erwartet eines von: research, explore, analysis, \
             synthesis, contract, coding, integration, verification, docs, composite"
        ))),
    }
}

/// Parst einen Knotenstatus.
///
/// # Errors
/// [`OpError::InvalidArguments`], wenn der Name unbekannt ist.
fn parse_status(raw: &str) -> Result<PlanNodeStatus, OpError> {
    match normalize(raw).as_str() {
        "draft" => Ok(PlanNodeStatus::Draft),
        "ready" => Ok(PlanNodeStatus::Ready),
        "in_progress" => Ok(PlanNodeStatus::InProgress),
        "blocked" => Ok(PlanNodeStatus::Blocked),
        "completed" => Ok(PlanNodeStatus::Completed),
        "superseded" => Ok(PlanNodeStatus::Superseded),
        "invalidated" => Ok(PlanNodeStatus::Invalidated),
        other => Err(OpError::InvalidArguments(format!(
            "unbekannter Status '{other}'; erwartet eines von: draft, ready, in_progress, \
             blocked, completed, superseded, invalidated"
        ))),
    }
}

/// Parst eine Nachweisart.
///
/// # Beschreibung
/// [`EvidenceKind`] hat zwar eine unfehlbare [`std::str::FromStr`]-Implementierung
/// (unbekannt ⇒ `Other`), die für Alt-Snapshots gedacht ist. An der Eingabegrenze
/// wäre dieselbe Toleranz ein Fehler: ein Tippfehler würde still zu `Other`, und
/// die spätere Kriterien-Auswertung fände den Nachweis nie. Deshalb wird hier
/// explizit geprüft — `other` bleibt als *gewählter* Wert zulässig.
///
/// # Errors
/// [`OpError::InvalidArguments`], wenn der Name unbekannt ist.
fn parse_evidence_kind(raw: &str) -> Result<EvidenceKind, OpError> {
    match normalize(raw).as_str() {
        "finding" => Ok(EvidenceKind::Finding),
        "cargo_test" => Ok(EvidenceKind::CargoTest),
        "clippy" => Ok(EvidenceKind::Clippy),
        "diff" => Ok(EvidenceKind::Diff),
        "trace_span" => Ok(EvidenceKind::TraceSpan),
        "manual" => Ok(EvidenceKind::Manual),
        "job" => Ok(EvidenceKind::Job),
        "other" => Ok(EvidenceKind::Other),
        other => Err(OpError::InvalidArguments(format!(
            "unbekannte Nachweisart '{other}'; erwartet eines von: finding, cargo_test, clippy, \
             diff, trace_span, manual, job, other"
        ))),
    }
}

/// Baut einen frischen Plan-Knoten im Status `Draft`.
///
/// # Beschreibung
/// Alle Scopes bleiben leer und werden per `plan patch` gesetzt — so kann ein
/// neuer Knoten nie versehentlich einen Schreibbereich beanspruchen, den ein
/// aktiver Knoten schon hält. `created_at`/`updated_at` sind Platzhalter: der
/// Store überschreibt beide mit seiner eigenen Uhr.
fn new_node(id: &str, kind: PlanNodeKind, objective: String) -> PlanNode {
    PlanNode {
        id: TaskId::new(id),
        objective,
        dependencies: Vec::new(),
        input_contracts: Vec::new(),
        output_contracts: Vec::new(),
        read_scope: Vec::new(),
        write_scope: Vec::new(),
        forbidden_scope: Vec::new(),
        acceptance_criteria: Vec::new(),
        invalidation_conditions: Vec::new(),
        status: PlanNodeStatus::Draft,
        evidence: Vec::new(),
        kind,
        wave: None,
        assignment: None,
        parent: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    }
}

/// Baut aus `<feld> <wert>` einen [`NodePatch`].
///
/// # Errors
/// [`OpError::InvalidArguments`] bei unbekanntem Feld oder unparsbarem Wert.
fn build_patch(field: &str, value: &str) -> Result<NodePatch, OpError> {
    let mut patch = NodePatch::default();
    match normalize(field).as_str() {
        "objective" => patch.objective = Some(value.to_owned()),
        "kind" => patch.kind = Some(parse_kind(value)?),
        "wave" => {
            let normalized = normalize(value);
            if matches!(normalized.as_str(), "none" | "-" | "keine") {
                // Äußeres `Some` = Feld angefasst, inneres `None` = löschen.
                patch.wave = Some(None);
            } else {
                let parsed: u32 = normalized.parse().map_err(|_| {
                    OpError::InvalidArguments(format!(
                        "'{value}' ist keine Wellennummer; erwartet eine Zahl oder 'none'"
                    ))
                })?;
                patch.wave = Some(Some(parsed));
            }
        }
        "write_scope" => patch.write_scope = Some(scope_list(value)),
        "read_scope" => patch.read_scope = Some(scope_list(value)),
        other => {
            return Err(OpError::InvalidArguments(format!(
                "unbekanntes Feld '{other}'; erwartet eines von: objective, kind, wave, \
                 write-scope, read-scope"
            )));
        }
    }
    Ok(patch)
}

/// Wandelt eine Komma-/Leerzeichen-Liste in Scope-Einträge.
fn scope_list(raw: &str) -> Vec<PathOrSymbol> {
    split_list(raw).into_iter().map(PathOrSymbol::new).collect()
}

/// Parst die Kinder-Spezifikation von `plan expand`.
///
/// # Beschreibung
/// Jeder Eintrag ist `kind:id` oder nur `id` — im zweiten Fall erbt das Kind die
/// Art des Elternknotens. Kinder starten ohne Schreibbereich: `validate_expand`
/// verlangt `child.write_scope ⊆ parent.write_scope`, und eine leere Menge
/// erfüllt das immer. Der Schreibbereich wird danach gezielt per `plan patch`
/// zugewiesen.
///
/// # Errors
/// [`OpError::InvalidArguments`] bei leerer Liste, leerer ID oder unbekannter Art.
fn parse_children(spec: &str, parent: &PlanNode, usage: &str) -> Result<Vec<PlanNode>, OpError> {
    let entries = split_list(spec);
    if entries.is_empty() {
        return Err(OpError::InvalidArguments(format!(
            "keine Kinder angegeben; Aufruf: {usage}"
        )));
    }

    let mut children = Vec::with_capacity(entries.len());
    for entry in entries {
        let (kind, id) = match entry.split_once(':') {
            Some((raw_kind, raw_id)) => (parse_kind(raw_kind)?, raw_id.trim().to_owned()),
            None => (parent.kind, entry),
        };
        if id.is_empty() {
            return Err(OpError::InvalidArguments(format!(
                "leere Kind-ID in der Kinderliste; Aufruf: {usage}"
            )));
        }
        let objective = format!("{} — Teilschritt {id}", parent.objective);
        children.push(new_node(&id, kind, objective));
    }
    Ok(children)
}

// ── Ausgabe ──────────────────────────────────────────────────────────────────

/// Kanonischer Name einer Knotenart (identisch zur Serde-Repräsentation).
fn kind_label(kind: PlanNodeKind) -> &'static str {
    match kind {
        PlanNodeKind::Research => "research",
        PlanNodeKind::Explore => "explore",
        PlanNodeKind::Analysis => "analysis",
        PlanNodeKind::Synthesis => "synthesis",
        PlanNodeKind::Contract => "contract",
        PlanNodeKind::Coding => "coding",
        PlanNodeKind::Integration => "integration",
        PlanNodeKind::Verification => "verification",
        PlanNodeKind::Docs => "docs",
        PlanNodeKind::Composite => "composite",
    }
}

/// Kanonischer Name eines Knotenstatus (identisch zur Serde-Repräsentation).
fn status_label(status: PlanNodeStatus) -> &'static str {
    match status {
        PlanNodeStatus::Draft => "draft",
        PlanNodeStatus::Ready => "ready",
        PlanNodeStatus::InProgress => "in_progress",
        PlanNodeStatus::Blocked => "blocked",
        PlanNodeStatus::Completed => "completed",
        PlanNodeStatus::Superseded => "superseded",
        PlanNodeStatus::Invalidated => "invalidated",
    }
}

/// Verbindet Scope-Einträge zu einer Kommaliste.
fn join_scope(scope: &[PathOrSymbol]) -> String {
    scope
        .iter()
        .map(PathOrSymbol::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rendert einen Knoten als zwei Zeilen: Kopf und Detailzeile.
///
/// # Beschreibung
/// Kopfzeile `- <id> [<art>/<status> wave=<n>] <ziel>`, Detailzeile nur mit den
/// tatsächlich belegten Feldern (`write`, `read`, `deps`, `evidenz`, `parent`).
/// Leere Felder werden weggelassen, damit ein Modell keine Platzhalter
/// interpretieren muss.
fn render_node(node: &PlanNode) -> String {
    let wave = match node.wave {
        Some(number) => format!(" wave={number}"),
        None => String::new(),
    };
    let mut head = format!(
        "- {} [{}/{}{wave}] {}",
        node.id,
        kind_label(node.kind),
        status_label(node.status),
        node.objective
    );

    let mut details: Vec<String> = Vec::new();
    if !node.write_scope.is_empty() {
        details.push(format!("write: {}", join_scope(&node.write_scope)));
    }
    if !node.read_scope.is_empty() {
        details.push(format!("read: {}", join_scope(&node.read_scope)));
    }
    if !node.dependencies.is_empty() {
        let deps = node
            .dependencies
            .iter()
            .map(TaskId::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        details.push(format!("deps: {deps}"));
    }
    if let Some(parent) = &node.parent {
        details.push(format!("parent: {parent}"));
    }
    if !node.evidence.is_empty() {
        details.push(format!("evidenz: {}", node.evidence.len()));
    }
    if !details.is_empty() {
        head.push_str("\n    ");
        head.push_str(&details.join(" | "));
    }
    head
}

/// Rendert die Kopfzeilen eines Plans (Identität, Bindung, Statuszählung).
fn render_plan_header(plan: &Plan) -> String {
    let goal_binding = match &plan.goal_id {
        Some(goal_id) => goal_id.as_str(),
        None => "keine",
    };
    let mut counts: Vec<String> = Vec::new();
    for status in [
        PlanNodeStatus::Draft,
        PlanNodeStatus::Ready,
        PlanNodeStatus::InProgress,
        PlanNodeStatus::Blocked,
        PlanNodeStatus::Completed,
        PlanNodeStatus::Superseded,
        PlanNodeStatus::Invalidated,
    ] {
        let count = plan
            .nodes
            .iter()
            .filter(|node| node.status == status)
            .count();
        if count > 0 {
            counts.push(format!("{}={count}", status_label(status)));
        }
    }
    let summary = if counts.is_empty() {
        "keine".to_owned()
    } else {
        counts.join(", ")
    };
    format!(
        "Plan {} (Revision {}) — Ziel: {}\nGoal-Bindung: {goal_binding}\nKnoten: {} ({summary})",
        plan.id,
        plan.revision,
        plan.goal_statement,
        plan.nodes.len()
    )
}

/// `plan inspect` — Kopf plus alle Knoten.
fn render_inspect(plan: &Plan) -> String {
    let mut lines = vec![render_plan_header(plan)];
    if plan.nodes.is_empty() {
        lines.push("Noch keine Knoten. `plan add <task-id> <kind> <ziel…>`".to_owned());
    } else {
        lines.extend(plan.nodes.iter().map(render_node));
    }
    lines.join("\n")
}

/// `plan ready` — die aktuell ausführbaren Knoten.
fn render_ready(plan: &Plan) -> String {
    let ready = graph::ready_nodes(plan);
    if ready.is_empty() {
        return "Kein Knoten ist ausführbar (alle blockiert, erledigt oder terminal).".to_owned();
    }
    let mut lines = vec![format!("{} ausführbare(r) Knoten:", ready.len())];
    lines.extend(ready.into_iter().map(render_node));
    lines.join("\n")
}

/// `plan waves` — topologische Wellen mit WriteSet-disjunkten Batches.
///
/// # Beschreibung
/// Welle `n` enthält alle Knoten, deren Abhängigkeiten vollständig in den Wellen
/// `0..n` liegen. Innerhalb einer Welle zerlegt
/// [`graph::partition_write_sets`] die Knoten zusätzlich in Batches, deren
/// Schreibbereiche einander nicht berühren — nur diese dürfen wirklich
/// gleichzeitig laufen.
///
/// # Errors
/// [`OpError::Execution`], wenn der Dependency-Graph zyklisch ist.
fn render_waves(plan: &Plan) -> Result<String, OpError> {
    let waves = graph::topological_waves(plan).map_err(map_plan_error)?;
    if waves.is_empty() {
        return Ok("Keine nicht-terminalen Knoten — keine Wellen zu planen.".to_owned());
    }

    let mut lines = vec![format!("{} Ausführungswelle(n):", waves.len())];
    for (index, wave) in waves.iter().enumerate() {
        let nodes: Vec<&PlanNode> = wave
            .iter()
            .filter_map(|id| plan.nodes.iter().find(|node| &node.id == id))
            .collect();
        lines.push(format!("Welle {index} ({} Knoten):", nodes.len()));
        for (batch_index, batch) in graph::partition_write_sets(&nodes).iter().enumerate() {
            let ids = batch
                .iter()
                .map(TaskId::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("  Batch {batch_index} (WriteSet-disjunkt): {ids}"));
        }
        for node in nodes {
            lines.push(format!("  {}", render_node(node).replace('\n', "\n  ")));
        }
    }
    Ok(lines.join("\n"))
}

/// `plan reconcile` — Controller-Lauf und Bericht.
///
/// # Beschreibung
/// [`PlanController::reconcile`] ist rein: sie liest den Snapshot und liefert
/// eine geordnete Schrittfolge. [`PlanController::apply`] wendet davon
/// ausschließlich Runtime-Schritte an (Evidenz anhängen, invalidieren,
/// Exploration einschieben, Bereitschaft erklären) und gibt alles zurück, was
/// eine Entscheidung verlangt — Vorschläge an das Modell, Job-Admission und
/// jeden Zielstatus-Vorschlag.
///
/// `new_findings` und `job_states` sind hier bewusst leer: die Zustellung von
/// Recherche-Ergebnissen und Job-Zuständen gehört der Research- bzw.
/// Job-Pipeline. Würde diese Operation alle abgelegten Findings bei jedem Lauf
/// erneut einspeisen, hinge an jedem Knoten nach kurzer Zeit dieselbe Evidenz
/// mehrfach.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: es gibt noch keinen Plan.
/// - [`OpError::Execution`]: der Store lehnt eine Mutation ab.
fn run_reconcile(
    ctx: &OpContext,
    store: &dyn PlanStore,
    config: &PlanToolConfig,
    actor: &str,
) -> Result<String, OpError> {
    let plan = current_plan(store)?;
    let goal_store = ctx.goal_store();
    let goal = goal_store.as_ref().and_then(|store| store.current().ok());

    let steps = PlanController::reconcile(ReconcileInput {
        goal: goal.as_ref(),
        plan: &plan,
        new_findings: &[],
        job_states: &[],
        config,
        now: OffsetDateTime::now_utc(),
    });

    let (events, deferred) = PlanController::apply(&steps, store, goal_store.as_deref(), actor)
        .map_err(map_bridge_error)?;

    let mut lines = vec![format!(
        "Abgleich: {} Schritt(e) berechnet, {} angewandt, {} offen.",
        steps.len(),
        events.len(),
        deferred.len()
    )];

    if events.is_empty() {
        lines.push("Angewandt: nichts.".to_owned());
    } else {
        lines.push("Angewandt:".to_owned());
        lines.extend(events.iter().map(|event| {
            format!(
                "- rev {}: {} (actor={})",
                event.revision,
                action_label(&event.action),
                event.actor
            )
        }));
    }

    if deferred.is_empty() {
        lines.push("Offen: nichts — keine Entscheidung nötig.".to_owned());
    } else {
        lines.push("Offen (nur vorgeschlagen, nicht angewandt):".to_owned());
        lines.extend(
            deferred
                .iter()
                .map(|step| format!("- {}", step_label(step))),
        );
    }

    Ok(lines.join("\n"))
}

/// Kurzname einer angewandten Plan-Aktion (identisch zum `op`-Tag der Serde-Form).
fn action_label(action: &PlanAction) -> String {
    match action {
        PlanAction::Create { plan_id, .. } => format!("create {plan_id}"),
        PlanAction::AddNode { node } => format!("add_node {}", node.id),
        PlanAction::UpdateNode { id, .. } => format!("update_node {id}"),
        PlanAction::AddDependency { child, parent } => {
            format!("add_dependency {child} → {parent}")
        }
        PlanAction::SetStatus { id, status, .. } => {
            format!("set_status {id} → {}", status_label(*status))
        }
        PlanAction::AttachEvidence { id, evidence } => {
            format!(
                "attach_evidence {id} [{}] {}",
                evidence.kind, evidence.locator
            )
        }
        PlanAction::Invalidate { ids, .. } => format!("invalidate {} Knoten", ids.len()),
        PlanAction::Supersede {
            new_parent_revision,
        } => format!("supersede → {new_parent_revision}"),
        PlanAction::Expand { parent, children } => {
            format!("expand {parent} → {} Kinder", children.len())
        }
        PlanAction::Condense {
            superseded,
            replacement,
            ..
        } => format!("condense {} Knoten → {}", superseded.len(), replacement.id),
        PlanAction::Reopen { id, .. } => format!("reopen {id}"),
        PlanAction::BindGoal { goal_id } => format!("bind_goal {goal_id}"),
        PlanAction::Inspect => "inspect".to_owned(),
    }
}

/// Menschen- und modelllesbare Beschreibung eines offenen Reconcile-Schrittes.
fn step_label(step: &ReconcileStep) -> String {
    match step {
        ReconcileStep::AttachEvidence { task, evidence } => {
            format!(
                "attach-evidence {task} [{}] {}",
                evidence.kind, evidence.locator
            )
        }
        ReconcileStep::Invalidate { ids, .. } => {
            format!("invalidate {} Knoten", ids.len())
        }
        ReconcileStep::InsertExplore { before, node } => {
            format!("insert-explore {} vor {before}", node.id)
        }
        ReconcileStep::ProposeExpand { parent, reason } => {
            format!("propose-expand {parent}: {reason}")
        }
        ReconcileStep::ProposeCondense { group, reason } => {
            let ids = group
                .iter()
                .map(TaskId::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            format!("propose-condense [{ids}]: {reason}")
        }
        ReconcileStep::MarkReady { ids } => format!("mark-ready {} Knoten", ids.len()),
        ReconcileStep::AdmitJobs { ids } => {
            let names = ids
                .iter()
                .map(TaskId::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            format!("admit-jobs [{names}] — braucht die Job-Bridge")
        }
        ReconcileStep::GoalStatus { status, reason } => {
            format!(
                "goal-status {status:?}: {reason} (nur per `/goal achieve` durch einen Menschen)"
            )
        }
        ReconcileStep::AskModel { prompt } => format!("ask-model: {prompt}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{CallSurface, PlanArgs, PlanCall};
    use crate::testutil::toks;
    use harw_operations::context::ServiceMap;
    use harw_operations::{FromRawArgs, OpContext, OpError};
    use harw_plan::actions::PlanAction;
    use harw_plan::ids::{PlanId, TaskId};
    use harw_plan::types::{PlanNodeKind, PlanNodeStatus};
    use harw_plan::{InMemoryPlanStore, PlanStore, PlanToolConfig};
    use harw_sandbox::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{
        IngressSurface, PermissionTier, Principal, PrincipalKind, SessionId, TenantId, TurnId,
        WorkspaceId,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ── Fixtures ─────────────────────────────────────────────────────────────

    /// Menschlicher Test-Principal, wie ihn `local_principal(Cli)` baut.
    fn human_principal() -> Principal {
        Principal::trusted_ingress(
            PrincipalKind::Human,
            "uid:1000",
            IngressSurface::Cli,
            PermissionTier::Operator,
        )
    }

    /// Baut einen `OpContext` mit temporärem Workspace und den übergebenen
    /// Diensten; ergänzt einen menschlichen Principal, falls keiner darin liegt.
    fn context_with(mut services: ServiceMap) -> (OpContext, std::path::PathBuf) {
        if services.get::<Principal>().is_none() {
            services.insert(human_principal());
        }
        bare_context(services)
    }

    /// Wie [`context_with`], aber ohne den Principal zu ergänzen.
    fn bare_context(services: ServiceMap) -> (OpContext, std::path::PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("harw-plan-op-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).expect("Test-Workspace anlegen");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .expect("WorkspaceRegistry bauen");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("Workspace auflösen");
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            root,
        )
    }

    /// Kontext mit aktiviertem Plan-Werkzeug und einem leeren In-Memory-Store.
    fn context_with_store() -> (OpContext, Arc<dyn PlanStore>, std::path::PathBuf) {
        let store: Arc<dyn PlanStore> = Arc::new(InMemoryPlanStore::new());
        let mut services = ServiceMap::new();
        services.insert(Arc::clone(&store));
        services.insert(PlanToolConfig::enabled_defaults());
        let (ctx, root) = context_with(services);
        (ctx, store, root)
    }

    fn cleanup(root: std::path::PathBuf) {
        std::fs::remove_dir_all(root).ok();
    }

    /// Führt einen Command-Aufruf über die getippten Tokens aus.
    async fn run_command(ctx: &OpContext, tokens: &[&str]) -> Result<String, OpError> {
        let call = PlanCall::from_raw_args(&toks(tokens))?;
        super::plan(ctx, call).await.map(|output| output.text)
    }

    // ── Argument-Parsing ─────────────────────────────────────────────────────

    #[test]
    fn from_raw_args_without_tokens_uses_default_subcommand_inspect() {
        let call = match PlanCall::from_raw_args(&toks(&[])) {
            Ok(call) => call,
            Err(error) => panic!("leerer Input muss den Default liefern: {error}"),
        };
        assert_eq!(call.surface, CallSurface::Command);
        assert!(matches!(call.args, PlanArgs::Inspect));
    }

    #[test]
    fn from_raw_args_unknown_subcommand_is_invalid_arguments() {
        let result = PlanCall::from_raw_args(&toks(&["fliegen"]));
        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
    }

    #[test]
    fn from_raw_args_create_takes_id_and_joined_goal() {
        match PlanCall::from_raw_args(&toks(&["create", "p-1", "Ziel", "mit", "Worten"])) {
            Ok(PlanCall {
                args: PlanArgs::Create { id, goal },
                surface,
            }) => {
                assert_eq!(surface, CallSurface::Command);
                assert_eq!(id.as_deref(), Some("p-1"));
                assert_eq!(goal.as_deref(), Some("Ziel mit Worten"));
            }
            other => panic!("erwartet Create, war: {other:?}"),
        }
    }

    #[test]
    fn from_raw_args_add_takes_id_kind_and_joined_objective() {
        match PlanCall::from_raw_args(&toks(&["add", "t-1", "coding", "Modul", "bauen"])) {
            Ok(PlanCall {
                args:
                    PlanArgs::Add {
                        id,
                        kind,
                        objective,
                    },
                ..
            }) => {
                assert_eq!(id.as_deref(), Some("t-1"));
                assert_eq!(kind.as_deref(), Some("coding"));
                assert_eq!(objective.as_deref(), Some("Modul bauen"));
            }
            other => panic!("erwartet Add, war: {other:?}"),
        }
    }

    #[test]
    fn from_raw_args_patch_takes_id_field_and_joined_value() {
        match PlanCall::from_raw_args(&toks(&["patch", "t-1", "write-scope", "a.rs", "b.rs"])) {
            Ok(PlanCall {
                args: PlanArgs::Patch { id, field, value },
                ..
            }) => {
                assert_eq!(id.as_deref(), Some("t-1"));
                assert_eq!(field.as_deref(), Some("write-scope"));
                assert_eq!(value.as_deref(), Some("a.rs b.rs"));
            }
            other => panic!("erwartet Patch, war: {other:?}"),
        }
    }

    #[test]
    fn from_raw_args_dep_takes_child_and_parent() {
        match PlanCall::from_raw_args(&toks(&["dep", "t-2", "t-1"])) {
            Ok(PlanCall {
                args: PlanArgs::Dep { child, parent },
                ..
            }) => {
                assert_eq!(child.as_deref(), Some("t-2"));
                assert_eq!(parent.as_deref(), Some("t-1"));
            }
            other => panic!("erwartet Dep, war: {other:?}"),
        }
    }

    #[test]
    fn from_raw_args_status_takes_id_and_status() {
        match PlanCall::from_raw_args(&toks(&["status", "t-1", "ready"])) {
            Ok(PlanCall {
                args: PlanArgs::Status { id, status },
                ..
            }) => {
                assert_eq!(id.as_deref(), Some("t-1"));
                assert_eq!(status.as_deref(), Some("ready"));
            }
            other => panic!("erwartet Status, war: {other:?}"),
        }
    }

    #[test]
    fn from_raw_args_evidence_takes_id_kind_and_joined_locator() {
        match PlanCall::from_raw_args(&toks(&["evidence", "t-1", "cargo_test", "cargo", "test"])) {
            Ok(PlanCall {
                args: PlanArgs::Evidence { id, kind, locator },
                ..
            }) => {
                assert_eq!(id.as_deref(), Some("t-1"));
                assert_eq!(kind.as_deref(), Some("cargo_test"));
                assert_eq!(locator.as_deref(), Some("cargo test"));
            }
            other => panic!("erwartet Evidence, war: {other:?}"),
        }
    }

    #[test]
    fn from_raw_args_unit_subcommands_parse_and_ignore_extra_tokens() {
        assert!(matches!(
            PlanCall::from_raw_args(&toks(&["ready", "--verbose"])).map(|call| call.args),
            Ok(PlanArgs::Ready)
        ));
        assert!(matches!(
            PlanCall::from_raw_args(&toks(&["waves"])).map(|call| call.args),
            Ok(PlanArgs::Waves)
        ));
        assert!(matches!(
            PlanCall::from_raw_args(&toks(&["reconcile"])).map(|call| call.args),
            Ok(PlanArgs::Reconcile)
        ));
        assert!(matches!(
            PlanCall::from_raw_args(&toks(&["inspect"])).map(|call| call.args),
            Ok(PlanArgs::Inspect)
        ));
    }

    #[test]
    fn from_raw_args_expand_condense_reopen_supersede_bind_goal_parse() {
        match PlanCall::from_raw_args(&toks(&["expand", "t-1", "coding:t-1a,coding:t-1b"])) {
            Ok(PlanCall {
                args: PlanArgs::Expand { parent, children },
                ..
            }) => {
                assert_eq!(parent.as_deref(), Some("t-1"));
                assert_eq!(children.as_deref(), Some("coding:t-1a,coding:t-1b"));
            }
            other => panic!("erwartet Expand, war: {other:?}"),
        }

        match PlanCall::from_raw_args(&toks(&["condense", "a,b", "c-1", "kurz", "gefasst"])) {
            Ok(PlanCall {
                args:
                    PlanArgs::Condense {
                        group,
                        replacement,
                        summary,
                    },
                ..
            }) => {
                assert_eq!(group.as_deref(), Some("a,b"));
                assert_eq!(replacement.as_deref(), Some("c-1"));
                assert_eq!(summary.as_deref(), Some("kurz gefasst"));
            }
            other => panic!("erwartet Condense, war: {other:?}"),
        }

        match PlanCall::from_raw_args(&toks(&["reopen", "t-1", "Annahme", "widerlegt"])) {
            Ok(PlanCall {
                args: PlanArgs::Reopen { id, reason },
                ..
            }) => {
                assert_eq!(id.as_deref(), Some("t-1"));
                assert_eq!(reason.as_deref(), Some("Annahme widerlegt"));
            }
            other => panic!("erwartet Reopen, war: {other:?}"),
        }

        match PlanCall::from_raw_args(&toks(&["supersede", "7"])) {
            Ok(PlanCall {
                args: PlanArgs::Supersede { revision },
                ..
            }) => assert_eq!(revision.as_deref(), Some("7")),
            other => panic!("erwartet Supersede, war: {other:?}"),
        }

        match PlanCall::from_raw_args(&toks(&["bind-goal", "g-1"])) {
            Ok(PlanCall {
                args: PlanArgs::BindGoal { goal_id },
                ..
            }) => assert_eq!(goal_id.as_deref(), Some("g-1")),
            other => panic!("erwartet BindGoal, war: {other:?}"),
        }
    }

    #[test]
    fn json_surface_deserializes_to_model_surface() {
        let value = serde_json::json!({ "action": "create", "id": "p-1", "goal": "Ziel" });
        let call: PlanCall = match serde_json::from_value(value) {
            Ok(call) => call,
            Err(error) => panic!("JSON-Deserialisierung schlug fehl: {error}"),
        };
        assert_eq!(call.surface, CallSurface::Model);
        assert!(matches!(call.args, PlanArgs::Create { .. }));
    }

    #[test]
    fn default_call_is_model_surface_inspect() {
        let call = PlanCall::default();
        assert_eq!(call.surface, CallSurface::Model);
        assert!(matches!(call.args, PlanArgs::Inspect));
    }

    #[test]
    fn test_actor_for_principal_human_needs_command_surface_and_human_principal() {
        let session = SessionId::new();
        let human = human_principal();
        let command = CallSurface::Command.actor_for_principal(&human, &session);
        assert_eq!(command, format!("human:uid:1000@{session}"));
        let model = CallSurface::Model.actor_for_principal(&human, &session);
        assert_eq!(model, format!("model:human/uid:1000@{session}"));
    }

    #[test]
    fn test_actor_for_principal_non_human_principal_never_gets_human_prefix() {
        let session = SessionId::new();
        for kind in [
            PrincipalKind::Model,
            PrincipalKind::Operation,
            PrincipalKind::Channel,
        ] {
            let principal = Principal::trusted_ingress(
                kind,
                "client-7",
                IngressSurface::Mcp,
                PermissionTier::Owner,
            );
            for surface in [CallSurface::Command, CallSurface::Model] {
                let actor = surface.actor_for_principal(&principal, &session);
                assert!(
                    actor.starts_with("model:"),
                    "{kind:?} über {surface:?} muss 'model:' tragen, war: {actor}"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_plan_without_principal_is_not_available_and_writes_nothing() {
        let store: Arc<dyn PlanStore> = Arc::new(InMemoryPlanStore::new());
        let mut services = ServiceMap::new();
        services.insert(Arc::clone(&store));
        services.insert(PlanToolConfig::enabled_defaults());
        let (ctx, root) = bare_context(services);

        let result = run_command(&ctx, &["create", "p-1", "ohne", "Principal"]).await;
        let plan = store.current();
        cleanup(root);

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert_eq!(message, super::MISSING_PRINCIPAL);
            }
            other => panic!("erwartet NotAvailable, war: {other:?}"),
        }
        assert!(plan.is_err(), "ohne Principal darf kein Plan entstehen");
    }

    #[tokio::test]
    async fn test_plan_create_rejects_invalid_plan_id_without_echoing_it() {
        let (ctx, store, root) = context_with_store();
        let traversal = run_command(&ctx, &["create", "../../etc", "Ziel"]).await;
        let upper = run_command(&ctx, &["create", "My_Plan", "Ziel"]).await;
        let plan = store.current();
        cleanup(root);

        for result in [traversal, upper] {
            match result {
                Err(OpError::InvalidArguments(message)) => {
                    assert!(message.contains("ungültige Plan-ID"), "war: {message}");
                    assert!(!message.contains(".."), "Rohwert/Pfad im Fehlertext: {message}");
                    assert!(!message.contains("My_Plan"), "Rohwert im Fehlertext: {message}");
                }
                other => panic!("erwartet InvalidArguments, war: {other:?}"),
            }
        }
        assert!(plan.is_err(), "eine abgelehnte ID darf keinen Plan anlegen");
    }

    #[test]
    fn test_surface_call_derives_surface_from_parse_path() {
        use crate::explore::ExploreArgs;
        let command = match super::SurfaceCall::<ExploreArgs>::from_raw_args(&toks(&["frage"])) {
            Ok(call) => call,
            Err(error) => panic!("Command-Parse schlug fehl: {error}"),
        };
        assert_eq!(command.surface, CallSurface::Command);
        let model: super::SurfaceCall<ExploreArgs> =
            match serde_json::from_value(serde_json::json!({ "question": "frage" })) {
                Ok(call) => call,
                Err(error) => panic!("JSON-Parse schlug fehl: {error}"),
            };
        assert_eq!(model.surface, CallSurface::Model);
        assert_eq!(
            super::SurfaceCall::<ExploreArgs>::default().surface,
            CallSurface::Model
        );
    }

    // ── Verfügbarkeit ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn missing_plan_store_is_not_available() {
        let (ctx, root) = context_with(ServiceMap::new());
        let result = super::plan(&ctx, PlanCall::from_command(PlanArgs::Inspect)).await;
        cleanup(root);

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("kein Plan-Store"), "war: {message}");
            }
            other => panic!("erwartet NotAvailable, war: {other:?}"),
        }
    }

    #[tokio::test]
    async fn disabled_config_is_not_available_and_names_the_switch() {
        let store: Arc<dyn PlanStore> = Arc::new(InMemoryPlanStore::new());
        let mut services = ServiceMap::new();
        services.insert(store);
        // Der Default der Konfiguration ist bewusst deaktiviert.
        services.insert(PlanToolConfig::default());
        let (ctx, root) = context_with(services);

        let result = super::plan(&ctx, PlanCall::from_command(PlanArgs::Inspect)).await;
        cleanup(root);

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("[tools.plan] enabled"), "war: {message}");
            }
            other => panic!("erwartet NotAvailable, war: {other:?}"),
        }
    }

    // ── Ende-zu-Ende gegen einen echten Store ────────────────────────────────

    #[tokio::test]
    async fn create_add_dep_and_ready_reflect_the_dependency_edge() {
        let (ctx, store, root) = context_with_store();

        let created = run_command(&ctx, &["create", "p-1", "Plan-Op", "verdrahten"]).await;
        let added_first =
            run_command(&ctx, &["add", "t-1", "explore", "Ist-Zustand", "lesen"]).await;
        let added_second = run_command(&ctx, &["add", "t-2", "coding", "Modul", "bauen"]).await;
        let linked = run_command(&ctx, &["dep", "t-2", "t-1"]).await;
        let ready = run_command(&ctx, &["ready"]).await;

        let plan = store.current();
        cleanup(root);

        assert!(created.is_ok(), "create schlug fehl: {created:?}");
        assert!(added_first.is_ok(), "add t-1 schlug fehl: {added_first:?}");
        assert!(
            added_second.is_ok(),
            "add t-2 schlug fehl: {added_second:?}"
        );
        assert!(linked.is_ok(), "dep schlug fehl: {linked:?}");

        match plan {
            Ok(plan) => {
                assert_eq!(plan.id, PlanId::parse("p-1").expect("gültige Test-ID"));
                assert_eq!(plan.nodes.len(), 2);
                let second = plan
                    .nodes
                    .iter()
                    .find(|node| node.id == TaskId::new("t-2"))
                    .map(|node| node.dependencies.clone());
                assert_eq!(second, Some(vec![TaskId::new("t-1")]));
            }
            Err(error) => panic!("Plan lesen schlug fehl: {error}"),
        }

        match ready {
            Ok(text) => {
                assert!(text.contains("t-1"), "t-1 muss ausführbar sein: {text}");
                assert!(
                    !text.contains("- t-2"),
                    "t-2 hängt an t-1 und darf nicht ausführbar sein: {text}"
                );
            }
            Err(error) => panic!("ready schlug fehl: {error}"),
        }
    }

    #[tokio::test]
    async fn actor_carries_the_human_prefix_on_the_command_surface() {
        let (ctx, store, root) = context_with_store();
        let created = run_command(&ctx, &["create", "p-1", "Akteur", "prüfen"]).await;
        let history = store.history(None);
        cleanup(root);

        assert!(created.is_ok(), "create schlug fehl: {created:?}");
        match history {
            Ok(events) => {
                let first = events.first().map(|event| event.actor.clone());
                match first {
                    Some(actor) => assert!(
                        actor.starts_with("human:"),
                        "Command-Fläche muss 'human:' schreiben, war: {actor}"
                    ),
                    None => panic!("History ist leer"),
                }
            }
            Err(error) => panic!("History lesen schlug fehl: {error}"),
        }
    }

    #[tokio::test]
    async fn actor_carries_the_model_prefix_on_the_model_tool_surface() {
        let (ctx, store, root) = context_with_store();
        let result = super::plan(
            &ctx,
            PlanCall::from_model(PlanArgs::Create {
                id: Some("p-1".to_owned()),
                goal: Some("Akteur prüfen".to_owned()),
            }),
        )
        .await;
        let history = store.history(None);
        cleanup(root);

        assert!(result.is_ok(), "create schlug fehl: {result:?}");
        match history {
            Ok(events) => match events.first().map(|event| event.actor.clone()) {
                Some(actor) => assert!(
                    actor.starts_with("model:"),
                    "Modell-Fläche muss 'model:' schreiben, war: {actor}"
                ),
                None => panic!("History ist leer"),
            },
            Err(error) => panic!("History lesen schlug fehl: {error}"),
        }
    }

    #[tokio::test]
    async fn waves_report_the_topological_layers_with_write_set_batches() {
        let (ctx, store, root) = context_with_store();

        let seeded = store
            .apply(
                PlanAction::Create {
                    plan_id: PlanId::parse("p-waves").expect("gültige Test-ID"),
                    goal: "Wellen".to_owned(),
                },
                "test",
            )
            .and_then(|_| {
                store.apply(
                    PlanAction::AddNode {
                        node: super::new_node("t-1", PlanNodeKind::Explore, "erst".to_owned()),
                    },
                    "test",
                )
            })
            .and_then(|_| {
                store.apply(
                    PlanAction::AddNode {
                        node: super::new_node("t-2", PlanNodeKind::Coding, "dann".to_owned()),
                    },
                    "test",
                )
            })
            .and_then(|_| {
                store.apply(
                    PlanAction::AddDependency {
                        child: TaskId::new("t-2"),
                        parent: TaskId::new("t-1"),
                    },
                    "test",
                )
            });

        let waves = run_command(&ctx, &["waves"]).await;
        cleanup(root);

        assert!(seeded.is_ok(), "Fixture schlug fehl: {seeded:?}");
        match waves {
            Ok(text) => {
                assert!(
                    text.contains("2 Ausführungswelle(n)"),
                    "zwei Wellen erwartet: {text}"
                );
                assert!(text.contains("Welle 0"), "Welle 0 fehlt: {text}");
                assert!(text.contains("Welle 1"), "Welle 1 fehlt: {text}");
                assert!(
                    text.contains("WriteSet-disjunkt"),
                    "Batch-Aufteilung fehlt: {text}"
                );
                let wave_zero = text.split("Welle 1").next().unwrap_or_default();
                assert!(
                    wave_zero.contains("t-1"),
                    "t-1 gehört in Welle 0: {wave_zero}"
                );
            }
            Err(error) => panic!("waves schlug fehl: {error}"),
        }
    }

    #[tokio::test]
    async fn reconcile_reports_applied_and_only_proposed_steps() {
        let (ctx, store, root) = context_with_store();

        let seeded = store
            .apply(
                PlanAction::Create {
                    plan_id: PlanId::parse("p-rec").expect("gültige Test-ID"),
                    goal: "Abgleich".to_owned(),
                },
                "test",
            )
            .and_then(|_| {
                store.apply(
                    PlanAction::AddNode {
                        node: super::new_node(
                            "t-1",
                            PlanNodeKind::Docs,
                            "dokumentieren".to_owned(),
                        ),
                    },
                    "test",
                )
            });

        let reconciled = run_command(&ctx, &["reconcile"]).await;
        let plan = store.current();
        cleanup(root);

        assert!(seeded.is_ok(), "Fixture schlug fehl: {seeded:?}");
        match reconciled {
            Ok(text) => {
                assert!(text.starts_with("Abgleich:"), "Kopfzeile fehlt: {text}");
                assert!(
                    text.contains("Offen") || text.contains("Angewandt"),
                    "Bericht muss beide Abschnitte führen: {text}"
                );
                assert!(
                    text.contains("admit-jobs"),
                    "ein Docs-Knoten geht als Job-Vorschlag zurück: {text}"
                );
            }
            Err(error) => panic!("reconcile schlug fehl: {error}"),
        }

        match plan {
            // `AdmitJobs` wird nicht angewandt — der Knoten bleibt Draft.
            Ok(plan) => assert_eq!(
                plan.nodes.first().map(|node| node.status),
                Some(PlanNodeStatus::Draft)
            ),
            Err(error) => panic!("Plan lesen schlug fehl: {error}"),
        }
    }

    #[tokio::test]
    async fn missing_arguments_report_the_usage_line() {
        let (ctx, _store, root) = context_with_store();
        let result = super::plan(
            &ctx,
            PlanCall::from_command(PlanArgs::Create {
                id: None,
                goal: None,
            }),
        )
        .await;
        cleanup(root);

        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("plan create"), "war: {message}");
            }
            other => panic!("erwartet InvalidArguments, war: {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_node_kind_is_rejected_with_the_full_value_list() {
        let (ctx, _store, root) = context_with_store();
        let result = super::plan(
            &ctx,
            PlanCall::from_command(PlanArgs::Add {
                id: Some("t-1".to_owned()),
                kind: Some("zaubern".to_owned()),
                objective: Some("etwas".to_owned()),
            }),
        )
        .await;
        cleanup(root);

        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("zaubern"), "war: {message}");
                assert!(message.contains("coding"), "Werteliste fehlt: {message}");
            }
            other => panic!("erwartet InvalidArguments, war: {other:?}"),
        }
    }

    #[tokio::test]
    async fn inspect_without_a_plan_points_at_plan_create() {
        let (ctx, _store, root) = context_with_store();
        let result = super::plan(&ctx, PlanCall::from_command(PlanArgs::Inspect)).await;
        cleanup(root);

        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("plan create"), "war: {message}");
            }
            other => panic!("erwartet InvalidArguments, war: {other:?}"),
        }
    }

    #[tokio::test]
    async fn patch_sets_the_write_scope_from_a_comma_list() {
        let (ctx, store, root) = context_with_store();

        let created = run_command(&ctx, &["create", "p-1", "Patch", "prüfen"]).await;
        let added = run_command(&ctx, &["add", "t-1", "coding", "bauen"]).await;
        let patched =
            run_command(&ctx, &["patch", "t-1", "write-scope", "src/a.rs,src/b.rs"]).await;
        let plan = store.current();
        cleanup(root);

        assert!(created.is_ok(), "create schlug fehl: {created:?}");
        assert!(added.is_ok(), "add schlug fehl: {added:?}");
        assert!(patched.is_ok(), "patch schlug fehl: {patched:?}");
        match plan {
            Ok(plan) => {
                let scope = plan
                    .nodes
                    .first()
                    .map(|node| super::join_scope(&node.write_scope));
                assert_eq!(scope.as_deref(), Some("src/a.rs, src/b.rs"));
            }
            Err(error) => panic!("Plan lesen schlug fehl: {error}"),
        }
    }
}
