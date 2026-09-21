//! `harw-ops` — konkrete Operationen (Commands + Modell-Tools) für Harwness.
//!
//! Diese Crate enthält die Kern-Operationen, die über den
//! [`harw_operations::Operation`]-Contract exponiert werden. Jede Operation
//! lebt in ihrer eigenen Datei und nutzt das `#[operation(...)]`-Makro aus
//! [`harw_macros`], um sowohl den [`harw_operations::Operation`]-Impl als auch
//! die zugehörige [`harw_operations::OperationMeta`] zu generieren.
//!
//! # Verantwortungsbereich
//! - Definiert die konkreten Op-Structs (z. B. `HelpOperation`, `StatusOperation`).
//! - Erzeugt statische [`harw_operations::OperationMeta`] mit Surface-Matrix
//!   gemäß Plan v2 (Command / ModelTool / Approval / ReadOnly / Visibility).
//! - Kennt **keine** Registry- oder Adapter-Logik — die Ops werden extern
//!   registriert (z. B. via `inventory::submit!` durch Extension-Crate).
//!
//! # Op-Set
//! **Grundausstattung** ([`register_all`], 36 Ops): `help`, `status`, `quit`,
//! `new`, `work`, `ps`, `attach`, `stop`, `diff`, `agent`, `skills`, `plugins`,
//! `model`, `provider`, `uia-model`, `uia-provider`, `permissions`, `compact`,
//! `memory`, `effort`, `mode`, `context-proposal`, `approval.pending`,
//! `approval.resolve`, `add-workdir`, `export`, `usage`, `bug-report`,
//! `approve`, `deny`, `review`, `cancel`, `retry`, `provider-concurrency`,
//! `uia-worker-model`, `uia-effort`.
//! `uia-worker-model` (Welle 2, harw-ops/src/model.rs) und `uia-effort`
//! (harw-ops/src/effort.rs) waren implementiert, aber bis zu diesem Knoten
//! nicht in `register_all` eingetragen — dadurch existierten `/uia-worker-model`
//! und `/uia-effort` in der TUI nicht, obwohl der Picker für
//! `/uia-worker-model` bereits `switch <id>` dagegen sendet. Sie sind
//! strukturelle Zwillinge von `uia-model`/`uia-provider`: eigene, vom
//! Session-Default unabhängige Pins (`uia_worker_model`/`uia_effort`), die
//! erst ab der nächsten UIA-Worker-Sitzung wirken.
//! `provider-concurrency` (Welle 6b) zeigt/verstellt die harte,
//! client-seitige Nebenläufigkeitsgrenze eines Providers live und ist —
//! anders als `provider`, das bewusst kein `model_tool` trägt — auch der UIA
//! zugänglich, damit sie selbst auf wiederholte HTTP-429-Antworten reagieren
//! kann (siehe `crate::provider`-Moduldoku).
//! `bug-report` schreibt einen minimalen, manuell ausgelösten lokalen
//! Bug-Report nach `<home>/bug-report/` (kein Netzwerk-Versand) — die
//! automatische Incident-Erkennung ist ein separates, noch ausstehendes
//! Arbeitspaket, siehe `crate::bug_report`-Moduldoku.
//! `approve`/`deny`/`review`/`cancel`/`retry` (Interaktionsvertrag §2.3/§4)
//! bilden die Genehmigungs-Befehlsgruppe: `approve` genehmigt einen wegen
//! einer Freigabe blockierten Job (`JobStore::unblock`, inklusive Akteur und
//! optionalem Freitext als Audit-Sidecar, siehe `crate::approve`-Moduldoku),
//! `deny` lehnt ihn ab — über `JobStore::deny_blocked` für einen `Blocked`-Job,
//! sonst über `JobStore::cancel` (siehe `crate::deny`-Moduldoku) —, `review`
//! zeigt den vollständigen Job-Zustand, `cancel` bricht ihn ab (siehe
//! `crate::cancel`-Moduldoku zum Verhältnis zu `stop`), `retry` fordert einen
//! terminal `Failed`/`Cancelled`-Job über `JobStore::retry` erneut an, mit
//! typisiertem Fehler statt stillem Requeue, sobald die Retry-Policy erschöpft
//! ist (siehe `crate::retry`-Moduldoku). Keine der fünf trägt ein `ModelTool`
//! außer `cancel` (siehe die jeweilige Moduldoku): das laufende Modell darf
//! eine gegen seine eigenen Werkzeugaufrufe gerichtete Genehmigungs-
//! entscheidung nicht selbst treffen.
//! `uia-model`/`uia-provider` are structural twins of `model`/`provider` that
//! read/write `uia_model`/`uia_provider` instead of
//! `default_model`/`default_provider`, so the UIA can pin its own selection
//! independent of the session default.
//! `usage` liest `harw_core::state_store::SessionStateSnapshot::total_usage`
//! über `harw_core_bridge::OpContextCoreExt::state_store` — reine
//! Session-Introspektion wie `mode`/`context-proposal`, deshalb ebenfalls
//! Grundausstattung statt Planungsfläche.
//! `add-workdir` (Slice B4, Contract §2 A8) legt eine zusätzliche
//! Workspace-Wurzel für die Sitzung frei (`harw_sandbox::ExtraRootsCell`),
//! optional dauerhaft fürs Projekt gemerkt; `export` (Slice B4, „Nachträgliche
//! Entscheidungen") liefert nur den `data`-Marker für den TUI-Exportdialog
//! (siehe `crate::export`-Moduldoku) — beide sind Sitzungs-/Oberflächen-
//! Steuerung wie `mode`, nicht Recherche/Plan, und stehen deshalb in der
//! Grundausstattung statt hinter dem `[tools.plan]`-Gate.
//! `context-proposal` (Knoten AW5-09) steht hier statt in
//! der Planungsfläche, weil es genau wie `/mode` keine Recherche/Plan-Aktion
//! ist, sondern eine Operator-Governance-Fläche auf
//! `harw_knowledge::context_proposal::ContextProposal` (list/view/accept/reject;
//! „annehmen" markiert, wendet nie an) — sie gehört nicht zum
//! `explore`/`research_*`/`plan`/`goal`/`analyze`-Controller-Kreis und darf
//! deshalb nicht hinter dem `[tools.plan] enabled`-Gate verschwinden. Sie
//! benötigt einen `Arc<harw_knowledge::KnowledgeStore>`-Service im `OpContext`.
//! `approval.pending`/`approval.resolve` (siehe `approval`-Moduldoku) machen
//! die Bestätigungsfläche über `Surface::Web` erreichbar — sie tragen aus
//! demselben Grund keine `ModelTool`-Fläche wie `mode`/`context-proposal`
//! (das laufende Modell darf seine eigene Bestätigungsfläche nicht bedienen)
//! und stehen ebenfalls in der Grundausstattung, nicht hinter dem
//! `[tools.plan]`-Gate.
//!
//! **Planungsfläche** ([`register_plan_tools`], 6 Ops, hinter dem
//! `[tools.plan] enabled`-Gate): `plan`, `goal`, `explore`, `research_deps`,
//! `research_web`, `analyze`. Sie bilden zusammen den Controller-Kreis —
//! Recherche erzeugt Findings, `plan reconcile` macht Evidenz daraus, `goal
//! check` wertet sie gegen die Kriterien aus.
//!
//! # Web-Fläche (`Surface::Web`) — nach welcher Regel entschieden wurde
//! `Surface::Web` existiert seit UI-00 (samt `WebAdapter`/`WebRouteTable` in
//! `harw-operations`), wurde aber von keiner Operation deklariert — der
//! `#[operation(...)]`-Makro kannte bis zu diesem Knoten kein `web(...)`-
//! Unterattribut überhaupt (siehe `harw-macros/src/operation.rs`). **Eine
//! Web-Fläche ist kein Automatismus.** Sie wird nicht vergeben, weil eine
//! Operation zufällig ein `Command`- oder `ModelTool`-Surface trägt, sondern
//! nur, wenn sich für genau diese Operation begründen lässt:
//!
//! 1. **Ist sie wirklich lesend?** `method: WebMethod::Get` steht nur an
//!    Operationen, deren Rumpf nachweislich nichts verändert (reine
//!    Auflistungen, Ansichten, read-only-Kindagenten). Ein `GET`, das mutiert,
//!    wird von Zwischenschichten (Prefetch, Retry, Cache) still ausgenutzt.
//! 2. **Welche `ApprovalPolicy`?** Wiederverwendet ausschließlich das Feld, das
//!    `Surface::ModelTool` bereits kennt — keine eigene Web-Autoritätsachse.
//!    Irreversible Aktionen (`stop`, `plan`, `goal`) bekommen `approval =
//!    "always"`, identisch zu ihrer ModelTool-Deklaration.
//! 3. **`PermissionTier` bleibt unverändert** — sie stammt vom erbauenden
//!    Knoten und wird nicht angehoben oder abgesenkt, nur weil eine weitere
//!    Fläche hinzukommt.
//! 4. **Ausschluss bei gemischtem Dispatch.** Operationen, deren Callback
//!    sowohl lesende als auch mutierende Sub-Kommandos über denselben
//!    Aufrufpfad bedient (`memory`, `context-proposal`, `agent`, `plugins`,
//!    `skills`) und die deshalb schon keine `ModelTool`-Fläche haben, bekommen
//!    aus demselben Grund auch keine Web-Fläche: weder ein pauschales
//!    `method: WebMethod::Get` (würde die mutierenden Sub-Kommandos falsch
//!    labeln) noch ein pauschales `approval` auf die lesenden Sub-Kommandos
//!    (unnötige Bestätigungspflicht) ist korrekt, ohne die Operation in
//!    getrennte Surfaces aufzuteilen — das liegt außerhalb dieses Knotens.
//! 5. **Ausschluss bei fail-closed-Stubs.** Operationen, die heute für jede
//!    Aktion `OpError::NotAvailable` liefern (`new`, `skills`, `plugins`,
//!    `compact`), bekommen keine Web-Fläche — es gäbe nichts zu bedienen.
//! 6. **Ausschluss bei Selbstautoritäts-Operationen.** `mode`, `model`,
//!    `provider` und `effort` verweigern bewusst schon die `ModelTool`-Fläche,
//!    weil das laufende Sprachmodell seine eigene Steuerung nicht selbst
//!    ändern darf; `mode` mutiert zusätzlich noch gar nichts (siehe dessen
//!    Moduldoku, `CONTROLLER_LACKS_MODE`). Für Web gilt dieselbe Vorsicht aus
//!    demselben Motiv wie Punkt 4 (gemischter Dispatch) plus der zusätzlichen
//!    Sorge, eine Wirkung vorzutäuschen, die es noch nicht gibt.
//!
//! Die vollständige Tabelle der exponierten und der bewusst nicht exponierten
//! Operationen mit Begründung steht im Abschlussbericht dieses Knotens; die
//! `#[operation(...)]`-Aufrufe der exponierten Operationen tragen je einen
//! Kommentar mit derselben Begründung direkt am `web(...)`-Unterattribut.
//!
//! # Nebenläufigkeit
//! Alle Op-Structs sind `Send + Sync`, weil das `#[operation]`-Makro Unit-Structs
//! ohne inneren Zustand erzeugt.

