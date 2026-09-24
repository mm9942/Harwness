//! Organisations-Typen und -Auflösung für Harwness Root Organization Templates (§15, §22 DSL-Spec).
//!
//! Dieses Modul definiert die rohen TOML-nahen Strukturen für Organisations-Definitionen
//! (`RawOrganizationDefinition`) sowie die aufgelöste Intermediate-Representation
//! (`ResolvedOrganization`). Es implementiert `resolve_organization`, welche eine
//! Layer-Kaskade (§4) durchläuft und strukturelle Invarianten gemäß §22 prüft.
//!
//! # Invarianten geprüft bei der Auflösung (§22)
//! - Invariante 10: Clans sind run-local Subtrees.
//! - Invariante 11: Cells sind temporäre Fan-out-Batches.
//! - Invariante 12: Organizations erzeugen keine zusätzlichen Roots.
//! - Duplikat `clan.id` → `DslError::Parse`.
//! - `cell.clan` muss auf existierende `clan.id` verweisen → `DslError::MissingBase`.
//! - `extends`-Referenz muss in den Layers aufzufinden sein → `DslError::MissingBase`.
//! - Patch-Operationen: `patch.clans.append`, `patch.cells.append`, `patch.name.replace`.
//!
//! # Schlüsseltypen
//! - [`RawOrganizationDefinition`] — TOML-nahe Eingabe
//! - [`RawRootSpec`] — Root-Orchestrator + Family-Binding
//! - [`RawClanSpec`] — run-lokaler Subtree
//! - [`RawCellSpec`] — temporärer Fan-out-Batch
//! - [`CellKind`], [`CellBarrier`], [`CellWritePartition`] — Enum-Konfiguration der Cell
//! - [`ResolvedOrganization`] — Aufgelöste IR
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync` und klonierbar. `resolve_organization` ist rein
//! (keine Seiteneffekte, keine Locks, keine Threads).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{DslError, DslResult};
use crate::ids::{DefinitionId, DefinitionRef};
use crate::layers::DefinitionLayer;
use crate::resolved::{ResolutionStep, ResolutionTrace};

// ─── Rohe Strukturen ────────────────────────────────────────────────────────

/// Rohe TOML-nahe Organization-Definition (Root-Level).
///
/// # Beschreibung
/// Entspricht direkt der TOML-Syntax aus §15 der DSL-Spezifikation.
/// Das `patch`-Feld enthält optionale Überschreibungen, die während der
/// Auflösung angewendet werden.
///
/// # Felder
/// - `schema` (`String`): Schema-Kennung, z. B. `"harwness.organization/v1"`.
/// - `id` (`DefinitionId`): Eindeutige Namensraum-ID dieser Organisation.
/// - `version` (`Version`): Semantische Vollversion.
/// - `extends` (`Option<DefinitionRef>`): Optionale Basisdefinition.
/// - `name` (`String`): Menschenlesbarer Name.
/// - `root` (`RawRootSpec`): Root-Orchestrator und Family-Binding.
/// - `clans` (`Vec<RawClanSpec>`): Liste der run-lokalen Clans.
/// - `cells` (`Vec<RawCellSpec>`): Liste der temporären Cell-Definitionen.
/// - `patch` (`toml::Table`): Explizite Merge-Operationen (§7).
///
/// # Nebenläufigkeit
/// `Send + Sync`.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::organization::RawOrganizationDefinition;
///
/// let toml_str = r#"
/// schema = "harwness.organization/v1"
/// id = "harwness.organization.software-project@1"
/// version = "1.0.0"
/// name = "Software Project Organization"
///
/// [root]
/// agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
/// family = { id = "harwness.family.focused-coding@1" }
/// "#;
/// let raw: RawOrganizationDefinition = toml::from_str(toml_str).unwrap();
/// assert_eq!(raw.name, "Software Project Organization");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawOrganizationDefinition {
    /// Schema-Kennung, z. B. `"harwness.organization/v1"`.
    pub schema: String,
    /// Eindeutige Namensraum-ID dieser Organisation (§5).
    pub id: DefinitionId,
    /// Semantische Vollversion (semver).
    pub version: crate::ids::Version,
    /// Optionale Basisdefinition via `extends` (§6).
    #[serde(default)]
    pub extends: Option<DefinitionRef>,
    /// Menschenlesbarer Name der Organisation.
    pub name: String,
    /// Root-Orchestrator und Family-Binding (§15).
    pub root: RawRootSpec,
    /// Run-lokale Clan-Subtrees (§22, Invariante 10).
    #[serde(default)]
    pub clans: Vec<RawClanSpec>,
    /// Temporäre Fan-out-Batches (§22, Invariante 11).
    #[serde(default)]
    pub cells: Vec<RawCellSpec>,
    /// Explizite Merge-Operationen (§7).
    #[serde(default)]
    pub patch: toml::Table,
}

/// Root-Level Orchestrator + Family-Binding (§15).
///
/// # Beschreibung
/// Verweist auf den Root-Orchestrator-Agenten und die zugehörige Family.
/// Gemäß §22 Invariante 12 darf die Organisation keinen weiteren Root deklarieren;
/// `root.agent` ist der einzige zugelassene Root-Einstiegspunkt.
///
/// # Felder
/// - `agent` (`DefinitionRef`): Verweis auf den Root-Orchestrator.
/// - `family` (`DefinitionRef`): Verweis auf die Family-Definition.
///
/// # Nebenläufigkeit
/// `Send + Sync`.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::organization::RawRootSpec;
///
/// let toml_str = r#"
/// agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
/// family = { id = "harwness.family.focused-coding@1" }
/// "#;
/// let spec: RawRootSpec = toml::from_str(toml_str).unwrap();
/// assert_eq!(spec.agent.id.kind, "agent");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRootSpec {
    /// Verweis auf den Root-Orchestrator-Agenten.
    pub agent: DefinitionRef,
    /// Verweis auf die Family-Definition.
    pub family: DefinitionRef,
}

/// Ein Clan — run-lokaler Organisationssubtree (§22, Invariante 10).
///
/// # Beschreibung
/// Clans strukturieren den Arbeitsbaum innerhalb eines Root-Runs. Sie sind
/// nicht über Runs hinweg persistent und besitzen einen Leader-Orchestrator
/// sowie einen Family-Scope.
///
/// # Felder
/// - `id` (`String`): Eindeutige ID innerhalb dieser Organisation (z. B. `"research"`).
/// - `name` (`String`): Menschenlesbarer Name.
/// - `leader` (`DefinitionRef`): Verweis auf den Leader-Orchestrator (muss ChildOrchestrator sein).
/// - `family` (`DefinitionRef`): Zugehörige Family-Definition.
/// - `plan_scope` (`String`): Plan-Scope-Glob, z. B. `"research/*"`.
/// - `child_depth_cost` (`u32`): Kosten pro Kindtiefe (Standard: 1).
///
/// # Nebenläufigkeit
/// `Send + Sync`.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::organization::RawClanSpec;
///
/// let toml_str = r#"
/// id = "research"
/// name = "Research Clan"
/// leader = { id = "harwness.agent.focused-coding-orchestrator@1" }
/// family = { id = "harwness.family.focused-coding@1" }
/// plan_scope = "research/*"
/// "#;
/// let clan: RawClanSpec = toml::from_str(toml_str).unwrap();
/// assert_eq!(clan.child_depth_cost, 1);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawClanSpec {
    /// Eindeutige Clan-ID innerhalb dieser Organisation.
    pub id: String,
    /// Menschenlesbarer Name des Clans.
    pub name: String,
    /// Verweis auf den Leader-Orchestrator (muss ChildOrchestrator-Rolle haben).
    pub leader: DefinitionRef,
    /// Zugehörige Family-Definition.
    pub family: DefinitionRef,
    /// Plan-Scope-Glob, z. B. `"implementation/*"`.
    pub plan_scope: String,
    /// Kosten pro Kindtiefe im Spawn-Baum (Standard: 1).
    #[serde(default = "default_depth_cost")]
    pub child_depth_cost: u32,
}

/// Standardwert für `child_depth_cost` (§15).
///
/// # Rückgabe
/// Gibt `1` zurück — den Standard-Tiefenkostenkoeffizienten.
fn default_depth_cost() -> u32 {
    1
}

impl RawClanSpec {
    /// Tiefe eines Kindes, das ein Knoten dieses Clans auf `parent_depth`
    /// startet (§15 `child_depth_cost`).
    ///
    /// # Beschreibung
    /// Jede Kindebene innerhalb eines Clans kostet `child_depth_cost`
    /// Tiefeneinheiten statt einer. Ein Clan mit Kosten `2` verbraucht damit
    /// die geerbte Tiefendecke (`ChildRecord::depth_ceiling` im
    /// Kind-Controller) doppelt so schnell — so begrenzt eine Organisation,
    /// wie tief ein einzelner Clan seinen Teilbaum wachsen lassen darf, ohne
    /// die globale Decke anzufassen. Die Kosten senken die erreichbare Tiefe
    /// nur, sie heben sie nie an: `resolve_organization` weist
    /// `child_depth_cost = 0` ab.
    ///
    /// # Argumente
    /// - `parent_depth` (`u32`): Tiefe des startenden Knotens.
    ///
    /// # Rückgabe
    /// `Some(parent_depth + child_depth_cost)`; `None` bei Überlauf — ein
    /// Aufrufer behandelt das fail-closed als „keine weitere Ebene“.
    ///
    /// # Nebenläufigkeit
    /// Rein.
    ///
    /// # Beispiele
    /// ```rust,no_run
    /// use harw_agent_dsl::organization::RawClanSpec;
    ///
    /// let clan: RawClanSpec = toml::from_str(r#"
    /// id = "research"
    /// name = "Research Clan"
    /// leader = { id = "harwness.agent.research-orchestrator@1" }
    /// family = { id = "harwness.family.research@1" }
    /// plan_scope = "research-*"
    /// child_depth_cost = 2
    /// "#).unwrap();
    /// assert_eq!(clan.child_depth(1), Some(3));
    /// ```
    #[must_use]
    pub fn child_depth(&self, parent_depth: u32) -> Option<u32> {
        parent_depth.checked_add(self.child_depth_cost)
    }

    /// Wie viele Kindebenen unter `parent_depth` innerhalb der Decke
    /// `depth_ceiling` für diesen Clan noch entstehen dürfen.
    ///
    /// # Argumente
    /// - `parent_depth` (`u32`): Tiefe des startenden Knotens.
    /// - `depth_ceiling` (`u32`): die absolute, geerbte Tiefendecke (größte
    ///   zulässige Tiefe einer Sitzung in diesem Teilbaum).
    ///
    /// # Rückgabe
    /// `(depth_ceiling − parent_depth) / child_depth_cost` (abgerundet,
    /// sättigend). Bei `child_depth_cost = 0` — nur über eine nicht
    /// aufgelöste Rohdefinition erreichbar — fail-closed `0`, nie
    /// „unbegrenzt“.
    ///
    /// # Nebenläufigkeit
    /// Rein.
    #[must_use]
    pub fn remaining_levels(&self, parent_depth: u32, depth_ceiling: u32) -> u32 {
        if self.child_depth_cost == 0 {
            return 0;
        }
        depth_ceiling.saturating_sub(parent_depth) / self.child_depth_cost
    }

    /// Ob ein Kind dieses Clans auf `parent_depth` unter `depth_ceiling`
    /// noch zulässig ist.
    ///
    /// # Rückgabe
    /// `true`, wenn [`Self::child_depth`] existiert und `≤ depth_ceiling` ist.
    #[must_use]
    pub fn admits_child_at(&self, parent_depth: u32, depth_ceiling: u32) -> bool {
        self.child_depth_cost > 0
            && self
                .child_depth(parent_depth)
                .is_some_and(|depth| depth <= depth_ceiling)
    }
}

