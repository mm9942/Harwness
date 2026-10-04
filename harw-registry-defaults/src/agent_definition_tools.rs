//! `AgentDefinitionToolProvider` — Bauplan-Werkzeuge für die Rolle
//! `agent-steward` (Addendum K, Contract Master §"Agent K-C", Nachtrag K,
//! Zusatzauftrag "Vorschlagsmodus", Nachtrag K3 "Schärfung der
//! Steward-Prüfung").
//!
//! # Verantwortung
//! `AgentDefinitionToolProvider` selbst stellt bis zu sechs Werkzeuge bereit
//! (welche registriert sind, hängt von [`DefinitionWriteMode`] **und** von
//! [`DefinitionAuthorCeiling`] ab); zusätzlich definiert diese Datei den
//! eigenständigen [`UiaSelfDocumentToolProvider`] für `uia_self.update_document`
//! (ein siebtes Werkzeug, aber ein eigener Provider mit eigener
//! Rollen-Gating-Logik, kein siebtes `AgentDefinitionToolProvider`-Werkzeug):
//! - `agents.validate {toml}`: parst + senkt eine Agentendefinition über
//!   dieselbe Pipeline wie
//!   [`crate::embedded_agents::builtin_agent_definitions`] (`parse_toml →
//!   resolve_definition → lower`) und meldet Fehler, ohne etwas zu schreiben.
//!   Rein lesend, **immer** registriert.
//! - `agents.list_proposals {}`: listet alle Vorschläge unter
//!   `<profile_agents_dir>/.proposals/`, markiert abgelaufene mit
//!   `expired: true`. Rein lesend, **immer** registriert.
//! - `agents.write_definition {scope, name, toml, run_id?}`: validiert
//!   zwingend, berechnet die Rechte-Deltas (siehe unten) und legt — außer im
//!   Sonderfall `scope = "run"` mit leeren Deltas — **immer** einen Vorschlag
//!   ab (nie einen Direkt-Schreibvorgang: Nachtrag K3 bindet dauerhaftes
//!   Schreiben an die Prüfung, unabhängig vom [`DefinitionWriteMode`]).
//! - `agents.write_uia {dir_name, definition_toml, personality_md, user_md?,
//!   identity_md?}`: wie oben, aber für ein vollständiges UIA-Bundle; verlangt
//!   zusätzlich `role = "user-interface"`, lehnt offensichtliche Geheimnisse
//!   ab und bekommt immer `review_level = "user_required"` (UIAs werden nie
//!   automatisch aktiviert). `identity_md`, falls angegeben, landet als eigene
//!   `identity.md`-Datei im Bundle (siehe [`harw_config::loader::load_uia_identity`]
//!   in `harw-config`), nicht mehr als Feld in `agent.toml`. Kein `scope = "run"`.
//! - `uia_self.update_document {target, content, reason}`: pflegt
//!   `identity.md`/`USER.md`/`Personality.md` der **eigenen** UIA-Sitzung
//!   nach der Ersteinrichtung; nur registriert, wenn der Provider mit
//!   `role = AgentRoleId::UserInterface` konstruiert wurde. Freigabepflichtig
//!   wie `agents.write_uia` (nicht in [`crate::AUTO_APPROVED_TOOLS`]). Kein
//!   Rechte-Delta nötig (reine Inhaltsdateien, keine Rechteverleihung).
//! - `agents.commit_proposal {proposal_id, user_confirmed?}` **nur im
//!   [`DefinitionWriteMode::Commit`] und nur mit gesetzter Decke**: validiert
//!   den Vorschlag erneut, rechnet beide Deltas erneut — **gegen die Decke
//!   des committenden Aufrufers**, die von der Decke des ursprünglichen
//!   Autors abweichen kann — und schreibt danach ans Ziel.
//! - `agents.reject_proposal {proposal_id, reason}` **nur im
//!   [`DefinitionWriteMode::Commit`] und nur mit gesetzter Decke**.
//!
//! # Die Urheber-Decke (Nachtrag K3)
//! Ein Spawn prüft bereits ∩-Algebra (`child_controller.rs`,
//! `AuthorityCeiling::intersect`), aber eine **dauerhafte Definitionsdatei**
//! ist an sich an keinen Urheber gebunden — sie gilt für jeden späteren
//! Aufrufer, der den definierten Agenten spawnt. [`DefinitionAuthorCeiling`]
//! schließt diese Lücke: Ohne sie (`ceiling = None`) registriert dieser
//! Provider fail-closed **nur** `agents.validate`/`agents.list_proposals` —
//! kein Schreiben, kein Vorschlag, kein Commit. Mit ihr vergleicht jeder
//! schreibende Aufruf die vom Kandidaten beanspruchten Rechte
//! ([`claimed_rights_of`]) gegen zwei Referenzen:
//! - **Urheber-Delta** (`rights_delta_author`): was der Kandidat über die
//!   Decke selbst hinaus beansprucht. Nicht leer ⇒ harte Ablehnung
//!   (`"authority elevation: the author cannot grant rights it does not
//!   hold"`) — niemand verleiht Rechte, die er selbst nicht hält. Es wird in
//!   diesem Fall **nichts** geschrieben, auch kein Vorschlag.
//! - **Basisrollen-Delta** (`rights_delta_base_role`): was der Kandidat über
//!   die eingebaute Definition bzw. das Registry-Profil derselben Rolle
//!   hinaus beansprucht (siehe [`base_role_rights_of`]).
//!
//! Daraus ergibt sich `review_level`: `"user_required"` für jedes
//! UIA-Bundle oder ein nicht-leeres Basisrollen-Delta; sonst `"uia"`
//! (dauerhaft, aber ohne über die Basisrolle hinausgehende Rechte); ein
//! auftragsgebundener `scope = "run"`-Vorschlag mit **beiden** leeren Deltas
//! bekommt `"none"` und wird sofort geschrieben (kein Vorschlag).
//!
//! # `user_confirmed` und die Freigabe-Kette
//! `agents.commit_proposal` verlangt bei `review_level = "user_required"`
//! `user_confirmed = true`. Dieses Feld ist **kein** Ersatz für die
//! Freigabe-Prüfung des Harness: `agents.commit_proposal` ist — wie jedes
//! schreibende Werkzeug dieses Providers — bewusst nicht in
//! [`crate::AUTO_APPROVED_TOOLS`] gelistet und bleibt damit über
//! [`crate::DefaultApprovalPolicy`] fail-closed freigabepflichtig: Der
//! Aufruf pausiert beim Nutzer, **bevor** dieser Code überhaupt läuft. Ein
//! Modell kann `user_confirmed: true` zwar im Argument behaupten, aber ohne
//! die vorgelagerte Freigabe erreicht der Aufruf diesen Code gar nicht erst.
//! Dieser Datei fehlt jede Möglichkeit, eine echte Nutzerbestätigung von
//! einer Modellbehauptung zu unterscheiden — sie verlässt sich strukturell
//! auf die Freigabekette, genau wie `fs.write`.
//!
//! # Schlüsseltypen
//! - [`AgentDefinitionToolProvider`]
//! - [`DefinitionWriteMode`]
//! - [`DefinitionAuthorCeiling`]
//!
//! # Nebenläufigkeit
//! `Send + Sync`; zustandslos außer den konfigurierten Zielverzeichnissen,
//! dem Modus und der Decke. Alle Dateizugriffe sind synchrones, blockierendes
//! I/O.
//!
//! # Fehler
//! Kein Aufruf dieses Providers gibt `Err` an den Aufrufer zurück —
//! Validierungs-, Rechte- und I/O-Fehler werden als `ToolOutput::Json`
//! (`{"ok": false, "errors": [...]}`) bzw. `ToolOutput::Error` gemeldet.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::parse::parse_toml;
use harw_agent_dsl::raw::RawAgentDefinition;
use harw_agent_dsl::resolve::resolve_definition;
use harw_agent_dsl::roles::AgentRoleId;
use harw_agent_dsl::{ExecutableAgentIr, lower};
use harw_authority::PermissionSet;
use harw_extension_api::contributors::ToolProvider;
use harw_extension_api::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
    ToolSpec,
};
use harw_tools::args::parse_args;
use harw_tools::{AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType};
use serde::Deserialize;

use crate::embedded_agents::builtin_agent_toml;

/// Zähler für eindeutige Temp-/Vorschlags-Namen innerhalb eines Prozesses
/// (mehrere gleichzeitige Aufrufe dürfen sich nie denselben Pfad teilen).
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Offensichtliche Geheimnis-Muster (Klein-/Großschreibung ignoriert), die
/// `agents.write_uia` (und `agents.commit_proposal` für `kind = "uia"`) in
/// jedem übergebenen Feld ablehnt (Nachtrag K).
const SECRET_PATTERNS: &[&str] = &["sk-", "-----begin", "api_key ="];

/// Name des Vorschlags-Unterverzeichnisses unter `profile_agents_dir`.
const PROPOSALS_DIR_NAME: &str = ".proposals";

/// Anzahl der Tage, nach denen ein Vorschlag abläuft (Nachtrag K3).
const PROPOSAL_TTL_DAYS: i64 = 7;

/// Der aktuelle Zeitpunkt für Auflösungs-Traces und Vorschlags-Zeitstempel.
fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}

/// Formatiert `dt` als RFC-3339-Text. Ein Formatierungsfehler (praktisch
/// unerreichbar für `OffsetDateTime`-Werte aus dieser Datei) fällt auf einen
/// festen Platzhalter zurück statt zu `panic!`en.
fn format_rfc3339(dt: time::OffsetDateTime) -> String {
    dt.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unbekannt".to_owned())
}

/// `now()` als RFC-3339-Text.
fn now_rfc3339() -> String {
    format_rfc3339(now())
}

/// Parst einen RFC-3339-Zeitstempel und prüft, ob er in der Vergangenheit
/// liegt. Ein nicht parsbarer Zeitstempel gilt als **nicht** abgelaufen
/// (lenient: eine defekte Altdatei blockiert die Listen-/Commit-Ansicht
/// nicht zusätzlich zu ihrem eigentlichen Defekt).
fn is_expired(rfc3339_timestamp: &str) -> bool {
    match time::OffsetDateTime::parse(
        rfc3339_timestamp,
        &time::format_description::well_known::Rfc3339,
    ) {
        Ok(expires_at) => expires_at < now(),
        Err(_) => false,
    }
}

/// Ob ein `agents.commit_proposal`/`agents.reject_proposal`-Aufruf
/// registriert ist. `agents.write_definition`/`agents.write_uia` schreiben
/// seit Nachtrag K3 **nie** direkt (außer `scope = "run"` mit leeren
/// Rechte-Deltas) — der Modus entscheidet nur noch, ob diese
/// Provider-Instanz ihre eigenen (oder fremde) Vorschläge auch freigeben
/// darf.
///
/// # Description
/// Ein `agent-steward`, der vom Root-Orchestrator gestartet wird, darf
/// Vorschläge nie selbst freigeben — nur die UIA (bzw. ihr eigener
/// `agent-steward`) darf das. Der Aufrufer wählt den Modus beim
/// Konstruieren des Providers ([`AgentDefinitionToolProvider::new`]); er ist
/// nicht zur Laufzeit umschaltbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionWriteMode {
    /// Registriert zusätzlich `agents.commit_proposal` und
    /// `agents.reject_proposal` (sofern eine [`DefinitionAuthorCeiling`]
    /// gesetzt ist).
    Commit,
    /// `agents.commit_proposal`/`agents.reject_proposal` sind in diesem
    /// Modus nie registriert.
    ProposalOnly,
}

/// Die effektiven Rechte eines Aufrufers (oder eines Kandidaten, oder einer
/// Basisrolle) für den Vergleich der ∩-Algebra (Nachtrag K3).
///
/// # Description
/// Dient in drei Rollen mit identischer Form: als **Urheber-Decke**, die
/// [`AgentDefinitionToolProvider::new`] entgegennimmt (die effektiven Rechte
/// des Eltern-Aufrufers des Stewards); als Rückgabe von
/// [`claimed_rights_of`] (die von einer gesenkten Definition beanspruchten
/// Rechte); und als Rückgabe von [`base_role_rights_of`] (die Rechte der
/// eingebauten Definition/des Profils derselben Rolle). Alle drei Werte
/// werden mit derselben Funktion ([`compute_rights_delta`]) verglichen.
///
/// # Concurrency
/// `Send + Sync`; reiner Datenwert.
#[derive(Debug, Clone)]
pub struct DefinitionAuthorCeiling {
    /// Organisatorische Rolle, für die diese Rechte gelten (bei einem
    /// Kandidaten: seine eigene Rolle; bei der Decke: die Rolle des
    /// Eltern-Aufrufers).
    pub role: AgentRoleId,
    /// Zugelassene Werkzeugnamen.
    pub tools: BTreeSet<String>,
    /// Zugelassene Sandbox-Rechte.
    pub permissions: PermissionSet,
    /// Maximale Spawn-Tiefe.
    pub max_depth: u32,
    /// Token-Budget-Obergrenze.
    pub budget_tokens: u64,
    /// Obergrenze der Reasoning-Effort-Stufe als undurchsichtiges Label
    /// (`"low"`/`"medium"`/`"high"`, wie in `[spawn.budget].effort_cap`).
    /// `None` ist die niedrigste Stufe — dieselbe „keine Aussage = kein
    /// Anspruch"-Semantik wie bei einer leeren `tools`-Menge oder
    /// `budget_tokens = 0`, damit alle fünf Felder monoton denselben Nullwert
    /// tragen.
    pub effort_cap: Option<String>,
}

/// Der Provider für die Bauplan-Werkzeuge der Rolle `agent-steward`.
///
/// # Description
/// `agents.write_definition` schreibt nur im Sonderfall `scope = "run"` mit
/// leeren Rechte-Deltas sofort (nach `<project_agents_dir>/../state/runs/
/// <run_id>/agents/<name>/definition.toml`); sonst legt es — wie `agents.write_uia`
/// immer — einen Vorschlag unter `profile_agents_dir/.proposals/<id>/` ab.
///
/// # Concurrency
/// `Send + Sync`; hält nur zwei `PathBuf`s, den Modus und die optionale
/// Decke, kein veränderlicher Zustand.
pub struct AgentDefinitionToolProvider {
    /// Zielverzeichnis für `scope = "project"` und für `scope = "run"`
    /// (relativ dazu: `../state/runs/<run_id>/agents/`). `None`, wenn kein
    /// Projektkontext bekannt ist.
    project_agents_dir: Option<PathBuf>,
    /// Zielverzeichnis für `scope = "profile"`, für jedes
    /// `agents.write_uia`-Bundle und für `.proposals/`. `None`, wenn kein
    /// Profil bekannt ist.
    profile_agents_dir: Option<PathBuf>,
    /// Ob diese Instanz `agents.commit_proposal`/`agents.reject_proposal`
    /// registriert (nur zusammen mit einer gesetzten Decke).
    mode: DefinitionWriteMode,
    /// Die Urheber-Decke; `None` ⇒ fail-closed (nur `agents.validate` und
    /// `agents.list_proposals`).
    ceiling: Option<DefinitionAuthorCeiling>,
}

impl AgentDefinitionToolProvider {
    /// Erstellt einen neuen Provider.
    ///
    /// # Arguments
    /// - `project_agents_dir` (`Option<PathBuf>`): Ziel für `scope =
    ///   "project"`/`"run"`.
    /// - `profile_agents_dir` (`Option<PathBuf>`): Ziel für `scope =
    ///   "profile"`, jedes `agents.write_uia`-Bundle und `.proposals/`.
    /// - `mode` ([`DefinitionWriteMode`]): steuert, ob diese Instanz
    ///   `agents.commit_proposal`/`agents.reject_proposal` registriert.
    /// - `ceiling` (`Option<`[`DefinitionAuthorCeiling`]`>`): die effektiven
    ///   Rechte des Eltern-Aufrufers. `None` ⇒ fail-closed: nur
    ///   `agents.validate` und `agents.list_proposals` werden registriert.
    ///
    /// # Returns
    /// Den fertig konfigurierten Provider.
    #[must_use]
    pub fn new(
        project_agents_dir: Option<PathBuf>,
        profile_agents_dir: Option<PathBuf>,
        mode: DefinitionWriteMode,
        ceiling: Option<DefinitionAuthorCeiling>,
    ) -> Self {
        Self {
            project_agents_dir,
            profile_agents_dir,
            mode,
            ceiling,
        }
    }
}