#![forbid(unsafe_code)]

pub mod add_workdir;
pub mod agent;
pub mod analyze;
pub mod approval;
pub mod approve;
pub mod attach;
pub mod bug_report;
pub mod cancel;
pub mod compact;
pub(crate) mod config_util;
pub mod context_proposal;
pub mod deny;
pub mod diff;
pub mod effort;
pub mod explore;
pub mod export;
pub mod goal;
pub mod help;
pub mod memory;
pub mod mode;
pub mod model;
pub mod new;
pub mod permissions;
pub mod plan;
pub mod plugins;
pub mod provider;
pub mod ps;
pub mod quit;
pub mod research;
pub mod retry;
pub mod review;
pub mod skills;
pub mod status;
pub mod stop;
#[cfg(test)]
pub(crate) mod testutil;
pub mod usage;
pub mod work;

use std::sync::{Arc, OnceLock};

use harw_operations::operation::BusyAvailability;
use harw_operations::registry::OperationRegistry;
use harw_operations::{
    CommandVisibility, OpContext, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
    OperationDomain, OperationMeta, PermissionTier, Surface,
};
use harw_plan::config::PlanToolConfig;

/// Registry-facing replacement for the unwired `/compact` implementation.
///
/// The underlying placeholder correctly returns `NotAvailable`, but its generated
/// metadata advertises a completed session-compaction action. The command
/// registry must instead describe its actual availability and return a direct,
/// renderable response on every adapter surface.
struct UnavailableCompactOperation;