impl ResolvedOrganization {
    /// Der Clan mit der ID `clan_id`, falls vorhanden.
    #[must_use]
    pub fn clan(&self, clan_id: &str) -> Option<&RawClanSpec> {
        self.clans.iter().find(|clan| clan.id == clan_id)
    }

    /// Die Tiefenkosten des Clans, den die Agentendefinition `leader_name`
    /// führt (Vergleich über `leader.id.name`).
    ///
    /// # Beschreibung
    /// Führt ein Leader mehrere Clans, gilt das **Maximum** ihrer Kosten —
    /// die strengere Grenze, nie die großzügigere.
    ///
    /// # Rückgabe
    /// `Some(kosten)`, wenn `leader_name` mindestens einen Clan führt, sonst
    /// `None` (der Aufrufer bleibt dann bei der Standardtiefe `1`).
    #[must_use]
    pub fn child_depth_cost_for_leader(&self, leader_name: &str) -> Option<u32> {
        self.clans
            .iter()
            .filter(|clan| clan.leader.id.name == leader_name)
            .map(|clan| clan.child_depth_cost)
            .max()
    }
}

/// Eine Cell — temporärer Fan-out-Batch innerhalb eines Clans (§22, Invariante 11).
///
/// # Beschreibung
/// Cells definieren kurzlebige parallele Arbeitsgruppen innerhalb eines Clans.
/// Das Feld `clan` muss auf eine existierende `clan.id` derselben Organisation
/// verweisen (geprüft in `resolve_organization`).
///
/// # Felder
/// - `id` (`String`): Eindeutige Cell-ID.
/// - `clan` (`String`): Referenz auf den übergeordneten Clan (`clan.id`).
/// - `kind` (`CellKind`): Ausführungsart (fanout, barrier, sequential).
/// - `barrier` (`CellBarrier`): Abschlussbedingung.
/// - `write_partition` (`CellWritePartition`): Schreibtrennungs-Policy.
/// - `members_from_plan` (`String`): Plan-Pfad-Glob für Mitglieder.
///
/// # Nebenläufigkeit
/// `Send + Sync`.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::organization::{RawCellSpec, CellKind, CellBarrier, CellWritePartition};
///
/// let toml_str = r#"
/// id = "provider-adapters"
/// clan = "implementation"
/// kind = "fanout"
/// barrier = "all-terminal"
/// write_partition = "required"
/// members_from_plan = "implementation/providers/*"
/// "#;
/// let cell: RawCellSpec = toml::from_str(toml_str).unwrap();
/// assert_eq!(cell.kind, CellKind::Fanout);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCellSpec {
    /// Eindeutige Cell-ID innerhalb dieser Organisation.
    pub id: String,
    /// Referenz auf den übergeordneten Clan (`clan.id`).
    pub clan: String,
    /// Ausführungsart der Cell.
    pub kind: CellKind,
    /// Abschlussbedingung der Cell.
    pub barrier: CellBarrier,
    /// Schreibtrennungs-Policy.
    pub write_partition: CellWritePartition,
    /// Plan-Pfad-Glob zur dynamischen Mitgliederbestimmung.
    pub members_from_plan: String,
}