// ---------------------------------------------------------------------------
// Validierungs-Pipeline (dieselbe wie embedded_agents: parse_toml → resolve → lower)
// ---------------------------------------------------------------------------

/// Ergebnis von [`validate_definition_toml`]: entweder eine erfolgreich
/// gesenkte Definition mit ihrer Roh-Form, oder eine Liste menschenlesbarer
/// Fehler.
enum Validated {
    /// Parsen, Auflösen und Senken sind erfolgreich durchgelaufen.
    Ok {
        /// Die roh geparste Definition (für `id`/`role`-Vorabprüfungen ohne
        /// erneutes Parsen).
        raw: Box<RawAgentDefinition>,
        /// Die gesenkte ausführbare Form.
        ir: Box<ExecutableAgentIr>,
    },
    /// Parsen, Auflösen oder Senken ist gescheitert; nichts wurde geschrieben.
    Err(Vec<String>),
}

/// Validiert eine Agentendefinition über dieselbe Pipeline wie
/// [`crate::embedded_agents::builtin_agent_definitions`]: `parse_toml` →
/// `resolve_definition` (über den eingebetteten Rollen als Basis-Schichten,
/// damit `extends`/Mixins gegen eingebaute Rollen wie `worker-base` auflösen)
/// → `lower`.
///
/// # Description
/// Die zu validierende Definition selbst wird als [`DefinitionLayer::Project`]
/// eingehängt — die höchste Schicht unterhalb von `RunLocal` — damit sie jede
/// eingebettete Basis überschreiben, aber nie mit ihr kollidieren kann.
///
/// # Arguments
/// - `toml_source` (`&str`): der zu validierende TOML-Quelltext.
///
/// # Returns
/// [`Validated::Ok`] mit roher und gesenkter Form bei Erfolg, sonst
/// [`Validated::Err`] mit mindestens einem Fehlertext.
fn validate_definition_toml(toml_source: &str) -> Validated {
    let raw = match parse_toml(toml_source) {
        Ok(raw) => raw,
        Err(error) => return Validated::Err(vec![format!("TOML: {error}")]),
    };

    let mut layers: Vec<(DefinitionLayer, RawAgentDefinition)> = Vec::new();
    for (name, source) in builtin_agent_toml() {
        match parse_toml(source) {
            Ok(base_raw) => layers.push((DefinitionLayer::BuiltIn, base_raw)),
            Err(error) => {
                return Validated::Err(vec![format!(
                    "interner Fehler: eingebettete Basis '{name}' ist defekt: {error}"
                )]);
            }
        }
    }
    let target_id = raw.id.clone();
    layers.push((DefinitionLayer::Project, raw.clone()));

    let resolved = match resolve_definition(&target_id, &layers, now()) {
        Ok(resolved) => resolved,
        Err(error) => return Validated::Err(vec![error.to_string()]),
    };
    match lower(&resolved) {
        Ok(ir) => Validated::Ok {
            raw: Box::new(raw),
            ir: Box::new(ir),
        },
        Err(error) => Validated::Err(vec![error.to_string()]),
    }
}

/// Ein gültiger, dateisystem-sicherer Slug: nicht leer, nur
/// `[a-z0-9-]`, kein Pfadtrenner, kein `.`/`..`.
fn is_valid_slug(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

// ---------------------------------------------------------------------------
// Rechte-Algebra (Nachtrag K3)
// ---------------------------------------------------------------------------

/// Ein Rechte-Delta: was ein Kandidat über eine Referenz (Decke oder
/// Basisrolle) hinaus beansprucht. Format wie im Contract vorgegeben:
/// `{added_tools[], added_permissions[], depth_increase, budget_increase,
/// effort_increase}`.
#[derive(Debug, Clone, Default)]
struct RightsDelta {
    added_tools: Vec<String>,
    added_permissions: Vec<String>,
    depth_increase: Option<u32>,
    budget_increase: Option<u64>,
    effort_increase: Option<String>,
}

impl RightsDelta {
    /// `true`, wenn keines der fünf Felder eine Überschreitung meldet.
    fn is_empty(&self) -> bool {
        self.added_tools.is_empty()
            && self.added_permissions.is_empty()
            && self.depth_increase.is_none()
            && self.budget_increase.is_none()
            && self.effort_increase.is_none()
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "added_tools": self.added_tools,
            "added_permissions": self.added_permissions,
            "depth_increase": self.depth_increase,
            "budget_increase": self.budget_increase,
            "effort_increase": self.effort_increase,
        })
    }
}

/// Rang einer Effort-Stufe für den Vergleich in [`compute_rights_delta`].
/// `None` (keine Aussage) ist der niedrigste Rang, ein unbekanntes Label der
/// höchste (konservativ: eine nicht erkannte Stufe gilt immer als
/// Überschreitung, außer die Referenz trägt exakt dasselbe Label).
fn effort_rank(label: Option<&str>) -> (i32, Option<String>) {
    match label.map(str::to_lowercase) {
        None => (0, None),
        Some(l) if l == "low" => (1, Some(l)),
        Some(l) if l == "medium" => (2, Some(l)),
        Some(l) if l == "high" => (3, Some(l)),
        Some(other) => (4, Some(other)),
    }
}

/// Vergleicht `claimed` gegen `limit` und liefert das Delta (siehe
/// [`RightsDelta`]). Beide Seiten müssen exakt dieselbe Form
/// ([`DefinitionAuthorCeiling`]) tragen — daher dient dieser Typ sowohl als
/// öffentliche Urheber-Decke als auch intern als Rückgabe von
/// [`claimed_rights_of`]/[`base_role_rights_of`].
fn compute_rights_delta(
    claimed: &DefinitionAuthorCeiling,
    limit: &DefinitionAuthorCeiling,
) -> RightsDelta {
    let added_tools: Vec<String> = claimed.tools.difference(&limit.tools).cloned().collect();
    let added_permissions: Vec<String> = claimed
        .permissions
        .iter()
        .filter(|permission| !limit.permissions.contains(*permission))
        .map(|permission| format!("{permission:?}"))
        .collect();
    let depth_increase = claimed
        .max_depth
        .checked_sub(limit.max_depth)
        .filter(|delta| *delta > 0);
    let budget_increase = claimed
        .budget_tokens
        .checked_sub(limit.budget_tokens)
        .filter(|delta| *delta > 0);
    let (claimed_rank, claimed_label) = effort_rank(claimed.effort_cap.as_deref());
    let (limit_rank, _) = effort_rank(limit.effort_cap.as_deref());
    let effort_increase = if claimed_rank > limit_rank {
        claimed_label
    } else {
        None
    };

    RightsDelta {
        added_tools,
        added_permissions,
        depth_increase,
        budget_increase,
        effort_increase,
    }
}

/// Ermittelt die von einer gesenkten Definition beanspruchten Rechte:
/// `[tools].admitted` als Werkzeugmenge, deren Rechte über
/// [`crate::tool_permission`], `[spawn].max_depth` (0, wenn nicht gesetzt),
/// `[spawn.budget].max_tokens` (0, wenn nicht gesetzt) und
/// `[spawn.budget].effort_cap`.
fn claimed_rights_of(ir: &ExecutableAgentIr) -> DefinitionAuthorCeiling {
    let tools: BTreeSet<String> = ir.tool_surface().admitted().iter().cloned().collect();
    let permissions =
        PermissionSet::from_policy(tools.iter().filter_map(|tool| crate::tool_permission(tool)));
    let max_depth = ir.spawn_contract().max_depth().unwrap_or(0);
    let budget = ir.spawn_contract().budget();
    let budget_tokens = budget.and_then(|b| b.max_tokens()).unwrap_or(0);
    let effort_cap = budget.and_then(|b| b.effort_cap()).map(str::to_owned);
    DefinitionAuthorCeiling {
        role: ir.role(),
        tools,
        permissions,
        max_depth,
        budget_tokens,
        effort_cap,
    }
}

/// Ermittelt die Rechte der Basisrolle für den Vergleich in
/// `rights_delta_base_role`.
///
/// # Description
/// Drei Stufen, in dieser Reihenfolge:
/// 1. Trägt eine eingebaute Rolle denselben Namen (`raw.id.name`), gelten
///    ihre gesenkten Rechte ([`claimed_rights_of`] auf ihrer eigenen IR) —
///    der genaueste verfügbare Vergleich.
/// 2. Sonst, falls [`crate::profile_for_role`] für diesen Namen ein
///    Registry-Profil kennt, gilt dessen Werkzeugmenge und
///    `required_permissions()`; Tiefe/Budget/Effort sind für ein Profil
///    nicht definiert und bleiben bei `0`/`None` (jeder Anspruch des
///    Kandidaten in diesen drei Feldern zählt dann als Überschreitung —
///    bewusst konservativ, siehe Bericht).
/// 3. Ist beides unbekannt (ein neuer, freihändig benannter Agent), hat die
///    Basisrolle keine Rechte — jeder Anspruch des Kandidaten ist ein Delta.
///    Das ist beabsichtigt: ein völlig neuer Rollenname bekommt ohne
///    erkennbare Referenz immer eine menschliche Prüfung.
fn base_role_rights_of(definition_name: &str, role: AgentRoleId) -> DefinitionAuthorCeiling {
    if let Ok(builtins) =
        crate::embedded_agents::builtin_agent_definitions(&std::collections::HashMap::new())
    {
        if let Some(base_ir) = builtins.get(definition_name) {
            return claimed_rights_of(base_ir);
        }
    }
    if let Some(profile) = crate::profile_for_role(definition_name) {
        let tools: BTreeSet<String> = profile
            .registered_tool_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        return DefinitionAuthorCeiling {
            role,
            tools,
            permissions: profile.required_permissions(),
            max_depth: 0,
            budget_tokens: 0,
            effort_cap: None,
        };
    }
    DefinitionAuthorCeiling {
        role,
        tools: BTreeSet::new(),
        permissions: PermissionSet::empty(),
        max_depth: 0,
        budget_tokens: 0,
        effort_cap: None,
    }
}

/// Ergebnis der Prüfung eines Kandidaten: seine roh geparste und gesenkte
/// Form sowie beide Rechte-Deltas.
struct EvaluatedCandidate {
    raw: Box<RawAgentDefinition>,
    ir: Box<ExecutableAgentIr>,
    rights_delta_author: RightsDelta,
    rights_delta_base_role: RightsDelta,
}

/// Validiert `toml_source` und berechnet beide Rechte-Deltas gegen `ceiling`.
fn evaluate_candidate(
    toml_source: &str,
    ceiling: &DefinitionAuthorCeiling,
) -> Result<EvaluatedCandidate, Vec<String>> {
    let (raw, ir) = match validate_definition_toml(toml_source) {
        Validated::Ok { raw, ir } => (raw, ir),
        Validated::Err(errors) => return Err(errors),
    };
    let claimed = claimed_rights_of(&ir);
    let rights_delta_author = compute_rights_delta(&claimed, ceiling);
    let base = base_role_rights_of(&raw.id.name, ir.role());
    let rights_delta_base_role = compute_rights_delta(&claimed, &base);
    Ok(EvaluatedCandidate {
        raw,
        ir,
        rights_delta_author,
        rights_delta_base_role,
    })
}

/// Baut die `ToolOutput`-Ablehnung für ein nicht-leeres Urheber-Delta
/// (Contract-Fehlertext, wörtlich).
fn author_elevation_rejection(delta: &RightsDelta) -> ToolOutput {
    ToolOutput::json(serde_json::json!({
        "ok": false,
        "written": false,
        "errors": ["authority elevation: the author cannot grant rights it does not hold"],
        "rights_delta_author": delta.to_json(),
    }))
}

/// Bestimmt `review_level` aus `kind` und dem Basisrollen-Delta (das
/// Urheber-Delta ist an dieser Stelle bereits als leer geprüft — siehe
/// [`author_elevation_rejection`]). `kind == "uia"` ist immer
/// `"user_required"` (Nachtrag K: UIAs aktivieren sich nie selbst); sonst
/// `"user_required"`, wenn die Definition mehr beansprucht als ihre
/// Basisrolle, sonst `"uia"`. Der Sonderfall `"none"` (auftragsgebunden,
/// `scope = "run"`) wird nicht hier, sondern vom Aufrufer entschieden, weil
/// er zusätzlich verlangt, dass **beide** Deltas leer sind.
fn review_level_for(kind: &str, rights_delta_base_role: &RightsDelta) -> &'static str {
    if kind == "uia" || !rights_delta_base_role.is_empty() {
        "user_required"
    } else {
        "uia"
    }
}

// ---------------------------------------------------------------------------
// Atomares Schreiben (Temp-Datei + rename im selben Verzeichnis)
// ---------------------------------------------------------------------------

/// Schreibt `content` atomar nach `path`: ein Temp-Nachbar im selben
/// Verzeichnis wird angelegt (`create_new`, also nie ein vorhandenes Ziel
/// verändert), geschrieben, synchronisiert und über `path` umbenannt.
/// Nachgebaut aus dem Muster in `harw-cli/src/settings.rs::write_atomic`
/// (dieses Crate hat keine `harw-fsutil`-Abhängigkeit).
///
/// # Arguments
/// - `path` (`&Path`): Zielpfad; sein Elternverzeichnis wird angelegt, falls
///   es fehlt.
/// - `content` (`&[u8]`): zu schreibender Inhalt.
/// - `mode` (`u32`): Unix-Dateirechte der neuen Datei (unter Unix gesetzt,
///   unter anderen Plattformen ignoriert).
///
/// # Returns
/// `Ok(())` bei Erfolg.
///
/// # Errors
/// Jeder I/O-Fehler beim Anlegen, Schreiben oder Umbenennen wird
/// durchgereicht; ein angelegter Temp-Pfad wird beim Fehlschlag entfernt
/// (best effort).
fn write_atomic(path: &Path, content: &[u8], mode: u32) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("Zielpfad hat kein Elternverzeichnis"))?;
    std::fs::create_dir_all(parent)?;

    let temp_path = parent.join(format!(
        ".agent-def-{}-{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
    }
    let write_result = options.open(&temp_path).and_then(|mut file| {
        std::io::Write::write_all(&mut file, content).and_then(|()| file.sync_all())
    });
    if let Err(source) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(source);
    }
    std::fs::rename(&temp_path, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp_path);
    })
}

/// Liest die `id` einer vorhandenen Definitionsdatei, falls sie existiert und
/// parsbar ist.
///
/// # Returns
/// `Ok(Some(id))`, wenn die Datei existiert und parst; `Ok(None)`, wenn sie
/// nicht existiert; `Err(reason)`, wenn sie existiert, aber weder lesbar noch
/// parsbar ist (fail-closed: eine defekte Nachbardatei wird nie stillschweigend
/// überschrieben).
fn existing_definition_id(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(source) => match parse_toml(&source) {
            Ok(raw) => Ok(Some(raw.id.to_string())),
            Err(error) => Err(format!(
                "vorhandene Datei {} ist nicht parsbar und wird nicht überschrieben: {error}",
                path.display()
            )),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "vorhandene Datei {} ist nicht lesbar: {error}",
            path.display()
        )),
    }
}