impl Operation for UnavailableCompactOperation {
    fn meta(&self) -> &OperationMeta {
        static META: OnceLock<OperationMeta> = OnceLock::new();
        META.get_or_init(|| OperationMeta {
            name: "compact",
            summary: "Session-Komprimierung ist in dieser Laufzeit nicht verfügbar.",
            domain: OperationDomain::Session,
            permission: PermissionTier::Operator,
            surfaces: vec![Surface::Command {
                path: "/compact",
                visibility: CommandVisibility::ChannelParity,
            }],
            aliases: &[],
            category: OperationCategory::Session,
            args_schema: None,
            output_schema: None,
            busy: BusyAvailability::DeferredUntilTurnEnd,
        })
    }

    fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
        Box::pin(async { Ok(compact_unavailable_output()) })
    }
}

fn compact_unavailable_output() -> OpOutput {
    OpOutput::from(crate::compact::COMPACT_HINT.to_owned())
}

/// Registriert alle 36 in dieser Crate definierten Kern-Operationen in der Registry.
///
/// # Beschreibung
/// Fügt der übergebenen [`OperationRegistry`] eine `Arc<dyn Operation>`-Instanz
/// jeder konkreten Op-Struct hinzu — jeweils genau einmal, in fester Reihenfolge:
/// `help, status, quit, new, work, ps, attach, stop, diff, agent, skills,
/// plugins, model, provider, uia-model, uia-provider, permissions, compact,
/// memory, effort, mode, context-proposal, approval.pending, approval.resolve,
/// add-workdir, export, usage, bug-report, approve, deny, review, cancel, retry,
/// provider-concurrency, uia-worker-model, uia-effort`.
///
/// Die Reihenfolge steuert nur die `iter()`-Reihenfolge und den Fallback-Namens-
/// Vorschlag; die eigentliche Auflösung erfolgt über `find_by_name` /
/// `find_by_command`.
///
/// # Argumente
/// - `registry` (`&mut OperationRegistry`): Ziel-Registry, die erweitert wird.
///   Bereits registrierte Operationen bleiben erhalten (kein Deduplizieren).
///
/// # Nebenläufigkeit
/// Erfordert `&mut`-Zugriff auf die Registry. Für parallele Aufbau-Szenarien
/// muss der Aufrufer synchronisieren (z. B. `OnceLock<Arc<OperationRegistry>>`).
///
/// # Beispiel
/// ```rust
/// use harw_operations::registry::OperationRegistry;
///
/// let mut registry = OperationRegistry::new();
/// harw_ops::register_all(&mut registry);
/// assert_eq!(registry.len(), 36);
/// assert!(registry.find_by_name("help").is_some());
/// assert!(registry.find_by_command("/uia-provider").is_some());
/// assert!(registry.find_by_command("/uia-model").is_some());
/// assert!(registry.find_by_command("/uia-worker-model").is_some());
/// assert!(registry.find_by_command("/uia-effort").is_some());
/// assert!(registry.find_by_command("/status").is_some());
/// assert!(registry.find_by_command("/effort").is_some());
/// assert!(registry.find_by_command("/mode").is_some());
/// assert!(registry.find_by_command("/context-proposal").is_some());
/// assert!(registry.find_by_name("approval.pending").is_some());
/// assert!(registry.find_by_name("approval.resolve").is_some());
/// assert!(registry.find_by_command("/add-workdir").is_some());
/// assert!(registry.find_by_command("/export").is_some());
/// assert!(registry.find_by_command("/usage").is_some());
/// assert!(registry.find_by_command("/bug-report").is_some());
/// assert!(registry.find_by_command("/approve").is_some());
/// assert!(registry.find_by_command("/deny").is_some());
/// assert!(registry.find_by_command("/review").is_some());
/// assert!(registry.find_by_command("/cancel").is_some());
/// assert!(registry.find_by_command("/retry").is_some());
/// ```
pub fn register_all(registry: &mut OperationRegistry) {
    let ops: [Arc<dyn Operation>; 36] = [
        Arc::new(help::HelpOperation),
        Arc::new(status::StatusOperation),
        Arc::new(quit::QuitOperation),
        Arc::new(new::NewOperation),
        Arc::new(work::WorkOperation),
        Arc::new(ps::PsOperation),
        Arc::new(attach::AttachOperation),
        Arc::new(stop::StopOperation),
        Arc::new(diff::DiffOperation),
        Arc::new(agent::AgentOperation),
        Arc::new(skills::SkillsOperation),
        Arc::new(plugins::PluginsOperation),
        Arc::new(model::ModelOperation),
        Arc::new(provider::ProviderOperation),
        // UIA-specific pinned provider/model selection (`uia_provider`/
        // `uia_model`), independent of `default_provider`/`default_model` —
        // structural twins of `model`/`provider` above, registered next to
        // them for the same reason.
        Arc::new(model::UiaModelOperation),
        Arc::new(provider::UiaProviderOperation),
        Arc::new(permissions::PermissionsOperation),
        Arc::new(UnavailableCompactOperation),
        Arc::new(memory::MemoryOperation),
        Arc::new(effort::EffortOperation),
        // `/mode` steuert die Session, nicht die Planungsfläche: es gehört zur
        // Grundausstattung und hängt bewusst *nicht* am `[tools.plan]`-Gate.
        // Ohne registrierten SessionController meldet es das ehrlich.
        Arc::new(mode::ModeOperation),
        // `/context-proposal` ist Operator-Governance auf Kontextprogramm-
        // Vorschlägen, kein Recherche-/Plan-Schritt: es gehört genau wie
        // `/mode` zur Grundausstattung und nicht hinter das `[tools.plan]`-
        // Gate (siehe Moduldoku, Abschnitt „Op-Set").
        Arc::new(context_proposal::ContextProposalOperation),
        // Bestätigungsfläche (UI-06-Folgeknoten): macht `harw_web::security`
        // über `Surface::Web` erreichbar, ohne selbst zu autorisieren (siehe
        // `approval`-Moduldoku). Grundausstattung statt Planungsfläche, aus
        // demselben Grund wie `mode`/`context-proposal` oben.
        Arc::new(approval::ApprovalPendingOperation),
        Arc::new(approval::ApprovalResolveOperation),
        // Slice B4 (Contract §2 A8): Sitzungs-/Oberflächen-Steuerung wie
        // `mode`/`context-proposal` oben — Grundausstattung, nicht hinter
        // dem `[tools.plan]`-Gate.
        Arc::new(add_workdir::AddWorkdirOperation),
        Arc::new(export::ExportOperation),
        // `/usage` (Agent OPS): liest `SessionStateSnapshot::total_usage` über
        // `OpContextCoreExt::state_store()` — reine Session-Introspektion wie
        // `mode`/`context-proposal` oben, deshalb Grundausstattung statt
        // Planungsfläche.
        Arc::new(usage::UsageOperation),
        // Manueller lokaler Bug-Report-Fallback (siehe `crate::bug_report`-
        // Moduldoku): reine Session-/Diagnose-Fläche wie `usage` oben,
        // deshalb Grundausstattung statt Planungsfläche.
        Arc::new(bug_report::BugReportOperation),
        // Genehmigungs-Befehlsgruppe (Interaktionsvertrag §2.3/§4): siehe
        // Moduldoku, Abschnitt „Op-Set", und die jeweiligen Modul-Dokus für
        // Umfang und bekannte Lücken gegenüber dem Vertrag.
        Arc::new(approve::ApproveOperation),
        Arc::new(deny::DenyOperation),
        Arc::new(review::ReviewOperation),
        Arc::new(cancel::CancelOperation),
        Arc::new(retry::RetryOperation),
        // W6b — UIA-Sichtbarkeit auf Provider-Concurrency/Rate-Limit-Zustand +
        // Live-Anpassung (siehe `crate::provider`-Moduldoku): eigene Operation
        // statt Unterbefehl von `/provider`, weil `/provider` bewusst KEIN
        // `model_tool` trägt ("Provider switches are exclusively permitted as
        // operator commands") — die Concurrency-Anpassung dagegen MUSS auch
        // der UIA selbst zugänglich sein (Reaktion auf beobachtete 429).
        Arc::new(provider::ProviderConcurrencyOperation),
        // Structural twin of `uia-model`/`uia-provider` above: pins
        // `uia_worker_model` (independent of `default_model`/`uia_model`),
        // effective from the next UIA-worker session. Was implemented but
        // never registered until this node, so `/uia-worker-model` —
        // including the TUI picker's `switch <id>` follow-up — ran into an
        // unknown command.
        Arc::new(model::UiaWorkerModelOperation),
        // Structural twin of `uia-model`/`uia-worker-model` above: pins
        // `uia_effort`, independent of the session's default reasoning
        // effort. Same registration gap and fix as `uia-worker-model`.
        Arc::new(effort::UiaEffortOperation),
    ];
    for op in ops {
        registry.register(op);
    }
}