/// Art der Cell-Ausführung (§15).
///
/// # Beschreibung
/// Bestimmt, wie die Mitglieder einer Cell ausgeführt werden.
///
/// # Varianten
/// - `Fanout` — Alle Mitglieder werden parallel gestartet.
/// - `Barrier` — Mitglieder warten auf ein gemeinsames Signal.
/// - `Sequential` — Mitglieder werden sequenziell ausgeführt.
///
/// # Serialisierung
/// `snake_case` (z. B. `"fanout"`, `"barrier"`, `"sequential"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellKind {
    /// Alle Mitglieder werden parallel gestartet.
    Fanout,
    /// Mitglieder warten auf ein gemeinsames Barrier-Signal.
    Barrier,
    /// Mitglieder werden sequenziell ausgeführt.
    Sequential,
}

/// Abschlussbedingung einer Cell (§15).
///
/// # Beschreibung
/// Legt fest, wann eine Cell als abgeschlossen gilt.
///
/// # Varianten
/// - `AllTerminal` — Alle Mitglieder müssen terminiert haben.
/// - `AnyTerminal` — Genügt, wenn ein Mitglied terminiert.
/// - `ExplicitJoin` — Expliziter Join durch den Orchestrator erforderlich.
///
/// # Serialisierung
/// `kebab-case` (z. B. `"all-terminal"`, `"any-terminal"`, `"explicit-join"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CellBarrier {
    /// Alle Mitglieder müssen terminiert haben.
    AllTerminal,
    /// Ein einzelnes terminiertes Mitglied genügt.
    AnyTerminal,
    /// Der Orchestrator führt den Join explizit durch.
    ExplicitJoin,
}

/// Schreibtrennungs-Policy einer Cell (§15).
///
/// # Beschreibung
/// Legt fest, ob Mitglieder disjunkte Schreibmengen haben müssen.
///
/// # Varianten
/// - `Required` — Disjunkte Schreibmengen sind zwingend.
/// - `Advisory` — Disjunkte Schreibmengen werden empfohlen.
/// - `None` — Keine Schreibtrennung gefordert.
///
/// # Serialisierung
/// `snake_case` (z. B. `"required"`, `"advisory"`, `"none"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellWritePartition {
    /// Disjunkte Schreibmengen sind zwingend erforderlich.
    Required,
    /// Disjunkte Schreibmengen werden empfohlen, aber nicht erzwungen.
    Advisory,
    /// Keine Schreibtrennungsanforderung.
    None,
}