/// Kebab-case-Name der Rolle, wie ihn `role = "..."` in einer TOML-Definition
/// trägt (`AgentRoleId` serialisiert selbst schon `rename_all = "kebab-case"`).
fn role_key(role: AgentRoleId) -> String {
    serde_json::to_value(role)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Prüft `fields` gegen [`SECRET_PATTERNS`] und liefert für jedes betroffene
/// Feld eine Fehlermeldung.
fn scan_for_secrets(fields: &[(&str, &str)]) -> Vec<String> {
    let mut errors = Vec::new();
    for (field_name, content) in fields {
        let lower = content.to_lowercase();
        if let Some(pattern) = SECRET_PATTERNS
            .iter()
            .find(|pattern| lower.contains(*pattern))
        {
            errors.push(format!(
                "Feld '{field_name}' enthält ein offensichtliches Geheimnis-Muster ('{pattern}') \
                 und wird abgelehnt"
            ));
        }
    }
    errors
}

/// Ein einfaches zeilenweises Diff gegen eine bestehende Zieldatei (kein
/// LCS-Alignment — nur ein positionsweiser Vergleich, wie vom Contract als
/// "einfaches Zeilen-Diff" verlangt). `old` = `None` ⇒ `"new file"`.
fn simple_line_diff(old: Option<&str>, new_content: &str) -> String {
    let Some(old) = old else {
        return "new file".to_owned();
    };
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();
    let mut out = String::new();
    for index in 0..old_lines.len().max(new_lines.len()) {
        match (old_lines.get(index), new_lines.get(index)) {
            (Some(o), Some(n)) if o == n => {}
            (Some(o), Some(n)) => out.push_str(&format!("-{o}\n+{n}\n")),
            (Some(o), None) => out.push_str(&format!("-{o}\n")),
            (None, Some(n)) => out.push_str(&format!("+{n}\n")),
            (None, None) => {}
        }
    }
    if out.is_empty() {
        "no changes".to_owned()
    } else {
        out
    }
}

// ---------------------------------------------------------------------------
// Ziel-Commit (Sonderfall `scope = "run"` UND `agents.commit_proposal`)
// ---------------------------------------------------------------------------

/// Rechte-Bits neu angelegter Agentendefinitions-Dateien.
const DEFINITION_FILE_MODE: u32 = 0o644;

/// Rechte-Bits der UIA-Bundle-Dateien (persönlich, nie welt-/gruppenlesbar).
const UIA_FILE_MODE: u32 = 0o600;

/// Der kanonische Ablageort einer Agentendefinition: `<dir>/<name>/definition.toml`
/// — genau das Format, das `harw_config::discover_config` liest (Plan R9,
/// Teil B). Bis Plan R9 schrieb [`commit_definition`] flach nach
/// `<dir>/<name>.toml`; die Discovery liest solche Altdateien weiterhin (mit
/// Warnung), siehe [`legacy_flat_definition_path`].
fn definition_path(target_dir: &Path, name: &str) -> PathBuf {
    target_dir.join(name).join("definition.toml")
}

/// Der alte, flache Ablageort `<dir>/<name>.toml` (nur noch gelesen).
fn legacy_flat_definition_path(target_dir: &Path, name: &str) -> PathBuf {
    target_dir.join(format!("{name}.toml"))
}

/// Die vorhandene Zieldatei für ein Diff: zuerst das Verzeichnisformat, sonst
/// eine flache Altdatei.
fn existing_definition_path(target_dir: &Path, name: &str) -> PathBuf {
    let canonical = definition_path(target_dir, name);
    if canonical.exists() {
        return canonical;
    }
    let legacy = legacy_flat_definition_path(target_dir, name);
    if legacy.exists() { legacy } else { canonical }
}

/// Schreibt eine einzelne Agentendefinition atomar nach
/// `<target_dir>/<name>/definition.toml` (siehe [`definition_path`]), sofern
/// eine dort — oder in einer flachen Altdatei `<target_dir>/<name>.toml` —
/// bereits vorhandene Definition dieselbe `id` trägt (sonst Fehler, siehe
/// [`existing_definition_id`]).
///
/// Eine flache Altdatei mit derselben `id` wird nach erfolgreichem Schreiben
/// entfernt, damit Discovery nicht zwei Stände derselben Definition sieht.
fn commit_definition(
    target_dir: &Path,
    name: &str,
    toml_source: &str,
    new_id: &str,
) -> Result<PathBuf, String> {
    let target_path = definition_path(target_dir, name);
    let legacy_path = legacy_flat_definition_path(target_dir, name);
    let legacy_id = existing_definition_id(&legacy_path)?;
    if let Some(existing_id) = &legacy_id {
        if existing_id != new_id {
            return Err(format!(
                "vorhandene Datei {} hat id '{existing_id}', neue Definition hat id '{new_id}' \
                 — wird nicht überschrieben",
                legacy_path.display()
            ));
        }
    }
    if let Some(existing_id) = existing_definition_id(&target_path)? {
        if existing_id != new_id {
            return Err(format!(
                "vorhandene Datei {} hat id '{existing_id}', neue Definition hat id '{new_id}' \
                 — wird nicht überschrieben",
                target_path.display()
            ));
        }
    }
    write_atomic(&target_path, toml_source.as_bytes(), DEFINITION_FILE_MODE).map_err(|error| {
        format!(
            "Schreiben von {} fehlgeschlagen: {error}",
            target_path.display()
        )
    })?;
    if legacy_id.is_some() {
        if let Err(error) = std::fs::remove_file(&legacy_path) {
            tracing::warn!(
                path = %legacy_path.display(),
                %error,
                "agents.commit_definition.legacy_flat_file_not_removed"
            );
        }
    }
    Ok(target_path)
}

/// Schreibt ein vollständiges UIA-Bundle (`definition.toml`, `agent.toml`,
/// `identity.md` (nur wenn angegeben), `Personality.md`, `USER.md`) atomar je
/// Datei nach `bundle_dir`, sofern ein dort bereits vorhandenes
/// `definition.toml` dieselbe `id` trägt.
fn commit_uia_bundle(
    bundle_dir: &Path,
    new_id: &str,
    definition_toml: &str,
    agent_toml: &str,
    identity_md: Option<&str>,
    personality_md: &str,
    user_md: &str,
) -> Result<(), String> {
    let definition_path = bundle_dir.join("definition.toml");
    if let Some(existing_id) = existing_definition_id(&definition_path)? {
        if existing_id != new_id {
            return Err(format!(
                "vorhandenes Bundle {} hat id '{existing_id}', neue Definition hat id '{new_id}' \
                 — wird nicht überschrieben",
                bundle_dir.display()
            ));
        }
    }
    let mut files: Vec<(PathBuf, &str)> = vec![
        (definition_path, definition_toml),
        (bundle_dir.join("agent.toml"), agent_toml),
        (bundle_dir.join("Personality.md"), personality_md),
        (bundle_dir.join("USER.md"), user_md),
    ];
    if let Some(identity_md) = identity_md {
        files.push((bundle_dir.join("identity.md"), identity_md));
    }
    for (path, content) in &files {
        write_atomic(path, content.as_bytes(), UIA_FILE_MODE)
            .map_err(|error| format!("Schreiben von {} fehlgeschlagen: {error}", path.display()))?;
    }
    Ok(())
}

/// Baut den Inhalt von `agent.toml` aus der geparsten `definition.toml`.
///
/// Gespiegeltes Feld-Set von `harw-cli/src/uia_bootstrap.rs::write_generated_uia`
/// (`name`/`role`/`description`). Trägt seit der Behebung der zuvor hier
/// dokumentierten Abweichung **kein** `identity`-Feld mehr — der
/// Identitätstext landet stattdessen in einer eigenen `identity.md`-Datei
/// (siehe [`commit_uia_bundle`]/[`propose_uia`]), die
/// `harw_config::loader::load_uia_identity` liest.
fn build_agent_toml(raw: &RawAgentDefinition) -> String {
    let name = raw
        .name
        .clone()
        .unwrap_or_else(|| raw.specialization.clone());
    let description = raw.description.clone().unwrap_or_default();
    format!("name = {name:?}\nrole = \"user-interface\"\ndescription = {description:?}\n")
}

// ---------------------------------------------------------------------------
// Vorschläge (`agents.list_proposals`, `agents.commit_proposal`,
// `agents.reject_proposal`)
// ---------------------------------------------------------------------------

/// Erzeugt eine zufällig/zeitbasierte, slug-sichere Vorschlags-ID.
///
/// Kein Zufallszahlengenerator als neue Abhängigkeit nötig: Millisekunden
/// seit der Unix-Epoche, Prozess-ID und ein prozessweiter Zähler ergeben in
/// Kombination eine praktisch eindeutige, rein aus `[a-z0-9-]` bestehende ID.
fn generate_proposal_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{millis:x}-{:x}-{counter:x}", std::process::id())
}

/// Das `.proposals/`-Verzeichnis unterhalb von `profile_agents_dir`.
fn proposals_root(profile_agents_dir: &Path) -> PathBuf {
    profile_agents_dir.join(PROPOSALS_DIR_NAME)
}

/// Das Verzeichnis eines einzelnen Vorschlags.
fn proposal_dir(profile_agents_dir: &Path, proposal_id: &str) -> PathBuf {
    proposals_root(profile_agents_dir).join(proposal_id)
}

/// Liest und parst `proposal.json` eines Vorschlags.
fn read_proposal_json(dir: &Path) -> Result<serde_json::Value, String> {
    let path = dir.join("proposal.json");
    let source = std::fs::read_to_string(&path)
        .map_err(|error| format!("Vorschlag {} nicht lesbar: {error}", path.display()))?;
    serde_json::from_str(&source).map_err(|error| {
        format!(
            "Vorschlag {} nicht lesbar (defektes JSON): {error}",
            path.display()
        )
    })
}

/// Schreibt `value` atomar als `proposal.json` in `dir`.
fn write_proposal_json(dir: &Path, value: &serde_json::Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("Vorschlags-Metadaten nicht serialisierbar: {error}"))?;
    write_atomic(&dir.join("proposal.json"), &bytes, DEFINITION_FILE_MODE)
        .map_err(|error| format!("Vorschlags-Metadaten nicht schreibbar: {error}"))
}

/// Legt einen `kind = "definition"`-Vorschlag an und gibt seine ID zurück.
#[allow(clippy::too_many_arguments)]
fn propose_definition(
    profile_agents_dir: &Path,
    scope: &str,
    name: &str,
    toml_source: &str,
    evaluated: &EvaluatedCandidate,
    review_level: &str,
    existing_target: Option<&Path>,
) -> Result<String, String> {
    let proposal_id = generate_proposal_id();
    let dir = proposal_dir(profile_agents_dir, &proposal_id);
    write_atomic(
        &dir.join("definition.toml"),
        toml_source.as_bytes(),
        DEFINITION_FILE_MODE,
    )
    .map_err(|error| format!("Vorschlag nicht schreibbar: {error}"))?;
    let old_content = existing_target.and_then(|path| std::fs::read_to_string(path).ok());
    let diff = simple_line_diff(old_content.as_deref(), toml_source);
    let created_at = now();
    let expires_at = created_at + time::Duration::days(PROPOSAL_TTL_DAYS);
    write_proposal_json(
        &dir,
        &serde_json::json!({
            "proposal_id": proposal_id,
            "kind": "definition",
            "scope": scope,
            "name": name,
            "dir_name": serde_json::Value::Null,
            "created_at": format_rfc3339(created_at),
            "expires_at": format_rfc3339(expires_at),
            "status": "pending_uia_review",
            "review_level": review_level,
            "author_role": role_key(evaluated.ir.role()),
            "rights_delta_author": evaluated.rights_delta_author.to_json(),
            "rights_delta_base_role": evaluated.rights_delta_base_role.to_json(),
            "diff": diff,
            "validation": {
                "ok": true,
                "role": role_key(evaluated.ir.role()),
                "id": evaluated.ir.id().to_string(),
            },
        }),
    )?;
    Ok(proposal_id)
}

/// Legt einen `kind = "uia"`-Vorschlag an und gibt seine ID zurück.
#[allow(clippy::too_many_arguments)]
fn propose_uia(
    profile_agents_dir: &Path,
    dir_name: &str,
    definition_toml: &str,
    agent_toml: &str,
    identity_md: Option<&str>,
    personality_md: &str,
    user_md: &str,
    evaluated: &EvaluatedCandidate,
    review_level: &str,
) -> Result<String, String> {
    let proposal_id = generate_proposal_id();
    let dir = proposal_dir(profile_agents_dir, &proposal_id);
    let mut files: Vec<(&str, &str)> = vec![
        ("definition.toml", definition_toml),
        ("agent.toml", agent_toml),
        ("Personality.md", personality_md),
        ("USER.md", user_md),
    ];
    if let Some(identity_md) = identity_md {
        files.push(("identity.md", identity_md));
    }
    for (file_name, content) in files {
        write_atomic(&dir.join(file_name), content.as_bytes(), UIA_FILE_MODE)
            .map_err(|error| format!("Vorschlag nicht schreibbar ({file_name}): {error}"))?;
    }
    let existing_definition = profile_agents_dir.join(dir_name).join("definition.toml");
    let old_content = std::fs::read_to_string(&existing_definition).ok();
    let diff = simple_line_diff(old_content.as_deref(), definition_toml);
    let created_at = now();
    let expires_at = created_at + time::Duration::days(PROPOSAL_TTL_DAYS);
    write_proposal_json(
        &dir,
        &serde_json::json!({
            "proposal_id": proposal_id,
            "kind": "uia",
            "scope": serde_json::Value::Null,
            "name": serde_json::Value::Null,
            "dir_name": dir_name,
            "created_at": format_rfc3339(created_at),
            "expires_at": format_rfc3339(expires_at),
            "status": "pending_uia_review",
            "review_level": review_level,
            "author_role": role_key(evaluated.ir.role()),
            "rights_delta_author": evaluated.rights_delta_author.to_json(),
            "rights_delta_base_role": evaluated.rights_delta_base_role.to_json(),
            "diff": diff,
            "validation": {
                "ok": true,
                "role": role_key(evaluated.ir.role()),
                "id": evaluated.ir.id().to_string(),
            },
        }),
    )?;
    Ok(proposal_id)
}

// ---------------------------------------------------------------------------
// `agents.validate`
// ---------------------------------------------------------------------------

/// Deserialisierte Argumente für `agents.validate`.
#[derive(Debug, Deserialize)]
struct ValidateArgs {
    /// Der zu validierende TOML-Quelltext.
    toml: String,
}

fn agents_validate_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "toml".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("TOML-Quelltext einer Agentendefinition.".to_owned()),
            ..Default::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("agents.validate"),
        description: "Validiert eine Agentendefinition (parse_toml → resolve_definition → \
             lower, gegen die eingebauten Rollen als Basis-Schichten). Schreibt nichts. \
             Gibt {ok, role?, id?, errors[]} zurück."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec!["toml".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

/// Baut das `agents.validate`-Ergebnis aus einem [`Validated`].
fn validate_output(validated: Validated) -> ToolOutput {
    match validated {
        Validated::Ok { ir, .. } => ToolOutput::json(serde_json::json!({
            "ok": true,
            "role": role_key(ir.role()),
            "id": ir.id().to_string(),
            "errors": Vec::<String>::new(),
        })),
        Validated::Err(errors) => ToolOutput::json(serde_json::json!({
            "ok": false,
            "role": serde_json::Value::Null,
            "id": serde_json::Value::Null,
            "errors": errors,
        })),
    }
}

struct AgentsValidateExecutor;

impl ToolExecutor for AgentsValidateExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let arguments = call.arguments.clone();
        Box::pin(async move {
            let args: ValidateArgs = match parse_args("agents.validate", &arguments) {
                Ok(args) => args,
                Err(out) => return Ok(out),
            };
            Ok(validate_output(validate_definition_toml(&args.toml)))
        })
    }
}

// ---------------------------------------------------------------------------
// `agents.write_definition`
// ---------------------------------------------------------------------------

/// Deserialisierte Argumente für `agents.write_definition`.
#[derive(Debug, Deserialize)]
struct WriteDefinitionArgs {
    /// `"project"`, `"profile"` oder `"run"`.
    scope: String,
    /// Dateiname ohne `.toml`; muss `[a-z0-9-]+` sein.
    name: String,
    /// Der zu schreibende TOML-Quelltext.
    toml: String,
    /// Pflicht bei `scope = "run"`, sonst unzulässig.
    #[serde(default)]
    run_id: Option<String>,
}