/// Anzahl der Operationen, die [`register_plan_tools`] bei aktivem Gate hinzufügt.
pub const PLAN_TOOL_COUNT: usize = 6;

/// Registriert die Planungs-, Explorations- und Recherche-Operationen — gegated.
///
/// # Beschreibung
/// Fügt der Registry sechs Operationen hinzu: `plan`, `goal`, `explore`,
/// `research_deps`, `research_web` und `analyze`. Ist `config.enabled` `false`,
/// wird **nichts** registriert und die Funktion ist ein No-op.
///
/// Das Gate wirkt damit *vor* dem Modell: eine nicht registrierte Operation
/// erscheint gar nicht erst in der Werkzeugliste eines `ModelRequest`. Das ist
/// strukturell stärker als eine Laufzeitprüfung im Op-Rumpf — es gibt keine
/// Aufrufmöglichkeit, statt einen abgelehnten Aufruf. Die Operationen prüfen
/// `enabled` **zusätzlich** in ihrem Rumpf (fail-closed, defense in depth), für
/// den Fall, dass eine Laufzeit sie an diesem Gate vorbei registriert.
///
/// Die sechs Operationen stehen bewusst zusammen: `explore` und `research_*`
/// erzeugen die Findings, die `plan reconcile` zu Evidenz macht, und `analyze`
/// schreibt seine Ergebnisse in denselben Plan. Ohne Plan-Store wäre die
/// Recherche folgenlos — sie zusammen zu schalten hält den Kreis geschlossen.
///
/// # Argumente
/// - `registry` (`&mut OperationRegistry`): Ziel-Registry, die erweitert wird.
/// - `config` (`&PlanToolConfig`): Gate; nur `enabled` wird hier gelesen. Die
///   übrigen Felder (`require_exploration_for`, `exploration_ttl_secs`,
///   `max_expand_depth`) wertet der Plan-Store bei der Validierung aus.
///
/// # Rückgabe
/// Die Anzahl tatsächlich registrierter Operationen: [`PLAN_TOOL_COUNT`] oder `0`.
///
/// # Nebenläufigkeit
/// Erfordert `&mut`-Zugriff auf die Registry; der Aufrufer synchronisiert.
///
/// # Beispiel
/// ```rust
/// use harw_operations::registry::OperationRegistry;
/// use harw_plan::config::PlanToolConfig;
///
/// let mut registry = OperationRegistry::new();
/// harw_ops::register_all(&mut registry);
///
/// // Ausgeschaltet: nichts kommt hinzu.
/// let added = harw_ops::register_plan_tools(&mut registry, &PlanToolConfig::default());
/// assert_eq!(added, 0);
/// assert!(registry.find_by_command("/plan").is_none());
///
/// // Eingeschaltet: die Planungsfläche erscheint.
/// let enabled = PlanToolConfig::enabled_defaults();
/// let added = harw_ops::register_plan_tools(&mut registry, &enabled);
/// assert_eq!(added, harw_ops::PLAN_TOOL_COUNT);
/// assert!(registry.find_by_command("/plan").is_some());
/// assert!(registry.find_by_command("/goal").is_some());
/// assert!(registry.find_by_command("/analyze").is_some());
/// ```
pub fn register_plan_tools(registry: &mut OperationRegistry, config: &PlanToolConfig) -> usize {
    if !config.enabled {
        return 0;
    }
    let ops: [Arc<dyn Operation>; PLAN_TOOL_COUNT] = [
        Arc::new(plan::PlanOperation),
        Arc::new(goal::GoalOperation),
        Arc::new(explore::ExploreOperation),
        Arc::new(research::ResearchDepsOperation),
        Arc::new(research::ResearchWebOperation),
        Arc::new(analyze::AnalyzeOperation),
    ];
    for op in ops {
        registry.register(op);
    }
    PLAN_TOOL_COUNT
}