// ─── Aufgelöste IR ──────────────────────────────────────────────────────────

/// Aufgelöste Organisation nach Durchlauf durch die Layer-Kaskade (IR).
///
/// # Beschreibung
/// Enthält die vollständig fusionierte Organisation nach `resolve_organization`.
/// Der `trace` dokumentiert alle angewandten Layer-Schritte für Audit-Zwecke.
///
/// # Felder
/// - `id` (`DefinitionId`): ID der Zielorganisation.
/// - `version` (`Version`): Semantische Version.
/// - `name` (`String`): Aufgelöster menschenlesbarer Name.
/// - `root` (`RawRootSpec`): Root-Binding (unveränderlich vom Basis-Layer).
/// - `clans` (`Vec<RawClanSpec>`): Kumulierte Clan-Liste.
/// - `cells` (`Vec<RawCellSpec>`): Kumulierte Cell-Liste.
/// - `trace` (`ResolutionTrace`): Auditpfad der angewandten Schritte.
///
/// # Nebenläufigkeit
/// `Send + Sync`.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::organization::{resolve_organization, RawOrganizationDefinition};
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_agent_dsl::layers::DefinitionLayer;
///
/// let toml_str = r#"
/// schema = "harwness.organization/v1"
/// id = "harwness.organization.sw@1"
/// version = "1.0.0"
/// name = "SW Org"
///
/// [root]
/// agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
/// family = { id = "harwness.family.focused-coding@1" }
/// "#;
/// let raw: RawOrganizationDefinition = toml::from_str(toml_str).unwrap();
/// let id = DefinitionId::parse("harwness.organization.sw@1").unwrap();
/// let layers = vec![(DefinitionLayer::BuiltIn, raw)];
/// let resolved = resolve_organization(&id, &layers, time::OffsetDateTime::now_utc()).unwrap();
/// assert_eq!(resolved.name, "SW Org");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedOrganization {
    /// Eindeutige Namensraum-ID der aufgelösten Organisation.
    pub id: DefinitionId,
    /// Semantische Vollversion.
    pub version: crate::ids::Version,
    /// Menschenlesbarer Name der Organisation.
    pub name: String,
    /// Root-Orchestrator und Family-Binding (unveränderlich vom Base-Layer).
    pub root: RawRootSpec,
    /// Vollständige Clan-Liste nach Patch-Anwendung.
    pub clans: Vec<RawClanSpec>,
    /// Vollständige Cell-Liste nach Patch-Anwendung.
    pub cells: Vec<RawCellSpec>,
    /// Auditpfad der Auflösung.
    pub trace: ResolutionTrace,
}

// ─── Auflösung ──────────────────────────────────────────────────────────────