fn agents_write_definition_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "scope".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Zielschicht: \"project\", \"profile\" oder \"run\" (auftragsgebunden, \
                 nur bei leeren Rechte-Deltas)."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "name".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Dateiname ohne '.toml', nur [a-z0-9-], kein Pfadtrenner.".to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "toml".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("TOML-Quelltext der Agentendefinition.".to_owned()),
            ..Default::default()
        },
    );
    props.insert(
        "run_id".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Pflicht bei scope = \"run\": Slug des Auftrags, unter dem sofort geschrieben \
                 wird (nur wenn beide Rechte-Deltas leer sind)."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("agents.write_definition"),
        description: "Validiert eine Agentendefinition zwingend und berechnet die Rechte-Deltas \
             gegen die Urheber-Decke und die Basisrolle. Ein nicht-leeres Urheber-Delta lehnt \
             hart ab (nichts wird geschrieben, auch kein Vorschlag). Bei scope = \"run\" mit \
             leeren Deltas wird sofort nach <project>/../state/runs/<run_id>/agents/<name>/definition.toml \
             geschrieben (review_level \"none\"). Sonst wird immer nur ein Vorschlag abgelegt, \
             den die UIA über agents.commit_proposal freigeben muss (review_level \"uia\" oder \
             \"user_required\"). Freigabepflichtig."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec![
                "scope".to_owned(),
                "name".to_owned(),
                "toml".to_owned(),
            ]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

struct AgentsWriteDefinitionExecutor {
    project_agents_dir: Option<PathBuf>,
    profile_agents_dir: Option<PathBuf>,
    ceiling: DefinitionAuthorCeiling,
}

impl AgentsWriteDefinitionExecutor {
    fn write(&self, call: &ToolCall) -> ToolOutput {
        let args: WriteDefinitionArgs = match parse_args("agents.write_definition", &call.arguments)
        {
            Ok(args) => args,
            Err(out) => return out,
        };
        if !matches!(args.scope.as_str(), "project" | "profile" | "run") {
            return ToolOutput::error(format!(
                "agents.write_definition: unbekannter scope '{}' \
                 (erwartet \"project\", \"profile\" oder \"run\")",
                args.scope
            ));
        }
        if !is_valid_slug(&args.name) {
            return ToolOutput::error(format!(
                "agents.write_definition: '{}' ist kein gültiger Name (nur [a-z0-9-], nicht leer)",
                args.name
            ));
        }
        if args.scope == "run" {
            match &args.run_id {
                Some(run_id) if is_valid_slug(run_id) => {}
                Some(_) => {
                    return ToolOutput::error(
                        "agents.write_definition: run_id ist kein gültiger Slug (nur [a-z0-9-])"
                            .to_owned(),
                    );
                }
                None => {
                    return ToolOutput::error(
                        "agents.write_definition: run_id ist bei scope = \"run\" Pflicht"
                            .to_owned(),
                    );
                }
            }
        } else if args.run_id.is_some() {
            return ToolOutput::error(
                "agents.write_definition: run_id ist nur bei scope = \"run\" zulässig".to_owned(),
            );
        }

        let evaluated = match evaluate_candidate(&args.toml, &self.ceiling) {
            Ok(evaluated) => evaluated,
            Err(errors) => {
                return ToolOutput::json(serde_json::json!({
                    "ok": false,
                    "written": false,
                    "errors": errors,
                }));
            }
        };
        if !evaluated.rights_delta_author.is_empty() {
            return author_elevation_rejection(&evaluated.rights_delta_author);
        }

        if args.scope == "run" {
            if !evaluated.rights_delta_base_role.is_empty() {
                return ToolOutput::json(serde_json::json!({
                    "ok": false,
                    "written": false,
                    "errors": ["scope = \"run\" erfordert, dass beide Rechte-Deltas leer sind"],
                    "rights_delta_base_role": evaluated.rights_delta_base_role.to_json(),
                }));
            }
            let Some(project_agents_dir) = &self.project_agents_dir else {
                return ToolOutput::error(
                    "agents.write_definition: kein Projekt-Verzeichnis konfiguriert (scope = \"run\")"
                        .to_owned(),
                );
            };
            // `run_id` wurde oben bereits als Some+Slug geprüft.
            let run_id = args.run_id.as_deref().unwrap_or_default();
            let target_dir = project_agents_dir
                .join("..")
                .join("state")
                .join("runs")
                .join(run_id)
                .join("agents");
            return match commit_definition(
                &target_dir,
                &args.name,
                &args.toml,
                &evaluated.ir.id().to_string(),
            ) {
                Ok(path) => ToolOutput::json(serde_json::json!({
                    "ok": true,
                    "written": true,
                    "path": path.display().to_string(),
                    "id": evaluated.ir.id().to_string(),
                    "review_level": "none",
                    "errors": Vec::<String>::new(),
                })),
                Err(reason) => ToolOutput::json(serde_json::json!({
                    "ok": false,
                    "written": false,
                    "errors": [reason],
                })),
            };
        }

        let Some(profile_agents_dir) = &self.profile_agents_dir else {
            return ToolOutput::error(
                "agents.write_definition: kein Profil-Verzeichnis konfiguriert — Vorschläge \
                 können nicht abgelegt werden"
                    .to_owned(),
            );
        };
        let target_dir = match args.scope.as_str() {
            "project" => &self.project_agents_dir,
            _ => &self.profile_agents_dir,
        };
        let existing_target = target_dir
            .as_ref()
            .map(|dir| existing_definition_path(dir, &args.name));
        let review_level = review_level_for("definition", &evaluated.rights_delta_base_role);
        match propose_definition(
            profile_agents_dir,
            &args.scope,
            &args.name,
            &args.toml,
            &evaluated,
            review_level,
            existing_target.as_deref(),
        ) {
            Ok(proposal_id) => ToolOutput::json(serde_json::json!({
                "ok": true,
                "written": false,
                "proposal_id": proposal_id,
                "status": "pending_uia_review",
                "review_level": review_level,
                "note": "Vorschlag abgelegt, noch nicht wirksam. Die UIA muss ihn prüfen und \
                          über agents.commit_proposal freigeben.",
                "errors": Vec::<String>::new(),
            })),
            Err(reason) => ToolOutput::json(serde_json::json!({
                "ok": false,
                "written": false,
                "errors": [reason],
            })),
        }
    }
}

impl ToolExecutor for AgentsWriteDefinitionExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(self.write(call)) })
    }
}

// ---------------------------------------------------------------------------
// `agents.write_uia` (Nachtrag K)
// ---------------------------------------------------------------------------

/// Deserialisierte Argumente für `agents.write_uia`.
#[derive(Debug, Deserialize)]
struct WriteUiaArgs {
    /// Verzeichnisname des Bundles unter `<profil>/agents/`; muss `[a-z0-9-]+` sein.
    dir_name: String,
    /// TOML-Quelltext von `definition.toml`; muss `role = "user-interface"` tragen.
    definition_toml: String,
    /// Inhalt von `Personality.md`.
    personality_md: String,
    /// Optionaler Inhalt von `USER.md`; ohne Angabe wird ein leeres Muster
    /// geschrieben (dieselbe Platzhalterform wie `uia_bootstrap::write_generated_uia`).
    #[serde(default)]
    user_md: Option<String>,
    /// Optionaler frei formulierter Identitätstext; landet, wenn angegeben,
    /// als eigene `identity.md` im Bundle (siehe [`commit_uia_bundle`]/
    /// [`propose_uia`]).
    #[serde(default)]
    identity_md: Option<String>,
}

fn agents_write_uia_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "dir_name".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Bundle-Verzeichnisname unter <profil>/agents/, nur [a-z0-9-].".to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "definition_toml".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "TOML-Quelltext von definition.toml; role muss \"user-interface\" sein.".to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "personality_md".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("Inhalt von Personality.md (Ton, Antwortverhalten).".to_owned()),
            ..Default::default()
        },
    );
    props.insert(
        "user_md".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Optionaler Inhalt von USER.md (freiwilliger Nutzerkontext).".to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "identity_md".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Optionaler Identitätstext (wer/was die UIA selbst ist); wird als eigene \
                 identity.md im Bundle abgelegt. Ohne Angabe wird identity.md nicht angelegt."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("agents.write_uia"),
        description: "Validiert ein UIA-Bundle zwingend, verlangt role = \"user-interface\" und \
             lehnt offensichtliche Geheimnisse ab. Berechnet die Rechte-Deltas wie \
             agents.write_definition; ein nicht-leeres Urheber-Delta lehnt hart ab. Legt sonst \
             immer einen Vorschlag mit review_level \"user_required\" ab (UIAs aktivieren sich \
             nie selbst) — die UIA muss ihn über agents.commit_proposal freigeben. \
             Freigabepflichtig."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec![
                "dir_name".to_owned(),
                "definition_toml".to_owned(),
                "personality_md".to_owned(),
            ]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

/// Platzhaltertext für `USER.md`, wenn `user_md` nicht angegeben wurde —
/// dieselbe Form wie `harw-cli/src/uia_bootstrap.rs::write_generated_uia`.
const DEFAULT_USER_MD: &str = "# Nutzerkontext\n\n<!-- Trage hier freiwillig bereitgestellte Präferenzen, Arbeitsweisen und relevante Kontextinformationen ein. Keine Geheimnisse eintragen. -->\n";

/// Aktivierungs-Hinweistext für ein erfolgreich freigegebenes UIA-Bundle
/// (Nachtrag K: "Ergebnistext nennt, wie der Nutzer aktivieren kann").
fn activation_hint(id: &str) -> String {
    format!(
        "Nicht aktiviert. Zum Aktivieren active_uia_definition = \"{id}\" in der \
         Harness-Config (harness.toml) setzen."
    )
}

struct AgentsWriteUiaExecutor {
    profile_agents_dir: Option<PathBuf>,
    ceiling: DefinitionAuthorCeiling,
}

impl AgentsWriteUiaExecutor {
    fn write(&self, call: &ToolCall) -> ToolOutput {
        let args: WriteUiaArgs = match parse_args("agents.write_uia", &call.arguments) {
            Ok(args) => args,
            Err(out) => return out,
        };

        let Some(profile_agents_dir) = &self.profile_agents_dir else {
            return ToolOutput::error(
                "agents.write_uia: kein Profil-Verzeichnis konfiguriert — UIA-Bundles werden \
                 nie ins Projekt geschrieben"
                    .to_owned(),
            );
        };
        if !is_valid_slug(&args.dir_name) {
            return ToolOutput::error(format!(
                "agents.write_uia: '{}' ist kein gültiger Verzeichnisname (nur [a-z0-9-], nicht leer)",
                args.dir_name
            ));
        }

        let user_md = args
            .user_md
            .clone()
            .unwrap_or_else(|| DEFAULT_USER_MD.to_owned());
        let secret_errors = scan_for_secrets(&[
            ("definition_toml", &args.definition_toml),
            ("personality_md", &args.personality_md),
            ("user_md", &user_md),
            ("identity_md", args.identity_md.as_deref().unwrap_or("")),
        ]);
        if !secret_errors.is_empty() {
            return ToolOutput::json(serde_json::json!({
                "ok": false,
                "written": false,
                "errors": secret_errors,
            }));
        }

        let evaluated = match evaluate_candidate(&args.definition_toml, &self.ceiling) {
            Ok(evaluated) => evaluated,
            Err(errors) => {
                return ToolOutput::json(serde_json::json!({
                    "ok": false,
                    "written": false,
                    "errors": errors,
                }));
            }
        };
        if evaluated.ir.role() != AgentRoleId::UserInterface {
            return ToolOutput::json(serde_json::json!({
                "ok": false,
                "written": false,
                "errors": [format!(
                    "agents.write_uia: definition_toml hat role '{}', erwartet 'user-interface'",
                    role_key(evaluated.ir.role())
                )],
            }));
        }
        if !evaluated.rights_delta_author.is_empty() {
            return author_elevation_rejection(&evaluated.rights_delta_author);
        }

        let agent_toml = build_agent_toml(&evaluated.raw);
        let review_level = review_level_for("uia", &evaluated.rights_delta_base_role);
        match propose_uia(
            profile_agents_dir,
            &args.dir_name,
            &args.definition_toml,
            &agent_toml,
            args.identity_md.as_deref(),
            &args.personality_md,
            &user_md,
            &evaluated,
            review_level,
        ) {
            Ok(proposal_id) => ToolOutput::json(serde_json::json!({
                "ok": true,
                "written": false,
                "proposal_id": proposal_id,
                "status": "pending_uia_review",
                "review_level": review_level,
                "note": "Vorschlag abgelegt, noch nicht wirksam. Die UIA muss ihn prüfen und \
                          über agents.commit_proposal freigeben.",
                "errors": Vec::<String>::new(),
            })),
            Err(reason) => ToolOutput::json(serde_json::json!({
                "ok": false,
                "written": false,
                "errors": [reason],
            })),
        }
    }
}

impl ToolExecutor for AgentsWriteUiaExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(self.write(call)) })
    }
}

// ---------------------------------------------------------------------------
// `agents.list_proposals`
// ---------------------------------------------------------------------------

fn agents_list_proposals_spec() -> ToolSpec {
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("agents.list_proposals"),
        description: "Listet alle abgelegten Vorschläge unter <profile_agents_dir>/.proposals/ \
             mit proposal_id, kind, scope/dir_name, review_level, created_at, expires_at, \
             status und expired. Rein lesend."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(BTreeMap::new()),
            required: Some(Vec::new()),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

struct AgentsListProposalsExecutor {
    profile_agents_dir: Option<PathBuf>,
}

impl AgentsListProposalsExecutor {
    fn list(&self) -> ToolOutput {
        let Some(profile_agents_dir) = &self.profile_agents_dir else {
            return ToolOutput::json(serde_json::json!({ "ok": true, "proposals": [] }));
        };
        let root = proposals_root(profile_agents_dir);
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ToolOutput::json(serde_json::json!({ "ok": true, "proposals": [] }));
            }
            Err(error) => {
                return ToolOutput::error(format!(
                    "agents.list_proposals: {} nicht lesbar: {error}",
                    root.display()
                ));
            }
        };

        let mut proposals = Vec::new();
        for entry in entries.flatten() {
            if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }
            match read_proposal_json(&entry.path()) {
                Ok(mut value) => {
                    let expired = value
                        .get("expires_at")
                        .and_then(|v| v.as_str())
                        .map(is_expired)
                        .unwrap_or(false);
                    if let Some(object) = value.as_object_mut() {
                        object.insert("expired".to_owned(), serde_json::Value::Bool(expired));
                    }
                    proposals.push(value);
                }
                Err(reason) => proposals.push(serde_json::json!({
                    "proposal_id": entry.file_name().to_string_lossy(),
                    "error": reason,
                })),
            }
        }
        proposals.sort_by(|a, b| {
            let a = a
                .get("proposal_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let b = b
                .get("proposal_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            a.cmp(b)
        });

        ToolOutput::json(serde_json::json!({ "ok": true, "proposals": proposals }))
    }
}

impl ToolExecutor for AgentsListProposalsExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(self.list()) })
    }
}

// ---------------------------------------------------------------------------
// `agents.commit_proposal` (nur `DefinitionWriteMode::Commit` + gesetzte Decke)
// ---------------------------------------------------------------------------

/// Deserialisierte Argumente für `agents.commit_proposal`.
#[derive(Debug, Deserialize)]
struct CommitProposalArgs {
    proposal_id: String,
    /// Pflicht (`true`), wenn der neu berechnete `review_level` dieses
    /// Providers `"user_required"` ist. Siehe Moduldoku, Abschnitt
    /// „`user_confirmed` und die Freigabe-Kette" — dieses Feld ersetzt die
    /// Freigabe-Prüfung des Harness nicht, sondern setzt sie voraus.
    #[serde(default)]
    user_confirmed: bool,
}