#[cfg(test)]
mod tests {
    use super::{compact_unavailable_output, register_all};
    use harw_operations::operation::WebMethod;
    use harw_operations::registry::OperationRegistry;
    use harw_operations::{ApprovalPolicy, PermissionTier, Surface};
    use harw_plan::config::PlanToolConfig;

    /// Baut eine Registry mit allen Operationen inklusive der gegateten
    /// Planungsfläche — die Ausgangslage für alle `Surface::Web`-Tests unten.
    fn full_registry() -> OperationRegistry {
        let mut reg = OperationRegistry::new();
        register_all(&mut reg);
        super::register_plan_tools(&mut reg, &PlanToolConfig::enabled_defaults());
        reg
    }

    /// Erwartete `Surface::Web`-Deklaration: Operationsname, Pfad, `method`
    /// und `approval`. Die genehmigende Begründung für jede Zeile steht am
    /// jeweiligen `#[operation(...)]`-Aufruf in der Op-Datei (siehe dort).
    const EXPECTED_WEB_SURFACES: &[(&str, &str, WebMethod, ApprovalPolicy)] = &[
        ("help", "/api/help", WebMethod::Get, ApprovalPolicy::None),
        ("status", "/api/status", WebMethod::Get, ApprovalPolicy::None),
        ("ps", "/api/ps", WebMethod::Get, ApprovalPolicy::None),
        ("diff", "/api/diff", WebMethod::Get, ApprovalPolicy::None),
        ("work", "/api/work", WebMethod::Get, ApprovalPolicy::None),
        ("attach", "/api/attach", WebMethod::Get, ApprovalPolicy::None),
        (
            "permissions",
            "/api/permissions",
            WebMethod::Post,
            ApprovalPolicy::Always,
        ),
        (
            "explore",
            "/api/explore",
            WebMethod::Get,
            ApprovalPolicy::None,
        ),
        (
            "research_deps",
            "/api/research-deps",
            WebMethod::Get,
            ApprovalPolicy::None,
        ),
        (
            "research_web",
            "/api/research-web",
            WebMethod::Get,
            ApprovalPolicy::None,
        ),
        // `analyze` deklarierte vor W3-M `readonly`, obwohl sein Rumpf dauerhaft
        // in den Plan-Store schreibt und einen Fan-out startet (F-031,
        // Register `x-findings-register-w1-w3.md:359`). Agent OPS-1 hat das in
        // seinem Owned-File `harw-ops/src/analyze.rs` bereits auf
        // `method = "post"` korrigiert (siehe `ledger/W3M/OPS-1.md` §3.1) —
        // diese Tabelle folgt dem tatsächlichen, korrigierten Code, nicht der
        // alten (falschen) `readonly`-Deklaration.
        (
            "analyze",
            "/api/analyze",
            WebMethod::Post,
            ApprovalPolicy::None,
        ),
        ("stop", "/api/stop", WebMethod::Post, ApprovalPolicy::Always),
        ("plan", "/api/plan", WebMethod::Post, ApprovalPolicy::Always),
        ("goal", "/api/goal", WebMethod::Post, ApprovalPolicy::Always),
        (
            "approval.pending",
            "/api/approval-pending",
            WebMethod::Get,
            ApprovalPolicy::None,
        ),
        (
            "approval.resolve",
            "/api/approval-resolve",
            WebMethod::Post,
            ApprovalPolicy::None,
        ),
    ];