/// Löst eine Organisation über die Layer-Kaskade auf (§4, §15, §22).
///
/// # Beschreibung
/// Durchläuft `layers` in aufsteigender Priorität (niedriger Index = niedrigere
/// Priorität). Zuerst wird die Basis-Definition der `target_id` gesucht. Wenn
/// `extends` deklariert ist, wird der Basis-Layer ebenfalls gesucht und als
/// Ausgangszustand verwendet. Anschließend werden Patches angewendet.
///
/// ## Validierungen (§22)
/// - Wenn `extends` gesetzt ist, muss die referenzierte Basis in `layers` vorhanden sein
///   → `DslError::MissingBase`.
/// - Doppelte `clan.id` innerhalb der aufgelösten Clan-Liste
///   → `DslError::Parse`.
/// - Jede `cell.clan`-Referenz muss auf eine existierende `clan.id` zeigen
///   → `DslError::MissingBase`.
///
/// ## Unterstützte Patch-Operationen
/// - `patch.clans.append` — fügt neue Clans zur Liste hinzu.
/// - `patch.cells.append` — fügt neue Cells zur Liste hinzu.
/// - `patch.name.replace` — überschreibt den Namen.
///
/// # Argumente
/// - `target_id` (`&DefinitionId`): ID der aufzulösenden Organisation.
/// - `layers` (`&[(DefinitionLayer, RawOrganizationDefinition)]`):
///   Geordnete Layer-Liste (Index 0 = niedrigste Priorität).
/// - `now` (`OffsetDateTime`): Zeitstempel für Trace-Einträge.
///
/// # Rückgabe
/// `Ok(ResolvedOrganization)` bei erfolgreicher Auflösung.
///
/// # Fehler
/// - [`DslError::MissingBase`]: `target_id` ist in keiner Schicht vorhanden, oder
///   `extends` verweist auf eine nicht vorhandene Definition.
/// - [`DslError::Parse`]: Doppelte `clan.id` gefunden.
/// - [`DslError::MissingBase`]: `cell.clan` zeigt auf nicht vorhandenen Clan.
///
/// # Nebenläufigkeit
/// Reine Funktion — keine Seiteneffekte, keine Locks. Sicher für parallelen Aufruf.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::organization::{resolve_organization, RawOrganizationDefinition};
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_agent_dsl::layers::DefinitionLayer;
///
/// let toml_str = r#"
/// schema = "harwness.organization/v1"
/// id = "harwness.organization.sw@1"
/// version = "1.0.0"
/// name = "SW Org"
///
/// [root]
/// agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
/// family = { id = "harwness.family.focused-coding@1" }
/// "#;
/// let raw: RawOrganizationDefinition = toml::from_str(toml_str).unwrap();
/// let id = DefinitionId::parse("harwness.organization.sw@1").unwrap();
/// let layers = vec![(DefinitionLayer::BuiltIn, raw)];
/// let resolved = resolve_organization(&id, &layers, time::OffsetDateTime::now_utc()).unwrap();
/// assert_eq!(resolved.clans.len(), 0);
/// ```
pub fn resolve_organization(
    target_id: &DefinitionId,
    layers: &[(DefinitionLayer, RawOrganizationDefinition)],
    now: OffsetDateTime,
) -> DslResult<ResolvedOrganization> {
    // Finde die Definition der Ziel-ID
    let target_def = layers
        .iter()
        .find(|(_, def)| &def.id == target_id)
        .map(|(_, def)| def)
        .ok_or_else(|| DslError::MissingBase {
            of: Box::new(target_id.clone()),
            referenced: Box::new(DefinitionRef {
                id: target_id.clone(),
                version: None,
            }),
            location: crate::error::DiagLocation::none(),
        })?;

    let mut trace_steps: Vec<ResolutionStep> = Vec::new();

    // Bestimme Ausgangszustand: Basis-Def oder target_def selbst
    let (
        mut resolved_name,
        mut resolved_clans,
        mut resolved_cells,
        resolved_root,
        resolved_version,
    ) = if let Some(base_ref) = &target_def.extends {
        // Basis-Definition in den Layers suchen
        let base_def = layers
            .iter()
            .find(|(_, def)| def.id == base_ref.id)
            .map(|(_, def)| def)
            .ok_or_else(|| DslError::MissingBase {
                of: Box::new(target_id.clone()),
                referenced: Box::new(base_ref.clone()),
                location: crate::error::DiagLocation::field("extends"),
            })?;

        trace_steps.push(ResolutionStep {
            source: base_def.id.to_string(),
            kind: "base".to_owned(),
            applied_at: now,
        });

        (
            base_def.name.clone(),
            base_def.clans.clone(),
            base_def.cells.clone(),
            base_def.root.clone(),
            target_def.version.clone(),
        )
    } else {
        // Kein extends — target_def ist die Basis
        trace_steps.push(ResolutionStep {
            source: target_def.id.to_string(),
            kind: "base".to_owned(),
            applied_at: now,
        });

        (
            target_def.name.clone(),
            target_def.clans.clone(),
            target_def.cells.clone(),
            target_def.root.clone(),
            target_def.version.clone(),
        )
    };

    // Wenn extends gesetzt ist, füge die eigenen Clans/Cells des target_def hinzu
    // und wende Patches an
    if target_def.extends.is_some() {
        // Füge eigene Clans/Cells hinzu (vor Patches)
        resolved_clans.extend(target_def.clans.iter().cloned());
        resolved_cells.extend(target_def.cells.iter().cloned());
        resolved_name = target_def.name.clone();

        trace_steps.push(ResolutionStep {
            source: target_def.id.to_string(),
            kind: "overlay".to_owned(),
            applied_at: now,
        });
    }

    // Patch-Operationen anwenden
    let patch = &target_def.patch;

    // patch.name.replace
    if let Some(name_table) = patch.get("name").and_then(|v| v.as_table())
        && let Some(replace_val) = name_table.get("replace").and_then(|v| v.as_str())
    {
        resolved_name = replace_val.to_owned();
    }

    // patch.clans.append
    if let Some(clans_table) = patch.get("clans").and_then(|v| v.as_table())
        && let Some(append_val) = clans_table.get("append")
    {
        // Deserialisiere Array von RawClanSpec
        let appended: Vec<RawClanSpec> = append_val
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| DslError::Toml(e.to_string()))?;
        resolved_clans.extend(appended);
    }

    // patch.cells.append
    if let Some(cells_table) = patch.get("cells").and_then(|v| v.as_table())
        && let Some(append_val) = cells_table.get("append")
    {
        let appended: Vec<RawCellSpec> = append_val
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| DslError::Toml(e.to_string()))?;
        resolved_cells.extend(appended);
    }

    // Validierung: doppelte clan.id → DslError::Parse
    let mut seen_clan_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for clan in &resolved_clans {
        if !seen_clan_ids.insert(clan.id.as_str()) {
            return Err(DslError::Parse(format!(
                "Doppelte clan.id '{}' in Organisation '{}'",
                clan.id, target_id
            )));
        }
    }

    // Validierung: child_depth_cost ≥ 1 → sonst DslError::Parse. Kosten 0
    // hießen „eine Kindebene kostet keine Tiefe“ — ein Clan könnte seinen
    // Teilbaum dann unbegrenzt tief wachsen lassen (§22: Clans sind
    // begrenzte, run-lokale Subtrees).
    for clan in &resolved_clans {
        if clan.child_depth_cost == 0 {
            return Err(DslError::Parse(format!(
                "clan '{}' in Organisation '{}': child_depth_cost muss mindestens 1 sein",
                clan.id, target_id
            )));
        }
    }

    // Validierung: cell.clan muss auf existierende clan.id verweisen → DslError::MissingBase
    for cell in &resolved_cells {
        if !seen_clan_ids.contains(cell.clan.as_str()) {
            // Baue eine synthetische DefinitionId für den Fehler
            let clan_pseudo_id = DefinitionId {
                namespace: target_id.namespace.clone(),
                kind: "clan".to_owned(),
                name: cell.clan.clone(),
                major: 0,
            };
            return Err(DslError::MissingBase {
                of: Box::new(target_id.clone()),
                referenced: Box::new(DefinitionRef {
                    id: clan_pseudo_id,
                    version: None,
                }),
                location: crate::error::DiagLocation::field("cells[*].clan"),
            });
        }
    }

    // Patch-Trace-Eintrag, falls ein nicht-leeres Patch vorhanden ist
    if !patch.is_empty() {
        trace_steps.push(ResolutionStep {
            source: target_def.id.to_string(),
            kind: "patch".to_owned(),
            applied_at: now,
        });
    }

    Ok(ResolvedOrganization {
        id: target_id.clone(),
        version: resolved_version,
        name: resolved_name,
        root: resolved_root,
        clans: resolved_clans,
        cells: resolved_cells,
        trace: ResolutionTrace { steps: trace_steps },
    })
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::DefinitionLayer;
    use crate::test_support::{TestResult, ctx};

    // Hilfsfunktion: minimale TOML-Org ohne Clans/Cells
    fn minimal_org_toml(id: &str, name: &str) -> String {
        format!(
            r#"
schema = "harwness.organization/v1"
id = "{id}"
version = "1.0.0"
name = "{name}"

[root]
agent = {{ id = "harwness.agent.focused-coding-orchestrator@1" }}
family = {{ id = "harwness.family.focused-coding@1" }}
"#
        )
    }

    fn parse_org(toml_str: &str) -> TestResult<RawOrganizationDefinition> {
        toml::from_str(toml_str).map_err(ctx("TOML-Parsing fehlgeschlagen"))
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    // Test 1: minimale TOML-Org → Serde-Roundtrip
    #[test]
    fn raw_org_serde_roundtrip_minimal() -> TestResult {
        let toml_str = minimal_org_toml("harwness.organization.sw@1", "SW Org");
        let raw: RawOrganizationDefinition = toml::from_str(&toml_str)?;
        assert_eq!(raw.schema, "harwness.organization/v1");
        assert_eq!(raw.id.name, "sw");
        assert_eq!(raw.name, "SW Org");
        assert!(raw.clans.is_empty());
        assert!(raw.cells.is_empty());
        assert!(raw.extends.is_none());
        Ok(())
    }

    // Test 2: vollständiges TOML-Beispiel mit root + 2 clans + 1 cell
    #[test]
    fn raw_org_serde_full_example() -> TestResult {
        let toml_str = r#"
schema = "harwness.organization/v1"
id = "harwness.organization.software-project@1"
version = "1.0.0"
name = "Software Project Organization"

[root]
agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }

[[clans]]
id = "research"
name = "Research Clan"
leader = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }
plan_scope = "research/*"