fn agents_commit_proposal_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "proposal_id".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "ID des freizugebenden Vorschlags (aus agents.list_proposals).".to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "user_confirmed".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Boolean),
            description: Some(
                "Pflicht (true), wenn der neu berechnete review_level \"user_required\" ist \
                 (jedes UIA-Bundle, oder ein Basisrollen-Delta). Dieses Werkzeug ist selbst \
                 freigabepflichtig — die eigentliche Nutzerbestätigung läuft über die \
                 Freigabe-Kette des Harness, bevor dieser Aufruf überhaupt ausgeführt wird."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("agents.commit_proposal"),
        description: "Validiert einen abgelegten, nicht abgelaufenen Vorschlag erneut und \
             berechnet beide Rechte-Deltas neu gegen die Decke DIESES Aufrufers (kann von der \
             Decke des ursprünglichen Autors abweichen). Ein Urheber-Delta lehnt hart ab. Bei \
             review_level \"user_required\" ist user_confirmed = true Pflicht. Schreibt danach \
             ans Ziel wie agents.write_definition/agents.write_uia. UIA-Bundles werden dadurch \
             nicht aktiviert. Freigabepflichtig."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec!["proposal_id".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

struct AgentsCommitProposalExecutor {
    project_agents_dir: Option<PathBuf>,
    profile_agents_dir: Option<PathBuf>,
    ceiling: DefinitionAuthorCeiling,
}

impl AgentsCommitProposalExecutor {
    fn commit(&self, call: &ToolCall) -> ToolOutput {
        let args: CommitProposalArgs = match parse_args("agents.commit_proposal", &call.arguments) {
            Ok(args) => args,
            Err(out) => return out,
        };
        if !is_valid_slug(&args.proposal_id) {
            return ToolOutput::error(format!(
                "agents.commit_proposal: '{}' ist keine gültige proposal_id",
                args.proposal_id
            ));
        }
        let Some(profile_agents_dir) = &self.profile_agents_dir else {
            return ToolOutput::error(
                "agents.commit_proposal: kein Profil-Verzeichnis konfiguriert".to_owned(),
            );
        };

        let dir = proposal_dir(profile_agents_dir, &args.proposal_id);
        let proposal = match read_proposal_json(&dir) {
            Ok(value) => value,
            Err(reason) => return ToolOutput::error(format!("agents.commit_proposal: {reason}")),
        };
        let status = proposal
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if status != "pending_uia_review" {
            return ToolOutput::error(format!(
                "agents.commit_proposal: Vorschlag '{}' hat status '{status}', erwartet \
                 'pending_uia_review'",
                args.proposal_id
            ));
        }
        let expired = proposal
            .get("expires_at")
            .and_then(|v| v.as_str())
            .map(is_expired)
            .unwrap_or(false);
        if expired {
            return ToolOutput::json(serde_json::json!({
                "ok": false,
                "written": false,
                "errors": [format!("Vorschlag '{}' ist abgelaufen", args.proposal_id)],
            }));
        }
        let kind = proposal
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let definition_toml = match std::fs::read_to_string(dir.join("definition.toml")) {
            Ok(source) => source,
            Err(error) => {
                return ToolOutput::error(format!(
                    "agents.commit_proposal: definition.toml von '{}' nicht lesbar: {error}",
                    args.proposal_id
                ));
            }
        };

        // Erneute Validierung UND erneute Deltas — jeweils gegen DIESE Decke
        // (kann von der Decke des ursprünglichen Autors abweichen).
        let evaluated = match evaluate_candidate(&definition_toml, &self.ceiling) {
            Ok(evaluated) => evaluated,
            Err(errors) => {
                return ToolOutput::json(serde_json::json!({
                    "ok": false,
                    "written": false,
                    "errors": errors,
                }));
            }
        };
        if kind == "uia" && evaluated.ir.role() != AgentRoleId::UserInterface {
            return ToolOutput::json(serde_json::json!({
                "ok": false,
                "written": false,
                "errors": [format!(
                    "agents.commit_proposal: definition_toml hat role '{}', erwartet \
                     'user-interface'",
                    role_key(evaluated.ir.role())
                )],
            }));
        }
        if !evaluated.rights_delta_author.is_empty() {
            return author_elevation_rejection(&evaluated.rights_delta_author);
        }

        let review_level = review_level_for(kind, &evaluated.rights_delta_base_role);
        if review_level == "user_required" && !args.user_confirmed {
            return ToolOutput::json(serde_json::json!({
                "ok": false,
                "written": false,
                "review_level": review_level,
                "requires_user_approval": true,
                "errors": ["review_level ist \"user_required\" — user_confirmed = true ist Pflicht \
                            (siehe Freigabe-Kette in der Moduldoku)"],
            }));
        }

        let commit_result: Result<(String, Option<String>), String> = match kind {
            "definition" => {
                let scope = proposal
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let name = proposal
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let target_dir = match scope {
                    "project" => &self.project_agents_dir,
                    _ => &self.profile_agents_dir,
                };
                match target_dir {
                    Some(target_dir) => commit_definition(
                        target_dir,
                        name,
                        &definition_toml,
                        &evaluated.ir.id().to_string(),
                    )
                    .map(|path| (path.display().to_string(), None)),
                    None => Err(format!("kein Verzeichnis für scope '{scope}' konfiguriert")),
                }
            }
            "uia" => {
                let secret_errors = scan_for_secrets(&[("definition_toml", &definition_toml)]);
                if !secret_errors.is_empty() {
                    Err(secret_errors.join("; "))
                } else {
                    let dir_name = proposal
                        .get("dir_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let agent_toml =
                        std::fs::read_to_string(dir.join("agent.toml")).unwrap_or_default();
                    // `identity.md` ist im Vorschlag nur vorhanden, wenn
                    // `identity_md` beim Ablegen angegeben wurde
                    // (`propose_uia` lässt die Datei sonst weg).
                    let identity_md = std::fs::read_to_string(dir.join("identity.md")).ok();
                    let personality_md =
                        std::fs::read_to_string(dir.join("Personality.md")).unwrap_or_default();
                    let user_md = std::fs::read_to_string(dir.join("USER.md")).unwrap_or_default();
                    let bundle_dir = profile_agents_dir.join(dir_name);
                    let new_id = evaluated.ir.id().to_string();
                    commit_uia_bundle(
                        &bundle_dir,
                        &new_id,
                        &definition_toml,
                        &agent_toml,
                        identity_md.as_deref(),
                        &personality_md,
                        &user_md,
                    )
                    .map(|()| {
                        (
                            bundle_dir.display().to_string(),
                            Some(activation_hint(&new_id)),
                        )
                    })
                }
            }
            other => Err(format!("unbekannte Vorschlagsart '{other}'")),
        };

        match commit_result {
            Ok((path, hint)) => {
                let mut updated = proposal.clone();
                if let Some(object) = updated.as_object_mut() {
                    object.insert(
                        "status".to_owned(),
                        serde_json::Value::String("committed".to_owned()),
                    );
                    object.insert(
                        "committed_at".to_owned(),
                        serde_json::Value::String(now_rfc3339()),
                    );
                    object.insert(
                        "user_confirmed".to_owned(),
                        serde_json::Value::Bool(args.user_confirmed),
                    );
                }
                // Ziel ist bereits geschrieben; ein Fehler beim Status-Update
                // ist nicht mehr rückgängig zu machen (kein Rollback des
                // Ziel-Schreibens) und wird deshalb nur als zusätzlicher
                // Eintrag in `errors` gemeldet, nicht als harter Fehlschlag —
                // dieses Crate hat keine `tracing`-Abhängigkeit (siehe Bericht).
                let mut status_update_errors: Vec<String> = Vec::new();
                if let Err(reason) = write_proposal_json(&dir, &updated) {
                    status_update_errors.push(format!(
                        "Ziel geschrieben, aber Vorschlags-Status nicht aktualisierbar: {reason}"
                    ));
                }
                let mut result = serde_json::json!({
                    "ok": true,
                    "written": true,
                    "proposal_id": args.proposal_id,
                    "path": path,
                    "id": evaluated.ir.id().to_string(),
                    "review_level": review_level,
                    "requires_user_approval": review_level == "user_required",
                    "errors": status_update_errors,
                });
                if let (Some(hint), Some(object)) = (hint, result.as_object_mut()) {
                    object.insert(
                        "activation_hint".to_owned(),
                        serde_json::Value::String(hint),
                    );
                }
                ToolOutput::json(result)
            }
            Err(reason) => ToolOutput::json(serde_json::json!({
                "ok": false,
                "written": false,
                "errors": [reason],
            })),
        }
    }
}

impl ToolExecutor for AgentsCommitProposalExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(self.commit(call)) })
    }
}

// ---------------------------------------------------------------------------
// `agents.reject_proposal` (nur `DefinitionWriteMode::Commit` + gesetzte Decke)
// ---------------------------------------------------------------------------

/// Deserialisierte Argumente für `agents.reject_proposal`.
#[derive(Debug, Deserialize)]
struct RejectProposalArgs {
    proposal_id: String,
    reason: String,
}

fn agents_reject_proposal_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "proposal_id".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("ID des abzulehnenden Vorschlags.".to_owned()),
            ..Default::default()
        },
    );
    props.insert(
        "reason".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("Menschenlesbare Begründung der Ablehnung.".to_owned()),
            ..Default::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("agents.reject_proposal"),
        description: "Markiert einen Vorschlag als 'rejected' mit Begründung. Schreibt nichts \
             an ein Ziel. Nur für einen Vorschlag mit status = \"pending_uia_review\". \
             Freigabepflichtig."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec!["proposal_id".to_owned(), "reason".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

struct AgentsRejectProposalExecutor {
    profile_agents_dir: Option<PathBuf>,
}

impl AgentsRejectProposalExecutor {
    fn reject(&self, call: &ToolCall) -> ToolOutput {
        let args: RejectProposalArgs = match parse_args("agents.reject_proposal", &call.arguments) {
            Ok(args) => args,
            Err(out) => return out,
        };
        if !is_valid_slug(&args.proposal_id) {
            return ToolOutput::error(format!(
                "agents.reject_proposal: '{}' ist keine gültige proposal_id",
                args.proposal_id
            ));
        }
        let Some(profile_agents_dir) = &self.profile_agents_dir else {
            return ToolOutput::error(
                "agents.reject_proposal: kein Profil-Verzeichnis konfiguriert".to_owned(),
            );
        };

        let dir = proposal_dir(profile_agents_dir, &args.proposal_id);
        let mut proposal = match read_proposal_json(&dir) {
            Ok(value) => value,
            Err(reason) => return ToolOutput::error(format!("agents.reject_proposal: {reason}")),
        };
        let status = proposal
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if status != "pending_uia_review" {
            return ToolOutput::error(format!(
                "agents.reject_proposal: Vorschlag '{}' hat status '{status}', erwartet \
                 'pending_uia_review'",
                args.proposal_id
            ));
        }

        if let Some(object) = proposal.as_object_mut() {
            object.insert(
                "status".to_owned(),
                serde_json::Value::String("rejected".to_owned()),
            );
            object.insert(
                "rejected_at".to_owned(),
                serde_json::Value::String(now_rfc3339()),
            );
            object.insert(
                "reason".to_owned(),
                serde_json::Value::String(args.reason.clone()),
            );
        }
        match write_proposal_json(&dir, &proposal) {
            Ok(()) => ToolOutput::json(serde_json::json!({
                "ok": true,
                "proposal_id": args.proposal_id,
                "status": "rejected",
            })),
            Err(reason) => ToolOutput::error(format!("agents.reject_proposal: {reason}")),
        }
    }
}

impl ToolExecutor for AgentsRejectProposalExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(self.reject(call)) })
    }
}

// ---------------------------------------------------------------------------
// `uia_self.update_document` (Structure Plan §4)
// ---------------------------------------------------------------------------

/// Ziel-Datei eines `uia_self.update_document`-Aufrufs. Ein geschlossenes
/// Enum statt eines freien Pfad-Parameters: verhindert Pfad-Traversal
/// strukturell, ohne eine eigene Prüfung zu benötigen (siehe Structure Plan
/// §4, "Pfad-Sicherheit").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelfDocumentTarget {
    Identity,
    User,
    Personality,
}

impl SelfDocumentTarget {
    /// Parst den `target`-Parameter; `None` für jeden unbekannten Wert.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "identity" => Some(Self::Identity),
            "user" => Some(Self::User),
            "personality" => Some(Self::Personality),
            _ => None,
        }
    }

    /// Der Dateiname im Agentenordner, den dieses Ziel beschreibt.
    fn file_name(self) -> &'static str {
        match self {
            Self::Identity => "identity.md",
            Self::User => "USER.md",
            Self::Personality => "Personality.md",
        }
    }
}

/// Deserialisierte Argumente für `uia_self.update_document`.
#[derive(Debug, Deserialize)]
struct UpdateDocumentArgs {
    /// `"identity"`, `"user"` oder `"personality"`.
    target: String,
    /// Vollständiger neuer Dateiinhalt (kein Patch/Diff).
    content: String,
    /// Kurze Begründung; erscheint im Freigabe-Prompt und im Ergebnis, wird
    /// aber nicht in die Zieldatei geschrieben.
    reason: String,
}

fn uia_self_update_document_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "target".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "\"identity\" (identity.md), \"user\" (USER.md) oder \"personality\" \
                 (Personality.md) — immer im eigenen Agentenordner dieser UIA-Sitzung."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "content".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("Vollständiger neuer Dateiinhalt.".to_owned()),
            ..Default::default()
        },
    );
    props.insert(
        "reason".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Kurze Begründung der Änderung; erscheint im Freigabe-Prompt.".to_owned(),
            ),
            ..Default::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("uia_self.update_document"),
        description: "Pflegt identity.md, USER.md oder Personality.md der EIGENEN \
             UIA-Sitzung (kein Fremdzugriff auf andere Agentenordner). Lehnt offensichtliche \
             Geheimnisse ab und zeigt vor dem Schreiben ein einfaches Zeilen-Diff. Rohe \
             Gedächtnisnotizen laufen NICHT über dieses Werkzeug, sondern über \
             Memory::record(Signal). Nur für die Rolle user-interface registriert. \
             Freigabepflichtig."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec![
                "target".to_owned(),
                "content".to_owned(),
                "reason".to_owned(),
            ]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

/// Rechte-Bits der von `uia_self.update_document` geschriebenen Dateien —
/// dieselbe Sichtbarkeit wie die übrigen UIA-Bundle-Dateien.
const SELF_DOCUMENT_FILE_MODE: u32 = UIA_FILE_MODE;

struct UiaSelfUpdateDocumentExecutor {
    /// Der Agentenordner der aufrufenden UIA-Sitzung selbst — nie ein
    /// fremder Ordner, nie aus dem Aufrufargument übernommen.
    agent_dir: PathBuf,
}

impl UiaSelfUpdateDocumentExecutor {
    fn write(&self, call: &ToolCall) -> ToolOutput {
        let args: UpdateDocumentArgs = match parse_args("uia_self.update_document", &call.arguments)
        {
            Ok(args) => args,
            Err(out) => return out,
        };
        let Some(target) = SelfDocumentTarget::parse(&args.target) else {
            return ToolOutput::error(format!(
                "uia_self.update_document: unbekanntes target '{}' \
                 (erwartet \"identity\", \"user\" oder \"personality\")",
                args.target
            ));
        };
        let secret_errors = scan_for_secrets(&[("content", &args.content)]);
        if !secret_errors.is_empty() {
            return ToolOutput::json(serde_json::json!({
                "ok": false,
                "written": false,
                "errors": secret_errors,
            }));
        }

        let path = self.agent_dir.join(target.file_name());
        let old_content = std::fs::read_to_string(&path).ok();
        let diff = simple_line_diff(old_content.as_deref(), &args.content);
        match write_atomic(&path, args.content.as_bytes(), SELF_DOCUMENT_FILE_MODE) {
            Ok(()) => ToolOutput::json(serde_json::json!({
                "ok": true,
                "written": true,
                "path": path.display().to_string(),
                "target": args.target,
                "reason": args.reason,
                "diff": diff,
                "errors": Vec::<String>::new(),
            })),
            Err(error) => ToolOutput::json(serde_json::json!({
                "ok": false,
                "written": false,
                "errors": [format!("Schreiben von {} fehlgeschlagen: {error}", path.display())],
            })),
        }
    }
}

impl ToolExecutor for UiaSelfUpdateDocumentExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(self.write(call)) })
    }
}

/// Der Provider für `uia_self.update_document` (Structure Plan §4).
///
/// # Description
/// Registriert das Werkzeug **nur**, wenn [`Self::role`] `role =
/// "user-interface"` ist — ein Root-Orchestrator oder Worker, der diesen
/// Provider (versehentlich) mit einer anderen Rolle konstruiert bekäme,
/// sieht das Werkzeug weder im Inventar noch bekommt er einen Executor dafür
/// (fail-closed, wie [`AgentDefinitionToolProvider`] ohne Decke). Wirkt
/// ausschließlich auf [`Self::agent_dir`] — den eigenen Agentenordner der
/// aufrufenden UIA-Sitzung, nie auf einen fremden.
///
/// # Concurrency
/// `Send + Sync`; zustandslos außer Agentenordner und Rolle.
pub struct UiaSelfDocumentToolProvider {
    agent_dir: PathBuf,
    role: AgentRoleId,
}

impl UiaSelfDocumentToolProvider {
    /// Erstellt einen neuen Provider für die gegebene Rolle und den
    /// gegebenen Agentenordner.
    ///
    /// # Arguments
    /// - `agent_dir` (`PathBuf`): der Agentenordner der aufrufenden Sitzung.
    /// - `role` ([`AgentRoleId`]): die organisatorische Rolle der aufrufenden
    ///   Sitzung; nur `AgentRoleId::UserInterface` registriert das Werkzeug.
    ///
    /// # Returns
    /// Den fertig konfigurierten Provider.
    #[must_use]
    pub fn new(agent_dir: PathBuf, role: AgentRoleId) -> Self {
        Self { agent_dir, role }
    }
}