    #[test]
    fn every_exposed_operation_appears_with_its_web_path_in_the_registry() {
        // Der Prüfstein dieses Knotens: `Surface::Web` existierte seit UI-00,
        // aber keine reale Operation deklarierte ihn — der Typgenerator für
        // `webui/lib/generated/operations.ts` sah `[] as const`. Dieser Test
        // belegt, dass jede hier als exponiert entschiedene Operation jetzt
        // tatsächlich über die Registry mit ihrem Web-Pfad auffindbar ist.
        let reg = full_registry();
        for (name, path, method, approval) in EXPECTED_WEB_SURFACES {
            let op = reg
                .find_by_name(name.trim())
                .unwrap_or_else(|| panic!("operation '{name}' must be registered"));
            let mut found = false;
            for surface in &op.meta().surfaces {
                if let Surface::Web {
                    path: p,
                    method: m,
                    approval: a,
                } = surface
                {
                    if *p == *path && *m == *method && *a == *approval {
                        found = true;
                    }
                }
            }
            assert!(
                found,
                "operation '{name}' must declare Surface::Web {{ path: \"{path}\", method: {method:?}, approval: {approval:?} }}"
            );
        }
    }

    #[test]
    fn web_surfaces_use_get_only_for_operations_that_do_not_mutate() {
        // Die Gegenprobe zur obigen Tabelle: alle als `WebMethod::Get`
        // deklarierten Web-Flächen gehören zu Operationen, die laut ihrer
        // eigenen Moduldoku nichts verändern (reine Auflistungen/Ansichten/
        // read-only-Kindagenten); die mutierenden Operationen (`stop`, `plan`,
        // `goal`, `approval.resolve`, `permissions`, `analyze`) stehen bewusst
        // mit `WebMethod::Post` in der Tabelle. `permissions` ist seit der
        // Umschaltung des Freigabemodus (`/permissions set`) darunter;
        // `analyze` seit dem F-031-Sicherheitsfix (siehe Kommentar an der
        // Tabellenzeile oben) — es persistiert dauerhaft im Plan-Store und
        // startet einen Fan-out, trotz vormals falsch deklariertem `readonly`.
        for (name, _path, method, _approval) in EXPECTED_WEB_SURFACES {
            let is_mutating = matches!(
                *name,
                "stop" | "plan" | "goal" | "approval.resolve" | "permissions" | "analyze"
            );
            let expected = if is_mutating {
                WebMethod::Post
            } else {
                WebMethod::Get
            };
            assert_eq!(
                *method, expected,
                "operation '{name}': web method must match its actual mutation behaviour"
            );
        }
    }

    #[test]
    fn web_surfaces_carry_the_same_permission_tier_as_the_operation() {
        // "Die PermissionTier bleibt unverändert" — jede Web-Fläche teilt sich
        // ein einziges `OperationMeta` mit den übrigen Flächen derselben
        // Operation, es gibt keinen zweiten, laxeren Berechtigungspfad.
        let reg = full_registry();
        let expected_permissions: &[(&str, PermissionTier)] = &[
            ("help", PermissionTier::Observer),
            ("status", PermissionTier::Observer),
            ("ps", PermissionTier::Observer),
            ("diff", PermissionTier::Observer),
            ("work", PermissionTier::Observer),
            ("attach", PermissionTier::Operator),
            ("permissions", PermissionTier::Operator),
            ("explore", PermissionTier::Operator),
            ("research_deps", PermissionTier::Operator),
            ("research_web", PermissionTier::Operator),
            ("analyze", PermissionTier::Operator),
            ("stop", PermissionTier::Operator),
            ("plan", PermissionTier::Operator),
            ("goal", PermissionTier::Operator),
            ("approval.pending", PermissionTier::Observer),
            ("approval.resolve", PermissionTier::Operator),
        ];
        for (name, tier) in expected_permissions {
            let op = reg.find_by_name(name.trim()).unwrap_or_else(|| {
                panic!("operation '{name}' must be registered");
            });
            assert_eq!(
                op.meta().permission,
                *tier,
                "operation '{name}' permission tier must arrive unchanged"
            );
        }
    }