[[clans]]
id = "implementation"
name = "Implementation Clan"
leader = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }
plan_scope = "implementation/*"

[[cells]]
id = "provider-adapters"
clan = "implementation"
kind = "fanout"
barrier = "all-terminal"
write_partition = "required"
members_from_plan = "implementation/providers/*"
"#;
        let raw: RawOrganizationDefinition = toml::from_str(toml_str)?;
        assert_eq!(raw.clans.len(), 2);
        assert_eq!(raw.cells.len(), 1);
        assert_eq!(raw.clans[0].id, "research");
        assert_eq!(raw.clans[1].id, "implementation");
        assert_eq!(raw.cells[0].clan, "implementation");
        Ok(())
    }

    // Test 3: CellKind::Fanout → snake_case "fanout"
    #[test]
    fn cell_kind_serde_snake_case() -> TestResult {
        let kind = CellKind::Fanout;
        let json = serde_json::to_string(&kind)?;
        assert_eq!(json, "\"fanout\"");
        let back: CellKind = serde_json::from_str(&json)?;
        assert_eq!(back, CellKind::Fanout);

        let barrier = CellKind::Barrier;
        let json2 = serde_json::to_string(&barrier)?;
        assert_eq!(json2, "\"barrier\"");

        let seq = CellKind::Sequential;
        let json3 = serde_json::to_string(&seq)?;
        assert_eq!(json3, "\"sequential\"");
        Ok(())
    }

    // Test 4: CellBarrier::AllTerminal → kebab-case "all-terminal"
    #[test]
    fn cell_barrier_serde_kebab_case() -> TestResult {
        let barrier = CellBarrier::AllTerminal;
        let json = serde_json::to_string(&barrier)?;
        assert_eq!(json, "\"all-terminal\"");
        let back: CellBarrier = serde_json::from_str(&json)?;
        assert_eq!(back, CellBarrier::AllTerminal);

        let any = CellBarrier::AnyTerminal;
        let json2 = serde_json::to_string(&any)?;
        assert_eq!(json2, "\"any-terminal\"");

        let explicit = CellBarrier::ExplicitJoin;
        let json3 = serde_json::to_string(&explicit)?;
        assert_eq!(json3, "\"explicit-join\"");
        Ok(())
    }

    // Test 5: CellWritePartition::None deserialisiert korrekt
    #[test]
    fn cell_write_partition_default_none_deserializes() -> TestResult {
        let json = "\"none\"";
        let wp: CellWritePartition = serde_json::from_str(json)?;
        assert_eq!(wp, CellWritePartition::None);

        let req: CellWritePartition = serde_json::from_str("\"required\"")?;
        assert_eq!(req, CellWritePartition::Required);

        let adv: CellWritePartition = serde_json::from_str("\"advisory\"")?;
        assert_eq!(adv, CellWritePartition::Advisory);
        Ok(())
    }

    // Test 6: TOML ohne child_depth_cost → Standard 1
    #[test]
    fn default_child_depth_cost_is_1() -> TestResult {
        let toml_str = r#"
id = "research"
name = "Research Clan"
leader = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }
plan_scope = "research/*"
"#;
        let clan: RawClanSpec = toml::from_str(toml_str)?;
        assert_eq!(clan.child_depth_cost, 1);
        Ok(())
    }

    fn clan_with_cost(cost: u32) -> TestResult<RawClanSpec> {
        let toml_str = format!(
            r#"
id = "research"
name = "Research Clan"
leader = {{ id = "harwness.agent.research-orchestrator@1" }}
family = {{ id = "harwness.family.research@1" }}
plan_scope = "research-*"
child_depth_cost = {cost}
"#
        );
        Ok(toml::from_str(&toml_str)?)
    }

    #[test]
    fn child_depth_adds_the_clan_cost() -> TestResult {
        assert_eq!(clan_with_cost(1)?.child_depth(2), Some(3));
        assert_eq!(clan_with_cost(2)?.child_depth(2), Some(4));
        assert_eq!(clan_with_cost(1)?.child_depth(u32::MAX), None);
        Ok(())
    }

    #[test]
    fn remaining_levels_divides_the_headroom_by_the_cost() -> TestResult {
        assert_eq!(clan_with_cost(1)?.remaining_levels(1, 4), 3);
        assert_eq!(clan_with_cost(2)?.remaining_levels(1, 4), 1);
        assert_eq!(clan_with_cost(2)?.remaining_levels(5, 4), 0);
        // Fail-closed: eine unaufgelöste Kostenangabe 0 gibt nie Tiefe frei.
        assert_eq!(clan_with_cost(0)?.remaining_levels(0, 4), 0);
        Ok(())
    }

    #[test]
    fn admits_child_at_respects_ceiling_and_zero_cost() -> TestResult {
        assert!(clan_with_cost(1)?.admits_child_at(2, 3));
        assert!(!clan_with_cost(2)?.admits_child_at(2, 3));
        assert!(!clan_with_cost(0)?.admits_child_at(0, 3));
        Ok(())
    }

    #[test]
    fn resolve_rejects_zero_child_depth_cost() -> TestResult {
        let toml_str = r#"
schema = "harwness.organization/v1"
id = "harwness.organization.zero-cost@1"
version = "1.0.0"
name = "Zero Cost"

[root]
agent = { id = "harwness.agent.root-orchestrator@1" }
family = { id = "harwness.family.research@1" }

[[clans]]
id = "research"
name = "Research Clan"
leader = { id = "harwness.agent.research-orchestrator@1" }
family = { id = "harwness.family.research@1" }
plan_scope = "research-*"
child_depth_cost = 0
"#;
        let raw = parse_org(toml_str)?;
        let id = DefinitionId::parse("harwness.organization.zero-cost@1")?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        match resolve_organization(&id, &layers, now()) {
            Err(DslError::Parse(message)) => {
                assert!(message.contains("child_depth_cost"), "{message}");
                Ok(())
            }
            other => Err(crate::test_support::TestError::Unexpected(format!(
                "erwartet DslError::Parse, bekommen: {other:?}"
            ))),
        }
    }

    #[test]
    fn child_depth_cost_for_leader_takes_the_strictest_clan() -> TestResult {
        let toml_str = r#"
schema = "harwness.organization/v1"
id = "harwness.organization.costs@1"
version = "1.0.0"
name = "Costs"

[root]
agent = { id = "harwness.agent.root-orchestrator@1" }
family = { id = "harwness.family.research@1" }

[[clans]]
id = "synthesis"
name = "Synthesis"
leader = { id = "harwness.agent.analysis-orchestrator@1" }
family = { id = "harwness.family.research@1" }
plan_scope = "*-synthesis"

[[clans]]
id = "security"
name = "Security"
leader = { id = "harwness.agent.analysis-orchestrator@1" }
family = { id = "harwness.family.security@1" }
plan_scope = "security-*"
child_depth_cost = 3
"#;
        let raw = parse_org(toml_str)?;
        let id = DefinitionId::parse("harwness.organization.costs@1")?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let resolved = resolve_organization(&id, &layers, now())?;
        assert_eq!(
            resolved.child_depth_cost_for_leader("analysis-orchestrator"),
            Some(3)
        );
        assert_eq!(resolved.child_depth_cost_for_leader("unbekannt"), None);
        assert_eq!(
            resolved.clan("synthesis").map(|clan| clan.child_depth_cost),
            Some(1)
        );
        assert!(resolved.clan("gibt-es-nicht").is_none());
        Ok(())
    }

    // Test 7: extends verweist auf unbekannte ID → DslError::MissingBase
    #[test]
    fn resolve_missing_base_errors() -> TestResult {
        let toml_str = r#"
schema = "harwness.organization/v1"
id = "mia.organization.derived@1"
version = "1.0.0"
name = "Derived"
extends = { id = "harwness.organization.nonexistent@1" }

[root]
agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }
"#;
        let raw: RawOrganizationDefinition = toml::from_str(toml_str)?;
        let id = DefinitionId::parse("mia.organization.derived@1")?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let result = resolve_organization(&id, &layers, now());
        assert!(
            matches!(result, Err(DslError::MissingBase { .. })),
            "Erwartet MissingBase, got: {:?}",
            result
        );
        Ok(())
    }

    // Test 8: Zwei Clans mit gleicher id → DslError::Parse
    #[test]
    fn resolve_duplicate_clan_id_errors() -> TestResult {
        let toml_str = r#"
schema = "harwness.organization/v1"
id = "harwness.organization.dup@1"
version = "1.0.0"
name = "Dup Org"

[root]
agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }

[[clans]]
id = "research"
name = "Research Clan"
leader = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }
plan_scope = "research/*"

[[clans]]
id = "research"
name = "Research Clan Duplicate"
leader = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }
plan_scope = "research/*"
"#;
        let raw: RawOrganizationDefinition = toml::from_str(toml_str)?;
        let id = DefinitionId::parse("harwness.organization.dup@1")?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let result = resolve_organization(&id, &layers, now());
        assert!(
            matches!(result, Err(DslError::Parse(_))),
            "Erwartet Parse-Fehler bei doppelter clan.id, got: {:?}",
            result
        );
        Ok(())
    }

    // Test 9: cell.clan zeigt auf nicht vorhandenen Clan → DslError::MissingBase
    #[test]
    fn resolve_cell_refs_unknown_clan_errors() -> TestResult {
        let toml_str = r#"
schema = "harwness.organization/v1"
id = "harwness.organization.badcell@1"
version = "1.0.0"
name = "Bad Cell Org"

[root]
agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }

[[clans]]
id = "research"
name = "Research Clan"
leader = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }
plan_scope = "research/*"

[[cells]]
id = "orphan-cell"
clan = "nonexistent-clan"
kind = "fanout"
barrier = "all-terminal"
write_partition = "required"
members_from_plan = "impl/*"
"#;
        let raw: RawOrganizationDefinition = toml::from_str(toml_str)?;
        let id = DefinitionId::parse("harwness.organization.badcell@1")?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let result = resolve_organization(&id, &layers, now());
        assert!(
            matches!(result, Err(DslError::MissingBase { .. })),
            "Erwartet MissingBase für unbekannten Clan, got: {:?}",
            result
        );
        Ok(())
    }

    // Test 10: Base hat 1 Clan, Patch fügt 2 hinzu → Ergebnis 3
    #[test]
    fn resolve_appends_clans_via_patch() -> TestResult {
        let base_toml = r#"
schema = "harwness.organization/v1"
id = "harwness.organization.base@1"
version = "1.0.0"
name = "Base Org"

[root]
agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }

[[clans]]
id = "research"
name = "Research Clan"
leader = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }
plan_scope = "research/*"
"#;

        let derived_toml = r#"
schema = "harwness.organization/v1"
id = "mia.organization.derived@1"
version = "1.0.0"
name = "Derived Org"
extends = { id = "harwness.organization.base@1" }

[root]
agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }

[patch.clans]
append = [
  { id = "memory", name = "Memory Clan", leader = { id = "harwness.agent.focused-coding-orchestrator@1" }, family = { id = "harwness.family.focused-coding@1" }, plan_scope = "memory/*", child_depth_cost = 1 },
  { id = "agent-runtime", name = "Agent Runtime Clan", leader = { id = "harwness.agent.focused-coding-orchestrator@1" }, family = { id = "harwness.family.focused-coding@1" }, plan_scope = "agents/*", child_depth_cost = 1 },
]
"#;

        let base_def: RawOrganizationDefinition = toml::from_str(base_toml)?;
        let derived_def: RawOrganizationDefinition = toml::from_str(derived_toml)?;

        let id = DefinitionId::parse("mia.organization.derived@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base_def),
            (DefinitionLayer::UserGlobal, derived_def),
        ];
        let resolved = resolve_organization(&id, &layers, now())?;
        assert_eq!(
            resolved.clans.len(),
            3,
            "Erwartet 3 Clans (1 Basis + 2 Patch), got {}",
            resolved.clans.len()
        );
        assert_eq!(resolved.clans[0].id, "research");
        assert_eq!(resolved.clans[1].id, "memory");
        assert_eq!(resolved.clans[2].id, "agent-runtime");
        Ok(())
    }

    // Test 11: patch.cells.append fügt Cells hinzu
    #[test]
    fn resolve_appends_cells_via_patch() -> TestResult {
        let base_toml = r#"
schema = "harwness.organization/v1"
id = "harwness.organization.base2@1"
version = "1.0.0"
name = "Base Org 2"

[root]
agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }

[[clans]]
id = "implementation"
name = "Implementation Clan"
leader = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }
plan_scope = "implementation/*"
"#;

        let derived_toml = r#"
schema = "harwness.organization/v1"
id = "mia.organization.derived2@1"
version = "1.0.0"
name = "Derived Org 2"
extends = { id = "harwness.organization.base2@1" }

[root]
agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }

[patch.cells]
append = [
  { id = "provider-adapters", clan = "implementation", kind = "fanout", barrier = "all-terminal", write_partition = "required", members_from_plan = "implementation/providers/*" },
  { id = "api-layer", clan = "implementation", kind = "sequential", barrier = "explicit-join", write_partition = "advisory", members_from_plan = "implementation/api/*" },
]
"#;

        let base_def: RawOrganizationDefinition = toml::from_str(base_toml)?;
        let derived_def: RawOrganizationDefinition = toml::from_str(derived_toml)?;

        let id = DefinitionId::parse("mia.organization.derived2@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base_def),
            (DefinitionLayer::UserGlobal, derived_def),
        ];
        let resolved = resolve_organization(&id, &layers, now())?;
        assert_eq!(resolved.cells.len(), 2);
        assert_eq!(resolved.cells[0].id, "provider-adapters");
        assert_eq!(resolved.cells[1].id, "api-layer");
        assert_eq!(resolved.cells[0].kind, CellKind::Fanout);
        assert_eq!(resolved.cells[1].kind, CellKind::Sequential);
        Ok(())
    }

    // Test 12: 2 Layer → mind. 2 Trace-Steps
    #[test]
    fn resolve_trace_contains_multi_layer_steps() -> TestResult {
        let base_toml = minimal_org_toml("harwness.organization.trace-base@1", "Trace Base");
        let derived_toml = r#"
schema = "harwness.organization/v1"
id = "mia.organization.trace-derived@1"
version = "1.0.0"
name = "Trace Derived"
extends = { id = "harwness.organization.trace-base@1" }

[root]
agent = { id = "harwness.agent.focused-coding-orchestrator@1" }
family = { id = "harwness.family.focused-coding@1" }
"#;
        let base_def = parse_org(&base_toml)?;
        let derived_def: RawOrganizationDefinition = toml::from_str(derived_toml)?;

        let id = DefinitionId::parse("mia.organization.trace-derived@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base_def),
            (DefinitionLayer::Workspace, derived_def),
        ];
        let resolved = resolve_organization(&id, &layers, now())?;
        assert!(
            resolved.trace.steps.len() >= 2,
            "Erwartet mind. 2 Trace-Schritte, got {}",
            resolved.trace.steps.len()
        );
        assert_eq!(resolved.trace.steps[0].kind, "base");
        assert_eq!(resolved.trace.steps[1].kind, "overlay");
        Ok(())
    }
}