impl ToolProvider for UiaSelfDocumentToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        if self.role == AgentRoleId::UserInterface {
            vec![uia_self_update_document_spec()]
        } else {
            Vec::new()
        }
    }

    fn executor(&self, name: &ToolName) -> Option<std::sync::Arc<dyn ToolExecutor>> {
        if self.role != AgentRoleId::UserInterface {
            return None;
        }
        match name.as_str() {
            "uia_self.update_document" => {
                Some(std::sync::Arc::new(UiaSelfUpdateDocumentExecutor {
                    agent_dir: self.agent_dir.clone(),
                }))
            }
            _ => None,
        }
    }

    /// Schreibend, daher nie commutative.
    fn parallel_safe(&self, _name: &ToolName) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// `ToolProvider`
// ---------------------------------------------------------------------------

impl ToolProvider for AgentDefinitionToolProvider {
    /// Gibt die registrierten Werkzeug-Spezifikationen zurück.
    ///
    /// # Description
    /// `agents.validate` und `agents.list_proposals` sind immer registriert.
    /// `agents.write_definition`/`agents.write_uia` nur, wenn [`Self::ceiling`]
    /// gesetzt ist (fail-closed sonst). `agents.commit_proposal`/
    /// `agents.reject_proposal` zusätzlich nur im [`DefinitionWriteMode::Commit`].
    fn tools(&self) -> Vec<ToolSpec> {
        let mut tools = vec![agents_validate_spec(), agents_list_proposals_spec()];
        if self.ceiling.is_some() {
            tools.push(agents_write_definition_spec());
            tools.push(agents_write_uia_spec());
            if self.mode == DefinitionWriteMode::Commit {
                tools.push(agents_commit_proposal_spec());
                tools.push(agents_reject_proposal_spec());
            }
        }
        tools
    }

    /// Gibt den Executor für den angegebenen Tool-Namen zurück; `None` für
    /// jedes Werkzeug, das laut [`Self::tools`] gerade nicht registriert ist.
    fn executor(&self, name: &ToolName) -> Option<std::sync::Arc<dyn ToolExecutor>> {
        match name.as_str() {
            "agents.validate" => Some(std::sync::Arc::new(AgentsValidateExecutor)),
            "agents.list_proposals" => Some(std::sync::Arc::new(AgentsListProposalsExecutor {
                profile_agents_dir: self.profile_agents_dir.clone(),
            })),
            "agents.write_definition" => self.ceiling.clone().map(|ceiling| {
                std::sync::Arc::new(AgentsWriteDefinitionExecutor {
                    project_agents_dir: self.project_agents_dir.clone(),
                    profile_agents_dir: self.profile_agents_dir.clone(),
                    ceiling,
                }) as std::sync::Arc<dyn ToolExecutor>
            }),
            "agents.write_uia" => self.ceiling.clone().map(|ceiling| {
                std::sync::Arc::new(AgentsWriteUiaExecutor {
                    profile_agents_dir: self.profile_agents_dir.clone(),
                    ceiling,
                }) as std::sync::Arc<dyn ToolExecutor>
            }),
            "agents.commit_proposal" if self.mode == DefinitionWriteMode::Commit => {
                self.ceiling.clone().map(|ceiling| {
                    std::sync::Arc::new(AgentsCommitProposalExecutor {
                        project_agents_dir: self.project_agents_dir.clone(),
                        profile_agents_dir: self.profile_agents_dir.clone(),
                        ceiling,
                    }) as std::sync::Arc<dyn ToolExecutor>
                })
            }
            "agents.reject_proposal" if self.mode == DefinitionWriteMode::Commit => {
                self.ceiling.as_ref().map(|_| {
                    std::sync::Arc::new(AgentsRejectProposalExecutor {
                        profile_agents_dir: self.profile_agents_dir.clone(),
                    }) as std::sync::Arc<dyn ToolExecutor>
                })
            }
            _ => None,
        }
    }

    /// `agents.validate` und `agents.list_proposals` sind rein lesend und
    /// commutative; alle schreibenden Werkzeuge sind es nicht.
    fn parallel_safe(&self, name: &ToolName) -> bool {
        matches!(name.as_str(), "agents.validate" | "agents.list_proposals")
    }
}

// ---------------------------------------------------------------------------
// `agents.build` (#22 Welle 2B, R10-Plan „harw-agent-compiler + CLI")
// ---------------------------------------------------------------------------
//
// Eigenständiger, von [`AgentDefinitionToolProvider`] unabhängiger Provider:
// er kompiliert eine Agentendefinition zu einem eigenständigen Programm
// (`harw-agent-compiler`, derselbe Weg wie `/agent build` in `harw-ops`).
// Läuft **immer** als Hintergrund-Job — auch `--native` (kann Minuten
// dauern) blockiert damit nie den Aufrufer; der Rückgabewert ist die
// Job-Kennung, nicht das fertige Ergebnis. Freigabepflichtig wie
// `shell.exec` (nicht in [`crate::AUTO_APPROVED_TOOLS`]) und **nie** in ein
// eingebautes `RegistryProfile` verdrahtet — nur eine ausdrückliche
// Agentendefinition, die `agents.build` in `tools.admitted` aufführt, darf
// diesen Provider bekommen. Die Entscheidung trifft
// [`agent_build_provider_for`]; die Montage der Wurzel (`harw-runtime`,
// `assembly.rs`) ruft sie mit der IR des aktiven Agenten, den Rechten der
// Sandbox und der Job-Verwaltung der Sitzung auf.

/// Warum [`agent_build_provider_for`] `agents.build` **nicht** registriert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentBuildWithheld {
    /// Keine ausdrückliche Agentendefinition (eingebaute Rolle, UIA ohne
    /// `--agent`): eingebaute Rollen bekommen `agents.build` nie von sich aus.
    NoExplicitDefinition,
    /// Die Definition nennt `agents.build` nicht in `tools.admitted` (oder
    /// verbietet es ausdrücklich in `tools.forbidden`).
    NotAdmitted,
    /// Die Rechte des Elternteils (bei der Wurzel: ihre Sandbox) tragen
    /// `Permission::ExecuteProcess` nicht — der Build startet einen Prozess.
    OutsideAuthority,
    /// Die Sitzung hat keine Job-Verwaltung; `agents.build` läuft nur als Job.
    NoJobManager,
}

impl AgentBuildWithheld {
    /// Eine Zeile für Log und Nutzer.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::NoExplicitDefinition => {
                "agents.build is not registered: built-in roles never receive it by default"
            }
            Self::NotAdmitted => {
                "agents.build is not registered: the agent definition does not admit it"
            }
            Self::OutsideAuthority => {
                "agents.build is not registered: the parent authority lacks ExecuteProcess"
            }
            Self::NoJobManager => {
                "agents.build is not registered: no job system in this session (agents.build only runs as a background job)"
            }
        }
    }
}

impl std::fmt::Display for AgentBuildWithheld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

/// Prüft die drei Bedingungen für `agents.build` in fester Reihenfolge.
///
/// # Arguments
/// - `admitted`/`forbidden`: `None` = keine ausdrückliche Definition;
///   sonst die Werkzeuglisten ihrer aufgelösten Oberfläche.
/// - `parent_permissions`: die Rechte des Elternteils.
/// - `has_job_manager`: ob die Sitzung eine Job-Verwaltung trägt.
fn agent_build_admission(
    surface: Option<(&[String], &[String])>,
    parent_permissions: &PermissionSet,
    has_job_manager: bool,
) -> Result<(), AgentBuildWithheld> {
    let Some((admitted, forbidden)) = surface else {
        return Err(AgentBuildWithheld::NoExplicitDefinition);
    };
    // Ausdrücklich: nur der wörtliche Eintrag zählt, kein Präfix und keine
    // `ALWAYS_AVAILABLE_TOOLS`-Ergänzung.
    let named = |list: &[String]| list.iter().any(|tool| tool == "agents.build");
    if !named(admitted) || named(forbidden) {
        return Err(AgentBuildWithheld::NotAdmitted);
    }
    if !parent_permissions.contains(harw_authority::Permission::ExecuteProcess) {
        return Err(AgentBuildWithheld::OutsideAuthority);
    }
    if !has_job_manager {
        return Err(AgentBuildWithheld::NoJobManager);
    }
    Ok(())
}

/// Baut den [`AgentBuildToolProvider`] für den aktiven Agenten — oder sagt,
/// warum nicht.
///
/// # Description
/// Registriert wird `agents.build` nur, wenn **alle** drei gelten:
/// 1. `definition` ist eine ausdrückliche Agentendefinition (`Some`) und
///    nennt `agents.build` wörtlich in `tools.admitted` (und nicht in
///    `tools.forbidden`). Eingebaute Rollen (`None`) bekommen es nie; ein
///    kompilierter Agent, dessen Manifest es nicht nennt, ebenso wenig.
/// 2. `parent_permissions` trägt `Permission::ExecuteProcess` (innerhalb der
///    Autorität des Elternteils; bei der Wurzel ist das ihre Sandbox).
/// 3. `manager` ist `Some` — ohne Job-Verwaltung läuft kein Build.
///
/// Die Freigabe ist davon unabhängig: `agents.build` steht nicht in
/// [`crate::AUTO_APPROVED_TOOLS`] und fragt deshalb wie `shell.exec`.
///
/// # Errors
/// [`AgentBuildWithheld`] mit dem ersten verletzten Kriterium.
pub fn agent_build_provider_for(
    definition: Option<&harw_agent_dsl::executable::ResolvedToolSurface>,
    parent_permissions: &PermissionSet,
    manager: Option<std::sync::Arc<harw_tool_job::JobManager>>,
) -> Result<AgentBuildToolProvider, AgentBuildWithheld> {
    agent_build_admission(
        definition.map(|surface| (surface.admitted(), surface.forbidden())),
        parent_permissions,
        manager.is_some(),
    )?;
    Ok(AgentBuildToolProvider::new(manager))
}

/// Abstraktion über [`harw_tool_job::JobManager::start`], austauschbar in
/// Tests (ein Fake zeichnet die Anfrage auf, ohne je einen Prozess zu
/// starten).
trait AgentBuildJobStarter: Send + Sync {
    /// Startet den vorbereiteten Build-Prozess als Job.
    fn start(
        &self,
        request: harw_tool_job::StartRequest,
        prepared: harw_tool_job::PreparedJob,
    ) -> Result<harw_tool_job::JobStatus, harw_tool_job::JobError>;
}

/// Startet über eine echte [`harw_tool_job::JobManager`]-Instanz.
struct RealJobStarter(std::sync::Arc<harw_tool_job::JobManager>);

impl AgentBuildJobStarter for RealJobStarter {
    fn start(
        &self,
        request: harw_tool_job::StartRequest,
        prepared: harw_tool_job::PreparedJob,
    ) -> Result<harw_tool_job::JobStatus, harw_tool_job::JobError> {
        self.0.start(request, prepared)
    }
}

/// Deserialisierte Argumente für `agents.build`.
#[derive(Debug, Deserialize)]
struct AgentsBuildArgs {
    /// Name einer bekannten Agentendefinition oder Pfad zu einer
    /// `definition.toml`.
    name_or_path: String,
    /// Zu bündelnde Schnittstellen (`cli`, `repl`, `mcp`, `http`, `tui`);
    /// `None` = die Vorgabe des Compilers (alle).
    #[serde(default)]
    interfaces: Option<Vec<String>>,
    /// `true` baut mit `--native` (dauert Minuten — läuft wie jeder
    /// `agents.build`-Aufruf ohnehin als Job).
    #[serde(default)]
    native: Option<bool>,
}

fn agents_build_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "name_or_path".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Name einer bekannten Agentendefinition oder Pfad zu einer definition.toml."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "interfaces".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Array),
            items: Some(Box::new(JsonSchema {
                schema_type: Some(JsonSchemaType::String),
                ..Default::default()
            })),
            description: Some(
                "Zu bündelnde Schnittstellen (cli, repl, mcp, http, tui); Vorgabe: alle."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "native".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Boolean),
            description: Some(
                "true baut mit --native (kann Minuten dauern; läuft immer als Job).".to_owned(),
            ),
            ..Default::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("agents.build"),
        description: "Kompiliert eine Agentendefinition zu einem eigenständigen Programm \
             (harw-agent-compiler, wie `/agent build`). Läuft immer als Hintergrund-Job; die \
             Antwort trägt die Job-Kennung, Fortschritt und Ergebnis stehen über job.status/ \
             job.logs. Freigabepflichtig wie shell.exec."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec!["name_or_path".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

/// Die Argument-Tokens für den Subprozess `harw agent build … --json`
/// (dieselbe Zuordnung wie `harw-ops`s `agent::start_build_job`).
fn agents_build_argv(args: &AgentsBuildArgs) -> Vec<String> {
    let mut argv = vec![
        "agent".to_owned(),
        "build".to_owned(),
        args.name_or_path.clone(),
    ];
    if let Some(interfaces) = args
        .interfaces
        .as_ref()
        .filter(|interfaces| !interfaces.is_empty())
    {
        argv.push("--interface".to_owned());
        argv.push(interfaces.join(","));
    }
    if args.native.unwrap_or(false) {
        argv.push("--native".to_owned());
    }
    argv.push("--json".to_owned());
    argv
}

struct AgentsBuildExecutor {
    starter: std::sync::Arc<dyn AgentBuildJobStarter>,
}

impl ToolExecutor for AgentsBuildExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let arguments = call.arguments.clone();
        Box::pin(async move {
            let args: AgentsBuildArgs = match parse_args("agents.build", &arguments) {
                Ok(args) => args,
                Err(out) => return Ok(out),
            };
            if args.name_or_path.trim().is_empty() {
                return Ok(ToolOutput::error(
                    "agents.build: name_or_path darf nicht leer sein".to_owned(),
                ));
            }
            if let Some(denied) = harw_tools::sandbox_guard::require_permission(
                context,
                harw_authority::Permission::ExecuteProcess,
                "agents.build",
            ) {
                return Ok(denied);
            }
            let exe = match std::env::current_exe() {
                Ok(exe) => exe,
                Err(error) => {
                    return Ok(ToolOutput::error(format!(
                        "agents.build: harw-Programm nicht auffindbar: {error}"
                    )));
                }
            };
            let cwd = context.sandbox().workspace().canonical_root().to_path_buf();
            let argv = agents_build_argv(&args);
            let display = format!("harw {}", argv.join(" "));
            let mut command = tokio::process::Command::new(&exe);
            command
                .args(&argv)
                .current_dir(&cwd)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(false);
            #[cfg(unix)]
            command.process_group(0);
            let request = harw_tool_job::StartRequest {
                name: format!("agents.build {}", args.name_or_path),
                command: display,
                cwd: Some(cwd),
                env_keys: Vec::new(),
                notify_every: std::time::Duration::from_secs(10),
                owner: harw_tool_job::JobOwner::new(context.session_id().as_str(), Vec::new()),
            };
            let prepared = harw_tool_job::PreparedJob {
                command,
                executed_on_host: true,
            };
            match self.starter.start(request, prepared) {
                Ok(status) => Ok(ToolOutput::json(serde_json::json!({
                    "job_id": status.meta.job_id.as_str(),
                    "note": "Build läuft als Hintergrund-Job. Fortschritt: job.status/job.logs.",
                }))),
                Err(error) => Ok(ToolOutput::error(format!("agents.build: {error}"))),
            }
        })
    }
}

/// Der Provider für `agents.build` (#22 Welle 2B).
///
/// # Description
/// Ohne [`harw_tool_job::JobManager`] (`manager = None`) bewirbt er nichts —
/// fail-closed, wie [`AgentDefinitionToolProvider`] ohne Urheber-Decke.
/// **Kein** eingebautes `RegistryProfile` konstruiert diesen Provider; er
/// steht nur einer expliziten Agentendefinition zur Verfügung, deren
/// `tools.admitted` `agents.build` nennt.
pub struct AgentBuildToolProvider {
    /// `None` unterdrückt die Registrierung vollständig.
    starter: Option<std::sync::Arc<dyn AgentBuildJobStarter>>,
}

impl AgentBuildToolProvider {
    /// Erstellt den Provider über der Job-Verwaltung der Sitzung.
    ///
    /// # Arguments
    /// - `manager` (`Option<Arc<JobManager>>`): `None` registriert
    ///   `agents.build` gar nicht (fail-closed ohne Job-Verwaltung).
    #[must_use]
    pub fn new(manager: Option<std::sync::Arc<harw_tool_job::JobManager>>) -> Self {
        Self {
            starter: manager.map(|manager| {
                std::sync::Arc::new(RealJobStarter(manager))
                    as std::sync::Arc<dyn AgentBuildJobStarter>
            }),
        }
    }
}