    #[test]
    fn no_two_web_surfaces_share_a_path_and_registration_does_not_panic() {
        // `register_all`/`register_plan_tools` rufen intern `register()` auf,
        // das bei einer `RegistryError::WebPathCollision` panikt (siehe
        // `harw_operations::registry`). Dass dieser Test überhaupt zu Ende
        // läuft, ist bereits der Beweis, dass keine zwei der hier deklarierten
        // Operationen denselben `/api/...`-Pfad beanspruchen. Zusätzlich eine
        // explizite Eindeutigkeitsprüfung über alle registrierten Web-Pfade.
        let reg = full_registry();
        let mut seen = std::collections::BTreeSet::new();
        for op in reg.iter() {
            for surface in &op.meta().surfaces {
                if let Surface::Web { path, .. } = surface {
                    assert!(
                        seen.insert(*path),
                        "web path '{path}' is declared by more than one operation"
                    );
                }
            }
        }
        assert_eq!(seen.len(), EXPECTED_WEB_SURFACES.len());
    }

    #[test]
    fn negative_model_tool_checks_are_unaffected_by_web_exposure() {
        // Die bestehenden Negativprüfungen (welche Operation KEINE
        // ModelTool-Fläche hat) leben in den jeweiligen Op-Dateien
        // (`mode.rs`, `context_proposal.rs`, `memory.rs`, `plugins.rs`,
        // `skills.rs`) und wurden von diesem Knoten nicht angefasst — dieser
        // Test dokumentiert nur, dass keine dieser Operationen versehentlich
        // eine Web-Fläche bekommen hat, die dieselbe Ausnahme unterlaufen
        // würde.
        let reg = full_registry();
        for name in ["mode", "context-proposal", "memory", "plugins", "skills"] {
            let op = reg
                .find_by_name(name)
                .unwrap_or_else(|| panic!("operation '{name}' must be registered"));
            assert!(
                !op.meta()
                    .surfaces
                    .iter()
                    .any(|s| matches!(s, Surface::Web { .. })),
                "operation '{name}' must not have gained a Web surface"
            );
        }
    }

    #[test]
    fn register_all_adds_thirty_six_operations() {
        let mut reg = OperationRegistry::new();
        register_all(&mut reg);
        assert_eq!(reg.len(), 36);
    }

    #[test]
    fn register_plan_tools_is_a_no_op_while_the_gate_is_closed() {
        let mut reg = OperationRegistry::new();
        register_all(&mut reg);
        let before = reg.len();

        let added = super::register_plan_tools(&mut reg, &PlanToolConfig::default());

        assert_eq!(added, 0, "geschlossenes Gate darf nichts registrieren");
        assert_eq!(reg.len(), before, "Registry darf nicht wachsen");
        for path in ["/plan", "/goal", "/explore", "/analyze"] {
            assert!(
                reg.find_by_command(path).is_none(),
                "{path} darf bei geschlossenem Gate nicht auffindbar sein",
            );
        }
    }

    #[test]
    fn register_plan_tools_adds_the_planning_surface_when_enabled() {
        let mut reg = OperationRegistry::new();
        register_all(&mut reg);
        let before = reg.len();

        let added = super::register_plan_tools(&mut reg, &PlanToolConfig::enabled_defaults());

        assert_eq!(added, super::PLAN_TOOL_COUNT);
        assert_eq!(reg.len(), before + super::PLAN_TOOL_COUNT);
        for path in [
            "/plan",
            "/goal",
            "/explore",
            "/research-deps",
            "/research-web",
            "/analyze",
        ] {
            assert!(
                reg.find_by_command(path).is_some(),
                "{path} wurde nicht registriert",
            );
        }
    }

    #[test]
    fn mode_is_registered_unconditionally_not_behind_the_plan_gate() {
        // `/mode` steuert die Session, nicht die Planungsfläche. Läge es hinter
        // dem Gate, könnte eine Laufzeit ohne Plan-Store den Modus nicht mehr
        // wechseln — inklusive des Wechsels zurück nach `work`.
        let mut reg = OperationRegistry::new();
        register_all(&mut reg);
        assert!(reg.find_by_command("/mode").is_some());
    }

    #[test]
    fn register_all_registers_by_command_path() {
        let mut reg = OperationRegistry::new();
        register_all(&mut reg);
        for path in [
            "/help",
            "/status",
            "/quit",
            "/new",
            "/work",
            "/ps",
            "/attach",
            "/stop",
            "/diff",
            "/agent",
            "/skills",
            "/plugins",
            "/model",
            "/provider",
            "/permissions",
            "/compact",
            "/effort",
            "/uia-worker-model",
            "/uia-effort",
            "/mode",
            "/context-proposal",
            "/add-workdir",
            "/export",
            "/usage",
            "/approve",
            "/deny",
            "/review",
            "/cancel",
            "/retry",
        ] {
            assert!(
                reg.find_by_command(path).is_some(),
                "path {path} was not registered",
            );
        }
    }