impl ToolProvider for AgentBuildToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        if self.starter.is_some() {
            vec![agents_build_spec()]
        } else {
            Vec::new()
        }
    }

    fn executor(&self, name: &ToolName) -> Option<std::sync::Arc<dyn ToolExecutor>> {
        match name.as_str() {
            "agents.build" => self.starter.clone().map(|starter| {
                std::sync::Arc::new(AgentsBuildExecutor { starter })
                    as std::sync::Arc<dyn ToolExecutor>
            }),
            _ => None,
        }
    }

    /// Startet jedes Mal einen neuen Prozess — nicht commutative.
    fn parallel_safe(&self, _name: &ToolName) -> bool {
        false
    }
}

#[cfg(test)]
mod agents_build_tests {
    use super::{
        AgentBuildJobStarter, AgentBuildToolProvider, AgentsBuildExecutor, agents_build_argv,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_extension_api::contributors::ToolProvider;
    use harw_extension_api::{ToolCall, ToolExecutionContext, ToolExecutor, ToolName, ToolOutput};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::{Arc, Mutex};

    fn sandbox(dir: &std::path::Path, permissions: Vec<Permission>) -> TestResult<SandboxSpec> {
        let ws = dir.join("project");
        std::fs::create_dir_all(&ws).map_err(ctx("Workspace anlegen"))?;
        let registry = WorkspaceRegistry::build(
            dir,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("project"),
                root: ws,
            }],
        )
        .map_err(ctx("Registry bauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("project"),
            )
            .map_err(ctx("Registry auflösen"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(permissions),
        ))
    }

    fn make_ctx(sandbox: SandboxSpec) -> ToolExecutionContext {
        ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox)
    }

    fn build_call(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: Default::default(),
            name: ToolName::new("agents.build"),
            arguments,
        }
    }

    /// Zeichnet jede Startanfrage auf und liefert eine feste Job-Kennung,
    /// ohne je einen echten Prozess zu starten (Bible R087/R165/R182: Tests
    /// dürfen nichts wirklich ausführen, was Minuten dauert oder das
    /// Testsystem verändert).
    struct FakeStarter {
        recorded: Mutex<Vec<harw_tool_job::StartRequest>>,
        job_id: &'static str,
    }

    impl FakeStarter {
        fn new(job_id: &'static str) -> Self {
            Self {
                recorded: Mutex::new(Vec::new()),
                job_id,
            }
        }

        fn recorded(&self) -> Vec<harw_tool_job::StartRequest> {
            self.recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl AgentBuildJobStarter for FakeStarter {
        fn start(
            &self,
            request: harw_tool_job::StartRequest,
            _prepared: harw_tool_job::PreparedJob,
        ) -> Result<harw_tool_job::JobStatus, harw_tool_job::JobError> {
            self.recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request.clone());
            let Some(job_id) = harw_tool_job::JobId::parse(self.job_id) else {
                return Err(harw_tool_job::JobError::Spawn(
                    "ungültige Test-Job-Kennung".to_owned(),
                ));
            };
            let meta = harw_tool_job::JobMeta {
                version: 1,
                job_id,
                name: request.name,
                command: request.command,
                cwd: request.cwd.map(|cwd| cwd.display().to_string()),
                env_keys: request.env_keys,
                state: harw_tool_job::JobState::Running,
                pid: None,
                proc_start_ticks: None,
                identity: None,
                executed_on_host: true,
                harw_instance: String::new(),
                owner: request.owner,
                created_at: jiff::Timestamp::UNIX_EPOCH,
                started_at: None,
                ended_at: None,
                exit_code: None,
                signal: None,
                stop_requested: false,
                detached: false,
                notify_every_secs: request.notify_every.as_secs(),
                progress: None,
                warnings: 0,
                errors: 0,
                launch_error: None,
                stragglers_reaped: false,
                log_truncated: false,
                launch_warnings: Vec::new(),
                origin_call_id: None,
                origin_tool: None,
                owner_agent: None,
            };
            Ok(harw_tool_job::JobStatus {
                meta,
                runtime_secs: None,
                stdout_lines: 0,
                stderr_lines: 0,
                last_lines: Vec::new(),
                log_dir: std::path::PathBuf::new(),
            })
        }
    }

    #[test]
    fn test_agents_build_argv_maps_interfaces_and_native() {
        let args = super::AgentsBuildArgs {
            name_or_path: "explorer".to_owned(),
            interfaces: Some(vec!["cli".to_owned(), "mcp".to_owned()]),
            native: Some(true),
        };
        assert_eq!(
            agents_build_argv(&args),
            vec![
                "agent",
                "build",
                "explorer",
                "--interface",
                "cli,mcp",
                "--native",
                "--json",
            ]
        );
    }

    #[test]
    fn test_agents_build_argv_omits_absent_optionals() {
        let args = super::AgentsBuildArgs {
            name_or_path: "worker".to_owned(),
            interfaces: None,
            native: None,
        };
        assert_eq!(
            agents_build_argv(&args),
            vec!["agent", "build", "worker", "--json"]
        );
    }

    /// Ohne Job-Verwaltung bewirbt der Provider `agents.build` nicht — kein
    /// Aufrufer kann es also je erreichen.
    #[test]
    fn test_provider_without_manager_registers_nothing() {
        let provider = AgentBuildToolProvider::new(None);
        assert!(provider.tools().is_empty());
        assert!(provider.executor(&ToolName::new("agents.build")).is_none());
    }

    /// Mit Job-Verwaltung bewirbt der Provider genau `agents.build`.
    #[test]
    fn test_provider_with_manager_registers_agents_build() {
        let provider = AgentBuildToolProvider {
            starter: Some(
                Arc::new(FakeStarter::new("job-test-001")) as Arc<dyn AgentBuildJobStarter>
            ),
        };
        let tools = provider.tools();
        assert_eq!(tools.len(), 1);
        assert!(provider.executor(&ToolName::new("agents.build")).is_some());
        assert!(
            provider
                .executor(&ToolName::new("agents.validate"))
                .is_none()
        );
    }

    /// Ohne `Permission::ExecuteProcess` lehnt der Executor ab, bevor er
    /// überhaupt einen Job anfragt.
    #[tokio::test]
    async fn test_execute_without_process_permission_is_denied() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegen"))?;
        let sandbox = sandbox(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let starter = Arc::new(FakeStarter::new("job-test-002"));
        let executor = AgentsBuildExecutor {
            starter: starter.clone(),
        };
        let call = build_call(serde_json::json!({ "name_or_path": "explorer" }));
        let output = executor
            .execute(&ctx, &call)
            .await
            .map_err(|error| TestError::Unexpected(format!("{error:?}")))?;
        match output {
            ToolOutput::Error { .. } => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected a permission error, got {other:?}"
                )));
            }
        }
        assert!(
            starter.recorded().is_empty(),
            "eine abgelehnte Ausführung darf keinen Job anfragen"
        );
        Ok(())
    }

    /// Mit `Permission::ExecuteProcess` startet der Executor einen Job und
    /// gibt dessen Kennung zurück, ohne je einen echten Prozess zu starten
    /// (der Fake spawnt nicht).
    #[tokio::test]
    async fn test_execute_with_process_permission_returns_job_id() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegen"))?;
        let sandbox = sandbox(dir.path(), vec![Permission::ExecuteProcess])?;
        let ctx = make_ctx(sandbox);
        let starter = Arc::new(FakeStarter::new("job-test-003"));
        let executor = AgentsBuildExecutor {
            starter: starter.clone(),
        };
        let call = build_call(serde_json::json!({
            "name_or_path": "explorer",
            "native": true,
        }));
        let output = executor
            .execute(&ctx, &call)
            .await
            .map_err(|error| TestError::Unexpected(format!("{error:?}")))?;
        match output {
            ToolOutput::Json { content } => {
                assert_eq!(content["job_id"], serde_json::json!("job-test-003"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected a json job id, got {other:?}"
                )));
            }
        }
        let recorded = starter.recorded();
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].command.contains("--native"));
        assert!(recorded[0].command.contains("explorer"));
        Ok(())
    }

    /// Ein leerer `name_or_path` lehnt ab, ohne die Berechtigung zu prüfen
    /// oder einen Job anzufragen.
    #[tokio::test]
    async fn test_execute_rejects_empty_name_or_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegen"))?;
        let sandbox = sandbox(dir.path(), vec![Permission::ExecuteProcess])?;
        let ctx = make_ctx(sandbox);
        let starter = Arc::new(FakeStarter::new("job-test-004"));
        let executor = AgentsBuildExecutor {
            starter: starter.clone(),
        };
        let call = build_call(serde_json::json!({ "name_or_path": "   " }));
        let output = executor
            .execute(&ctx, &call)
            .await
            .map_err(|error| TestError::Unexpected(format!("{error:?}")))?;
        match output {
            ToolOutput::Error { message } => assert!(message.contains("name_or_path")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected an error output, got {other:?}"
                )));
            }
        }
        assert!(starter.recorded().is_empty());
        Ok(())
    }

    /// Eine Agentendefinition, die `agents.build` ausdrücklich admittiert.
    const BUILDER_WORKER: &str = r#"
schema = "harwness.agent/v1"
id = "user.agent.builder@1"
version = "1.0.0"
extends = { id = "harwness.agent.worker-base@1" }
role = "worker"
specialization = "builder"

[tools]
admitted = ["fs.read", "agents.build"]
"#;

    fn job_manager(dir: &std::path::Path) -> TestResult<Arc<harw_tool_job::JobManager>> {
        harw_tool_job::JobManager::new(
            harw_tool_job::JobManagerConfig::new(dir),
            Arc::new(harw_tool_job::NoopNotifier),
        )
        .map_err(ctx("Job-Verwaltung anlegen"))
    }

    fn builder_ir() -> TestResult<harw_agent_dsl::ExecutableAgentIr> {
        match super::validate_definition_toml(BUILDER_WORKER) {
            super::Validated::Ok { ir, .. } => Ok(*ir),
            super::Validated::Err(errors) => Err(TestError::Unexpected(format!(
                "Builder-Definition muss lowern: {errors:?}"
            ))),
        }
    }

    /// Definition admittiert `agents.build`, die Rechte tragen
    /// `ExecuteProcess` und eine Job-Verwaltung existiert → registriert.
    #[test]
    fn test_definition_admitting_agents_build_gets_the_tool_with_a_job_manager() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegen"))?;
        let ir = builder_ir()?;
        let provider = super::agent_build_provider_for(
            Some(ir.tool_surface()),
            &PermissionSet::from_policy([Permission::ExecuteProcess]),
            Some(job_manager(dir.path())?),
        )
        .map_err(|withheld| TestError::Unexpected(withheld.to_string()))?;
        let names: Vec<String> = provider
            .tools()
            .iter()
            .map(|spec| spec.name().to_owned())
            .collect();
        assert_eq!(names, vec!["agents.build".to_owned()]);
        assert!(provider.executor(&ToolName::new("agents.build")).is_some());
        Ok(())
    }

    /// Ohne Job-Verwaltung wird `agents.build` nicht registriert — mit einem
    /// klaren Grund.
    #[test]
    fn test_without_job_manager_agents_build_is_withheld_with_a_reason() -> TestResult {
        let ir = builder_ir()?;
        let withheld = match super::agent_build_provider_for(
            Some(ir.tool_surface()),
            &PermissionSet::from_policy([Permission::ExecuteProcess]),
            None,
        ) {
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "ohne Job-Verwaltung darf agents.build nicht registriert werden".to_owned(),
                ));
            }
            Err(withheld) => withheld,
        };
        assert_eq!(withheld, super::AgentBuildWithheld::NoJobManager);
        assert!(withheld.reason().contains("no job system"));
        Ok(())
    }

    /// Außerhalb der Autorität des Elternteils (kein `ExecuteProcess`) wird
    /// `agents.build` auch mit Job-Verwaltung nicht registriert.
    #[test]
    fn test_agents_build_is_withheld_outside_the_parent_authority() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegen"))?;
        let ir = builder_ir()?;
        let result = super::agent_build_provider_for(
            Some(ir.tool_surface()),
            &PermissionSet::from_policy([Permission::ReadWorkspace]),
            Some(job_manager(dir.path())?),
        );
        assert!(matches!(
            result,
            Err(super::AgentBuildWithheld::OutsideAuthority)
        ));
        Ok(())
    }

    /// Keine eingebaute Rolle bekommt `agents.build` — weder ohne
    /// Definition (`None`) noch über ihre eingebaute Werkzeugoberfläche,
    /// selbst mit vollen Rechten und Job-Verwaltung.
    #[test]
    fn test_builtin_roles_never_get_agents_build() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegen"))?;
        let manager = job_manager(dir.path())?;
        let permissions = PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
        ]);
        assert!(matches!(
            super::agent_build_provider_for(None, &permissions, Some(Arc::clone(&manager))),
            Err(super::AgentBuildWithheld::NoExplicitDefinition)
        ));
        let builtin =
            crate::embedded_agents::builtin_agent_definitions(&std::collections::HashMap::new())
                .map_err(ctx("eingebaute Definitionen"))?;
        for (role, ir) in &builtin {
            let result = super::agent_build_provider_for(
                Some(ir.tool_surface()),
                &permissions,
                Some(Arc::clone(&manager)),
            );
            assert!(
                matches!(result, Err(super::AgentBuildWithheld::NotAdmitted)),
                "{role} darf agents.build nicht bekommen"
            );
        }
        Ok(())
    }

    /// Ein `agents.build`-Aufruf erzeugt eine Freigabe-Anfrage wie
    /// `shell.exec` — unter `ask` und `auto` (ohne Regel).
    #[tokio::test]
    async fn test_agents_build_call_produces_an_approval_request() -> TestResult {
        use harw_extension_api::approval_mode::ApprovalModeCell;
        use harw_extension_api::{ApprovalDecision, ApprovalHandler, ApprovalMode};

        let call = build_call(serde_json::json!({ "name_or_path": "builder" }));
        for mode in [ApprovalMode::AlwaysAsk, ApprovalMode::Delegated] {
            let policy = crate::DefaultApprovalPolicy::new(ApprovalModeCell::new(mode));
            let decision = policy.review(&call).await;
            assert!(
                matches!(decision, ApprovalDecision::AskUser(_)),
                "agents.build muss unter {mode:?} nachfragen, war {decision:?}"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::Permission;

    fn empty_ceiling(role: AgentRoleId) -> DefinitionAuthorCeiling {
        DefinitionAuthorCeiling {
            role,
            tools: BTreeSet::new(),
            permissions: PermissionSet::empty(),
            max_depth: 0,
            budget_tokens: 0,
            effort_cap: None,
        }
    }

    const COMMITTED_WORKER: &str = r#"
schema = "harwness.agent/v1"
id = "user.agent.note-taker@1"
version = "1.0.0"
extends = { id = "harwness.agent.worker-base@1" }
role = "worker"
specialization = "note-taker"

[tools]
admitted = ["fs.read"]
"#;

    #[test]
    fn test_commit_definition_writes_the_directory_layout_discovery_reads() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let agents = home.path().join("agents");
        let path = commit_definition(
            &agents,
            "note-taker",
            COMMITTED_WORKER,
            "user.agent.note-taker@1",
        )
        .map_err(TestError::Unexpected)?;
        assert_eq!(path, agents.join("note-taker").join("definition.toml"));

        let config = harw_config::discover_config(&[home.path().to_path_buf()])
            .map_err(ctx("discovery muss die committete Definition lesen"))?;
        let config = crate::config_agents::ConfigAgents::from_config(&config)
            .map_err(ctx("die committete Definition muss senken"))?;
        let ir = config
            .executable_agents
            .get("user.agent.note-taker@1")
            .ok_or(TestError::Missing(
                "committete Definition fehlt in der Discovery",
            ))?;
        assert_eq!(ir.specialization(), "note-taker");
        Ok(())
    }

    #[test]
    fn test_commit_definition_replaces_a_legacy_flat_file_with_the_same_id() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let agents = home.path().join("agents");
        std::fs::create_dir_all(&agents).map_err(ctx("agents"))?;
        let legacy = agents.join("note-taker.toml");
        std::fs::write(&legacy, COMMITTED_WORKER).map_err(ctx("Altdatei"))?;
        assert_eq!(existing_definition_path(&agents, "note-taker"), legacy);

        commit_definition(
            &agents,
            "note-taker",
            COMMITTED_WORKER,
            "user.agent.note-taker@1",
        )
        .map_err(TestError::Unexpected)?;
        assert!(!legacy.exists(), "die flache Altdatei wird entfernt");
        assert_eq!(
            existing_definition_path(&agents, "note-taker"),
            agents.join("note-taker").join("definition.toml")
        );

        // Eine Altdatei mit fremder ID wird nie überschrieben.
        std::fs::write(
            agents.join("other.toml"),
            COMMITTED_WORKER.replace("note-taker@1", "other@1"),
        )
        .map_err(ctx("fremde Altdatei"))?;
        assert!(
            commit_definition(
                &agents,
                "other",
                COMMITTED_WORKER,
                "user.agent.note-taker@1"
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn test_is_valid_slug_rejects_path_separators_and_empty() {
        assert!(is_valid_slug("agent-steward"));
        assert!(is_valid_slug("uia-01"));
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("../escape"));
        assert!(!is_valid_slug("a/b"));
        assert!(!is_valid_slug("Has-Upper"));
        assert!(!is_valid_slug("has space"));
    }

    #[test]
    fn test_generate_proposal_id_is_a_valid_slug() {
        let id = generate_proposal_id();
        assert!(is_valid_slug(&id), "proposal id must be a valid slug: {id}");
    }

    #[test]
    fn test_validate_definition_toml_rejects_unparsable_source() -> TestResult {
        match validate_definition_toml("not = [valid") {
            Validated::Err(errors) => assert!(!errors.is_empty()),
            Validated::Ok { .. } => {
                return Err(TestError::Unexpected(
                    "expected a validation error".to_owned(),
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn test_compute_rights_delta_flags_added_tool_and_higher_budget() {
        let claimed = DefinitionAuthorCeiling {
            role: AgentRoleId::Worker,
            tools: BTreeSet::from(["fs.write".to_owned(), "shell.exec".to_owned()]),
            permissions: PermissionSet::from_policy([
                Permission::WriteWorkspace,
                Permission::ExecuteProcess,
            ]),
            max_depth: 2,
            budget_tokens: 5_000,
            effort_cap: Some("high".to_owned()),
        };
        let limit = DefinitionAuthorCeiling {
            role: AgentRoleId::Worker,
            tools: BTreeSet::from(["fs.write".to_owned()]),
            permissions: PermissionSet::from_policy([Permission::WriteWorkspace]),
            max_depth: 1,
            budget_tokens: 1_000,
            effort_cap: Some("low".to_owned()),
        };
        let delta = compute_rights_delta(&claimed, &limit);
        assert_eq!(delta.added_tools, vec!["shell.exec".to_owned()]);
        assert_eq!(delta.added_permissions, vec!["ExecuteProcess".to_owned()]);
        assert_eq!(delta.depth_increase, Some(1));
        assert_eq!(delta.budget_increase, Some(4_000));
        assert_eq!(delta.effort_increase, Some("high".to_owned()));
        assert!(!delta.is_empty());
    }

    #[test]
    fn test_compute_rights_delta_is_empty_for_equal_or_narrower_claim() {
        let ceiling = DefinitionAuthorCeiling {
            role: AgentRoleId::Worker,
            tools: BTreeSet::from(["fs.read".to_owned()]),
            permissions: PermissionSet::from_policy([Permission::ReadWorkspace]),
            max_depth: 3,
            budget_tokens: 10_000,
            effort_cap: Some("medium".to_owned()),
        };
        let delta = compute_rights_delta(&ceiling, &ceiling);
        assert!(
            delta.is_empty(),
            "identical claim and ceiling must yield an empty delta"
        );
    }

    #[test]
    fn test_agents_write_definition_rejects_author_elevation_without_touching_disk() -> TestResult {
        let ceiling = empty_ceiling(AgentRoleId::UserInterface);
        let executor = AgentsWriteDefinitionExecutor {
            project_agents_dir: None,
            profile_agents_dir: None,
            ceiling,
        };
        let toml = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.elevated-worker@1"
version = "1.0.0"
extends = { id = "harwness.agent.worker-base@1" }
role = "worker"
specialization = "elevated-worker"

[tools]
admitted = ["fs.write"]

[spawn]
max_depth = 0

[return]
contract = "harwness.return.research-finding@1"
"#;
        let call = ToolCall {
            id: Default::default(),
            name: ToolName::new("agents.write_definition"),
            arguments: serde_json::json!({
                "scope": "profile",
                "name": "elevated-worker",
                "toml": toml,
            }),
        };
        let output = executor.write(&call);
        match output {
            ToolOutput::Json { content } => {
                assert_eq!(content["ok"], serde_json::json!(false));
                assert_eq!(content["written"], serde_json::json!(false));
                let errors = content["errors"]
                    .as_array()
                    .ok_or(TestError::Missing("errors ist ein Array"))?;
                assert!(
                    errors[0]
                        .as_str()
                        .ok_or(TestError::Missing("errors[0] ist ein String"))?
                        .contains("authority elevation")
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected a json error output, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_agents_write_definition_rejects_invalid_slug_without_touching_disk() -> TestResult {
        let ceiling = empty_ceiling(AgentRoleId::RootOrchestrator);
        let executor = AgentsWriteDefinitionExecutor {
            project_agents_dir: None,
            profile_agents_dir: None,
            ceiling,
        };
        let call = ToolCall {
            id: Default::default(),
            name: ToolName::new("agents.write_definition"),
            arguments: serde_json::json!({
                "scope": "project",
                "name": "../escape",
                "toml": "irrelevant",
            }),
        };
        let output = executor.write(&call);
        match output {
            ToolOutput::Error { message } => assert!(message.contains("gültiger Name")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected an error output, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_agents_write_definition_run_scope_requires_run_id() -> TestResult {
        let ceiling = empty_ceiling(AgentRoleId::RootOrchestrator);
        let executor = AgentsWriteDefinitionExecutor {
            project_agents_dir: None,
            profile_agents_dir: None,
            ceiling,
        };
        let call = ToolCall {
            id: Default::default(),
            name: ToolName::new("agents.write_definition"),
            arguments: serde_json::json!({
                "scope": "run",
                "name": "task-agent",
                "toml": "irrelevant",
            }),
        };
        let output = executor.write(&call);
        match output {
            ToolOutput::Error { message } => assert!(message.contains("run_id")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected an error output, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_review_level_for_uia_is_always_user_required() {
        let empty_delta = RightsDelta::default();
        assert_eq!(review_level_for("uia", &empty_delta), "user_required");
    }

    #[test]
    fn test_review_level_for_definition_with_base_role_delta_is_user_required() {
        let mut delta = RightsDelta::default();
        delta.added_tools.push("shell.exec".to_owned());
        assert_eq!(review_level_for("definition", &delta), "user_required");
    }

    #[test]
    fn test_review_level_for_definition_without_base_role_delta_is_uia() {
        let empty_delta = RightsDelta::default();
        assert_eq!(review_level_for("definition", &empty_delta), "uia");
    }

    #[test]
    fn test_is_expired_detects_past_timestamp() {
        let past = format_rfc3339(now() - time::Duration::days(1));
        let future = format_rfc3339(now() + time::Duration::days(1));
        assert!(is_expired(&past));
        assert!(!is_expired(&future));
    }

    #[test]
    fn test_build_agent_toml_no_longer_embeds_identity_field() -> TestResult {
        let raw = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.test-uia@1"
version = "1.0.0"
role = "user-interface"
specialization = "test-uia"
name = "Test UIA"
description = "eine Test-UIA"

[tools]
admitted = []

[return]
contract = "harwness.return.research-finding@1"
"#,
        )
        .map_err(ctx("parse fixture definition"))?;
        let agent_toml = build_agent_toml(&raw);
        assert!(!agent_toml.contains("identity"));
        assert!(agent_toml.contains("role = \"user-interface\""));
        Ok(())
    }

    #[test]
    fn test_commit_uia_bundle_writes_identity_md_when_present() -> TestResult {
        let directory = std::env::temp_dir().join(format!(
            "harw-agent-def-tools-commit-with-identity-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        commit_uia_bundle(
            &directory,
            "harwness.agent.test-uia@1",
            "id = \"harwness.agent.test-uia@1\"",
            "name = \"Test\"",
            Some("Ich bin Test."),
            "warm",
            "# Nutzerkontext",
        )
        .map_err(ctx("commit uia bundle"))?;
        assert_eq!(
            std::fs::read_to_string(directory.join("identity.md"))
                .map_err(ctx("identity.md lesen"))?,
            "Ich bin Test."
        );
        std::fs::remove_dir_all(&directory).map_err(ctx("Verzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn test_commit_uia_bundle_omits_identity_md_when_absent() -> TestResult {
        let directory = std::env::temp_dir().join(format!(
            "harw-agent-def-tools-commit-without-identity-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        commit_uia_bundle(
            &directory,
            "harwness.agent.test-uia@1",
            "id = \"harwness.agent.test-uia@1\"",
            "name = \"Test\"",
            None,
            "warm",
            "# Nutzerkontext",
        )
        .map_err(ctx("commit uia bundle"))?;
        assert!(!directory.join("identity.md").exists());
        std::fs::remove_dir_all(&directory).map_err(ctx("Verzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn test_update_document_rejects_non_uia_role() {
        let provider = UiaSelfDocumentToolProvider::new(PathBuf::from("/tmp"), AgentRoleId::Worker);
        assert!(provider.tools().is_empty());
        assert!(
            provider
                .executor(&ToolName::new("uia_self.update_document"))
                .is_none()
        );
    }

    #[test]
    fn test_update_document_rejects_secret_pattern() -> TestResult {
        let executor = UiaSelfUpdateDocumentExecutor {
            agent_dir: std::env::temp_dir(),
        };
        let call = ToolCall {
            id: Default::default(),
            name: ToolName::new("uia_self.update_document"),
            arguments: serde_json::json!({
                "target": "identity",
                "content": "sk-does-not-belong-here",
                "reason": "test",
            }),
        };
        let output = executor.write(&call);
        match output {
            ToolOutput::Json { content } => {
                assert_eq!(content["ok"], serde_json::json!(false));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected a json error output, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_update_document_writes_identity_target() -> TestResult {
        let directory = std::env::temp_dir().join(format!(
            "harw-agent-def-tools-self-doc-identity-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).map_err(ctx("Verzeichnis anlegen"))?;
        let executor = UiaSelfUpdateDocumentExecutor {
            agent_dir: directory.clone(),
        };
        let call = ToolCall {
            id: Default::default(),
            name: ToolName::new("uia_self.update_document"),
            arguments: serde_json::json!({
                "target": "identity",
                "content": "Ich bin Assistant.",
                "reason": "Erstpflege",
            }),
        };
        let output = executor.write(&call);
        match output {
            ToolOutput::Json { content } => assert_eq!(content["ok"], serde_json::json!(true)),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected a json success output, got {other:?}"
                )));
            }
        }
        assert_eq!(
            std::fs::read_to_string(directory.join("identity.md"))
                .map_err(ctx("identity.md lesen"))?,
            "Ich bin Assistant."
        );
        std::fs::remove_dir_all(&directory).map_err(ctx("Verzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn test_update_document_writes_user_target() -> TestResult {
        let directory = std::env::temp_dir().join(format!(
            "harw-agent-def-tools-self-doc-user-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).map_err(ctx("Verzeichnis anlegen"))?;
        let executor = UiaSelfUpdateDocumentExecutor {
            agent_dir: directory.clone(),
        };
        let call = ToolCall {
            id: Default::default(),
            name: ToolName::new("uia_self.update_document"),
            arguments: serde_json::json!({
                "target": "user",
                "content": "Name: Alice",
                "reason": "Nutzer hat sich vorgestellt",
            }),
        };
        let output = executor.write(&call);
        assert_eq!(
            std::fs::read_to_string(directory.join("USER.md")).map_err(ctx("USER.md lesen"))?,
            "Name: Alice"
        );
        assert!(matches!(output, ToolOutput::Json { .. }));
        std::fs::remove_dir_all(&directory).map_err(ctx("Verzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn test_update_document_writes_personality_target() -> TestResult {
        let directory = std::env::temp_dir().join(format!(
            "harw-agent-def-tools-self-doc-personality-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).map_err(ctx("Verzeichnis anlegen"))?;
        let executor = UiaSelfUpdateDocumentExecutor {
            agent_dir: directory.clone(),
        };
        let call = ToolCall {
            id: Default::default(),
            name: ToolName::new("uia_self.update_document"),
            arguments: serde_json::json!({
                "target": "personality",
                "content": "warm, knapp",
                "reason": "Ton geschärft",
            }),
        };
        let output = executor.write(&call);
        assert_eq!(
            std::fs::read_to_string(directory.join("Personality.md"))
                .map_err(ctx("Personality.md lesen"))?,
            "warm, knapp"
        );
        assert!(matches!(output, ToolOutput::Json { .. }));
        std::fs::remove_dir_all(&directory).map_err(ctx("Verzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn test_update_document_never_escapes_agent_dir() -> TestResult {
        let directory = std::env::temp_dir().join(format!(
            "harw-agent-def-tools-self-doc-scope-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).map_err(ctx("Verzeichnis anlegen"))?;
        let executor = UiaSelfUpdateDocumentExecutor {
            agent_dir: directory.clone(),
        };
        // `target` ist ein geschlossenes Enum ohne freien Pfad-Parameter —
        // selbst ein Traversal-artiger Wert wird als unbekanntes target
        // abgelehnt, nie als Pfadfragment interpretiert.
        let call = ToolCall {
            id: Default::default(),
            name: ToolName::new("uia_self.update_document"),
            arguments: serde_json::json!({
                "target": "../escape",
                "content": "x",
                "reason": "x",
            }),
        };
        let output = executor.write(&call);
        match output {
            ToolOutput::Error { message } => assert!(message.contains("unbekanntes target")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected an error output, got {other:?}"
                )));
            }
        }
        let parent = directory
            .parent()
            .ok_or(TestError::Missing("directory hat ein Elternverzeichnis"))?;
        assert!(!parent.join("escape").exists());
        std::fs::remove_dir_all(&directory).map_err(ctx("Verzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn test_update_document_not_in_auto_approved_tools() {
        assert!(
            !crate::AUTO_APPROVED_TOOLS.contains(&"uia_self.update_document"),
            "uia_self.update_document must stay fail-closed and never be auto-approved"
        );
    }

    #[test]
    fn test_provider_lists_tools_by_ceiling_and_mode() {
        let no_ceiling =
            AgentDefinitionToolProvider::new(None, None, DefinitionWriteMode::Commit, None);
        let no_ceiling_tools = no_ceiling.tools();
        let names: Vec<&str> = no_ceiling_tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["agents.validate", "agents.list_proposals"]);
        assert!(
            no_ceiling
                .executor(&ToolName::new("agents.write_definition"))
                .is_none()
        );
        assert!(
            no_ceiling
                .executor(&ToolName::new("agents.commit_proposal"))
                .is_none()
        );

        let ceiling = Some(empty_ceiling(AgentRoleId::UserInterface));
        let commit = AgentDefinitionToolProvider::new(
            None,
            None,
            DefinitionWriteMode::Commit,
            ceiling.clone(),
        );
        let commit_tools = commit.tools();
        let commit_names: Vec<&str> = commit_tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            commit_names,
            vec![
                "agents.validate",
                "agents.list_proposals",
                "agents.write_definition",
                "agents.write_uia",
                "agents.commit_proposal",
                "agents.reject_proposal",
            ]
        );
        for name in &commit_names {
            assert!(commit.executor(&ToolName::new(*name)).is_some());
        }

        let proposal_only = AgentDefinitionToolProvider::new(
            None,
            None,
            DefinitionWriteMode::ProposalOnly,
            ceiling,
        );
        let proposal_tools = proposal_only.tools();
        let proposal_names: Vec<&str> = proposal_tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            proposal_names,
            vec![
                "agents.validate",
                "agents.list_proposals",
                "agents.write_definition",
                "agents.write_uia",
            ]
        );
        assert!(
            proposal_only
                .executor(&ToolName::new("agents.commit_proposal"))
                .is_none()
        );
        assert!(
            proposal_only
                .executor(&ToolName::new("agents.reject_proposal"))
                .is_none()
        );
    }
}