    #[test]
    fn context_proposal_is_discoverable_through_the_registry_with_its_permission_intact() {
        // Der eigentliche Prüfstein dieses Integrationsschritts: die Operation
        // war fertig und getestet, aber in keiner Registry erreichbar (siehe
        // Moduldoku, Abschnitt „Op-Set" vor diesem Commit). Jetzt muss sie
        // über `/context-proposal` auffindbar sein, und ihre vom erbauenden
        // Knoten (AW5-09) festgelegte `PermissionTier` darf sich dabei nicht
        // verschoben haben.
        let mut reg = OperationRegistry::new();
        register_all(&mut reg);

        let op = reg
            .find_by_command("/context-proposal")
            .expect("/context-proposal must be registered");
        assert_eq!(op.meta().name, "context-proposal");
        assert_eq!(
            op.meta().permission,
            harw_operations::PermissionTier::Operator,
            "permission tier must arrive unchanged from the building node"
        );
    }

    #[test]
    fn every_declared_operation_struct_is_registered_somewhere() {
        // Vollständigkeitsprüfung: jede in dieser Crate deklarierte Operation
        // (per `#[operation(...)]`-Makro erzeugte `*Operation`-Struct) muss in
        // `register_all` oder `register_plan_tools` landen — sonst ist sie
        // gebaut, getestet, dokumentiert und für niemanden erreichbar. Genau
        // dieses Muster (`/context-proposal`) hat diesen Knoten ausgelöst; ein
        // Test wie dieser existierte vorher nicht, weil `register_all` bislang
        // nur seine eigene feste Liste gegen sich selbst prüfte (`len() == N`),
        // nie gegen die Menge aller im Crate deklarierten Operationen.
        //
        // `compact::CompactOperation` ist die eine dokumentierte Ausnahme: sie
        // wird bewusst durch `UnavailableCompactOperation` ersetzt (siehe
        // `compact`-Moduldoku — Session-Mutation ist noch nicht angebunden),
        // nicht vergessen.
        let mut reg = OperationRegistry::new();
        register_all(&mut reg);
        super::register_plan_tools(&mut reg, &PlanToolConfig::enabled_defaults());

        let registered_names: std::collections::BTreeSet<&str> =
            reg.iter().map(|op| op.meta().name).collect();

        let declared_names = [
            "help",
            "status",
            "quit",
            "new",
            "work",
            "ps",
            "attach",
            "stop",
            "diff",
            "agent",
            "skills",
            "plugins",
            "model",
            "provider",
            // `uia-model`/`uia-provider` were previously absent from this
            // completeness list although they were already registered in
            // `register_all` — an unexplained gap, not a deliberate
            // exception like `compact` below. Added here for the same
            // reason `uia-worker-model`/`uia-effort` triggered this node:
            // this list is documented as exhaustive over declared op
            // structs, and there was no recorded rationale for leaving the
            // other UIA twins out.
            "uia-model",
            "uia-provider",
            "permissions",
            "compact",
            "memory",
            "effort",
            "mode",
            "context-proposal",
            "approval.pending",
            "approval.resolve",
            "add-workdir",
            "export",
            "usage",
            "bug-report",
            "approve",
            "deny",
            "review",
            "cancel",
            "retry",
            // W6b, same completeness-list gap as `uia-model`/`uia-provider`
            // above: already registered, never added here.
            "provider-concurrency",
            // This node: implemented, now registered, now listed.
            "uia-worker-model",
            "uia-effort",
            "plan",
            "goal",
            "explore",
            "research_deps",
            "research_web",
            "analyze",
        ];

        for name in declared_names {
            assert!(
                registered_names.contains(name),
                "operation '{name}' is declared in harw-ops but not registered anywhere",
            );
        }
    }

    #[test]
    fn compact_is_registered_as_unavailable_not_as_a_functional_action() {
        let mut reg = OperationRegistry::new();
        register_all(&mut reg);

        let compact = reg
            .find_by_command("/compact")
            .expect("/compact must remain discoverable");
        assert_eq!(compact.meta().name, "compact");
        assert_eq!(
            compact.meta().summary,
            "Session-Komprimierung ist in dieser Laufzeit nicht verfügbar."
        );
    }

    #[test]
    fn compact_unavailable_output_is_explicit_and_renderable() {
        assert_eq!(
            compact_unavailable_output().text,
            crate::compact::COMPACT_HINT
        );
    }

    #[test]
    fn register_all_second_pass_rejects_duplicates() {
        use harw_operations::registry::RegistryError;
        use std::sync::Arc;

        let mut reg = OperationRegistry::new();
        register_all(&mut reg);
        assert_eq!(
            reg.len(),
            36,
            "first register_all must produce exactly 36 ops"
        );

        // Attempt to register HelpOperation a second time via the fallible path.
        // "help" is the first op registered, so it is the canonical first duplicate.
        let result = reg.try_register(Arc::new(super::help::HelpOperation));
        assert!(
            matches!(result, Err(RegistryError::DuplicateName { ref name }) if name == "help"),
            "expected DuplicateName {{ name: \"help\" }}, got: {result:?}",
        );

        // Registry must not have grown — the rejected op was not inserted.
        assert_eq!(
            reg.len(),
            36,
            "registry must stay at 36 after a rejected duplicate"
        );
    }
}
