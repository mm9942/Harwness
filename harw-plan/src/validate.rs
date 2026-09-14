//! Validation aller Plan-Aktionen vor `apply` — die Sicherheitsgrenze des
//! Plan-Graphen.
//!
//! Verantwortungsbereich: Implementiert alle 16 Validierungsregeln aus
//! Design-Doc §5. Jede Regel ist eine eigenständige Hilfsfunktion oder ein
//! klar markierter Block (`// Regel N: …`); [`validate_with`] kombiniert sie
//! pro Aktionsvariante.
//!
//! # Regelübersicht
//! 1. Referenzierte Knoten existieren.
//! 2. `AddDependency` erzeugt keinen Zyklus (DFS über `dependencies`).
//! 3. `SetStatus` folgt der Status-Matrix.
//! 4. `SetStatus(Completed)` verlangt mindestens einen `EvidenceRef`.
//! 5. `forbidden_scope ∩ write_scope == ∅`.
//! 6. `write_scope` disjunkt zu aktiven (`Ready`/`InProgress`) Knoten — beim
//!    Einfügen **und** beim Übergang nach `Ready`/`InProgress`.
//! 7. `Supersede` erhöht die Revision streng monoton.
//! 8. `Invalidate` nur entlang der Status-Matrix (`Completed` → eigener
//!    Fehler; `Superseded`/`Invalidated` sind nicht invalidierbar).
//! 9. `AttachEvidence` ist idempotent (Duplikate sind kein Fehler).
//! 10. `Expand`: Kind-`write_scope` liegt im Parent-`write_scope`, Parent ist
//!     nicht versiegelt, Kind-IDs sind frei, Kinder untereinander disjunkt,
//!     Verschachtelungstiefe ≤ `PlanToolConfig::max_expand_depth`.
//! 11. `SetStatus(Completed)` auf einem Composite-Knoten verlangt, dass alle
//!     Kind-Knoten `Completed` sind.
//! 12. Explore-before-implement: Knoten, deren `kind` in
//!     `PlanToolConfig::require_exploration_for` liegt, brauchen vor
//!     `Ready`/`InProgress` eine frische Exploration.
//! 13. `UpdateNode` auf einem versiegelten Knoten (`Completed`, `Superseded`,
//!     `Invalidated`) ist verboten — `Reopen` ist der einzige Weg zurück.
//! 14. Ein `NodePatch`, der `write_scope`/`forbidden_scope`/`dependencies`
//!     setzt, wird gegen die Regeln 1, 2, 5 und 6 revalidiert.
//! 15. `Reopen` führt ausschließlich von `Invalidated` nach `Draft` und
//!     verlangt eine nicht-leere Begründung.
//! 16. `Condense` verdichtet ausschließlich abgeschlossene
//!     `Research`/`Explore`/`Analysis`-Knoten in einen neuen
//!     `Contract`-Knoten.
//!
//! # Schließungen aus F-013 §5.1 (W3/C-PLAN)
//! - Einfügende Aktionen (`AddNode`, `Expand`-Kinder, `Condense`-Ersatz)
//!   akzeptieren nur `Draft` — kein `Completed` ohne Evidenz, kein
//!   eingeschleuster Terminalzustand.
//! - `Superseded` ist terminal: `Invalidate` folgt der Matrix.
//! - Nachweise mit Zukunfts-Zeitstempel gelten nicht als frisch (die Mutation
//!   kappt Payload-Zeitstempel zusätzlich auf `now`).
//! - `AddDependency` und `UpdateNode.dependencies` prüfen Siegel und Status:
//!   kein versiegelter Kind-Knoten, kein abgelöster Parent, und ein aktiver
//!   Kind-Knoten bekommt nur abgeschlossene Dependencies.
//! - `SetStatus → Ready|InProgress` prüft Regel 6.
//! - Regel 12 wird beim Eintritt nach `Ready` geprüft; `Ready → InProgress`
//!   prüft sie nicht erneut, weil ein bereits zugelassener Knoten sonst nach
//!   Fristablauf unerreichbar festhinge (die Graph-Abfrage
//!   `graph::missing_explorations` meldet nur `Draft`/`Blocked`-Knoten).
//! - `Create` prüft die `PlanId`-Grammatik (Pfad-Traversal, F-013/G-032).
//!
//! # Status-Matrix
//! Legale Übergänge (siehe [`STATUS_MATRIX`]):
//!
//! ```text
//! Draft       → Ready | Blocked | Invalidated | Superseded
//! Ready       → InProgress | Blocked | Invalidated | Superseded
//! InProgress  → Completed | Blocked | Invalidated | Superseded
//! Blocked     → Ready | Invalidated | Superseded
//! Completed   → Superseded
//! Superseded  → (terminal)
//! Invalidated → Draft   [nur über `PlanAction::Reopen`]
//! ```
//!
//! `Invalidated → Draft` ist neu und der einzige Rückweg aus einem
//! Terminalzustand. Er steht in der Matrix, wird von `SetStatus` aber
//! ausdrücklich abgelehnt (siehe [`REOPEN_ONLY_TRANSITIONS`]): nur
//! [`PlanAction::Reopen`] darf ihn gehen, damit die Wiedereröffnung immer eine
//! Begründung trägt und der Store den Versuchszähler erhöhen kann.
//!
//! # Konfigurationsabhängigkeit
//! Die Regeln 10 und 12 hängen von [`PlanToolConfig`] und der aktuellen Zeit
//! ab. [`validate_with`] ist deshalb die vollständige API. [`validate`] bleibt
//! als Kompatibilitätsfassade erhalten: es benutzt Default-Config-Semantik
//! **ohne** Explorationspflicht (Regel 12 deaktiviert).
//!
//! Exportierte Funktionen: [`validate`], [`validate_with`].

use std::collections::HashSet;

use time::OffsetDateTime;

use crate::actions::{NodePatch, PlanAction};
use crate::admission::ScopeMatcher;
use crate::config::PlanToolConfig;
use crate::error::{PlanError, PlanResult};
use crate::ids::{PathOrSymbol, PlanId, RevisionId, TaskId};
use crate::types::{EvidenceKind, EvidenceRef, Plan, PlanNode, PlanNodeKind, PlanNodeStatus};

/// Legale Statusübergänge: (Von, Nach).
///
/// # Description
/// `Superseded` ist terminal. `Invalidated` ist terminal **außer** über
/// [`PlanAction::Reopen`] — der Eintrag `(Invalidated, Draft)` ist deshalb in
/// [`REOPEN_ONLY_TRANSITIONS`] zusätzlich als reopen-exklusiv markiert und
/// wird von `SetStatus` abgelehnt (Regel 15).
const STATUS_MATRIX: &[(PlanNodeStatus, PlanNodeStatus)] = &[
    (PlanNodeStatus::Draft, PlanNodeStatus::Ready),
    (PlanNodeStatus::Draft, PlanNodeStatus::Blocked),
    // `Invalidate` auf einem Entwurf (z. B. Vertragsänderung vor dem Start)
    // ist legitim und steht deshalb explizit in der Matrix (F-013 §5.1 Punkt 2).
    (PlanNodeStatus::Draft, PlanNodeStatus::Invalidated),
    (PlanNodeStatus::Ready, PlanNodeStatus::InProgress),
    (PlanNodeStatus::Ready, PlanNodeStatus::Blocked),
    (PlanNodeStatus::Ready, PlanNodeStatus::Invalidated),
    (PlanNodeStatus::InProgress, PlanNodeStatus::Completed),
    (PlanNodeStatus::InProgress, PlanNodeStatus::Blocked),
    (PlanNodeStatus::InProgress, PlanNodeStatus::Invalidated),
    (PlanNodeStatus::Blocked, PlanNodeStatus::Ready),
    (PlanNodeStatus::Blocked, PlanNodeStatus::Invalidated),
    // Superseded kann von jedem nicht-terminalen Status erreicht werden
    (PlanNodeStatus::Draft, PlanNodeStatus::Superseded),
    (PlanNodeStatus::Ready, PlanNodeStatus::Superseded),
    (PlanNodeStatus::InProgress, PlanNodeStatus::Superseded),
    (PlanNodeStatus::Blocked, PlanNodeStatus::Superseded),
    (PlanNodeStatus::Completed, PlanNodeStatus::Superseded),
    // Regel 15: einziger Rückweg aus einem Terminalzustand — reopen-exklusiv.
    (PlanNodeStatus::Invalidated, PlanNodeStatus::Draft),
];

/// Übergänge, die zwar in [`STATUS_MATRIX`] stehen, aber ausschließlich über
/// [`PlanAction::Reopen`] gegangen werden dürfen.
///
/// `SetStatus` lehnt sie mit [`PlanError::IllegalTransition`] ab, damit jede
/// Wiedereröffnung eine Begründung trägt (Regel 15).
const REOPEN_ONLY_TRANSITIONS: &[(PlanNodeStatus, PlanNodeStatus)] =
    &[(PlanNodeStatus::Invalidated, PlanNodeStatus::Draft)];

/// Statuswerte, in denen ein Knoten als versiegelt gilt (Regel 13).
const SEALED_STATUSES: &[PlanNodeStatus] = &[
    PlanNodeStatus::Completed,
    PlanNodeStatus::Superseded,
    PlanNodeStatus::Invalidated,
];

/// Statuswerte, die einen Knoten als aktiv ausweisen (Regel 6).
const ACTIVE_STATUSES: &[PlanNodeStatus] = &[PlanNodeStatus::Ready, PlanNodeStatus::InProgress];

/// Knotenarten, die eine frische Exploration erbringen können (Regel 12).
///
/// Crate-weit geteilt mit `graph::missing_explorations` (F-130: beide Module
/// zählen `Explore` **und** `Research`).
pub(crate) const EXPLORATION_KINDS: &[PlanNodeKind] =
    &[PlanNodeKind::Explore, PlanNodeKind::Research];

/// Knotenarten, die per `Condense` verdichtet werden dürfen (Regel 16).
const CONDENSABLE_KINDS: &[PlanNodeKind] = &[
    PlanNodeKind::Research,
    PlanNodeKind::Explore,
    PlanNodeKind::Analysis,
];

// ──────────────────────────────────────────────────────────────────────────────
// Basis-Hilfsfunktionen
// ──────────────────────────────────────────────────────────────────────────────

/// Prüft, ob ein Statusübergang laut [`STATUS_MATRIX`] legal ist.
fn is_legal_transition(from: PlanNodeStatus, to: PlanNodeStatus) -> bool {
    STATUS_MATRIX.iter().any(|&(f, t)| f == from && t == to)
}

/// Prüft, ob ein Übergang ausschließlich über `Reopen` erlaubt ist (Regel 15).
fn is_reopen_only(from: PlanNodeStatus, to: PlanNodeStatus) -> bool {
    REOPEN_ONLY_TRANSITIONS
        .iter()
        .any(|&(f, t)| f == from && t == to)
}

/// Findet einen Knoten im Plan anhand seiner ID.
fn find_node<'a>(plan: &'a Plan, id: &TaskId) -> Option<&'a PlanNode> {
    plan.nodes.iter().find(|n| &n.id == id)
}

/// Erkennt Zyklen via DFS (Tiefensuche) im Dependency-Graph.
///
/// # Description
/// Traversiert den Graph von `start` aus und prüft, ob `target` erreichbar ist.
/// Erreichbarkeit von `target` über `start` → `target` würde nach Hinzufügen
/// der Kante `child → parent` einen Zyklus erzeugen.
///
/// # Arguments
/// - `plan` (`&Plan`): aktueller Plan.
/// - `start` (`&TaskId`): Startknoten der Traversierung (der neue Parent).
/// - `target` (`&TaskId`): gesuchter Knoten (der neue Child).
fn has_path(plan: &Plan, start: &TaskId, target: &TaskId) -> bool {
    let mut visited: HashSet<&TaskId> = HashSet::new();
    let mut stack = vec![start];
    while let Some(current) = stack.pop() {
        if current == target {
            return true;
        }
        if visited.contains(current) {
            continue;
        }
        visited.insert(current);
        if let Some(node) = find_node(plan, current) {
            for dep in &node.dependencies {
                stack.push(dep);
            }
        }
    }
    false
}

/// Prüft, ob Knoten `id` in `plan` vorhanden ist (Regel 1).
fn ensure_node_exists(plan: &Plan, id: &TaskId) -> PlanResult<()> {
    if find_node(plan, id).is_none() {
        return Err(PlanError::NodeMissing { id: id.clone() });
    }
    Ok(())
}

/// Weist leere oder ausschließlich aus Leerraum bestehende Rohwerte ab.
///
/// # Arguments
/// - `field` (`&'static str`): Feldname für die Fehlermeldung.
/// - `value` (`&str`): der zu prüfende Rohwert.
///
/// # Errors
/// - [`PlanError::InvalidId`]: wenn `value.trim()` leer ist.
fn ensure_not_blank(field: &'static str, value: &str) -> PlanResult<()> {
    if value.trim().is_empty() {
        return Err(PlanError::InvalidId {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// Prüft, ob sämtliche Abhängigkeiten eines Knotens existieren und
/// abgeschlossen sind (Regel 1 + Startbedingung).
///
/// # Errors
/// - [`PlanError::NodeMissing`]: eine Abhängigkeit existiert nicht.
/// - [`PlanError::DependencyNotCompleted`]: eine Abhängigkeit ist noch nicht
///   `Completed`.
fn ensure_dependencies_completed(plan: &Plan, node: &PlanNode) -> PlanResult<()> {
    for dependency_id in &node.dependencies {
        let dependency = find_node(plan, dependency_id).ok_or_else(|| PlanError::NodeMissing {
            id: dependency_id.clone(),
        })?;

        if dependency.status != PlanNodeStatus::Completed {
            return Err(PlanError::DependencyNotCompleted {
                id: node.id.clone(),
                dependency: dependency.id.clone(),
                status: format!("{:?}", dependency.status),
            });
        }
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Scope-Hilfsfunktionen (Regeln 5, 6, 10, 14)
// ──────────────────────────────────────────────────────────────────────────────

/// Regel 5: `forbidden_scope ∩ write_scope == ∅`.
///
/// # Errors
/// - [`PlanError::ForbiddenScopeOverlap`]: ein verbotener Pfad kollidiert mit
///   einem Schreibpfad (hierarchisch, nicht nur exakt).
fn ensure_forbidden_disjoint(
    write_scope: &[PathOrSymbol],
    forbidden_scope: &[PathOrSymbol],
) -> PlanResult<()> {
    for forbidden in forbidden_scope {
        if write_scope
            .iter()
            .any(|write| ScopeMatcher::conflicts(forbidden.as_str(), write.as_str()))
        {
            return Err(PlanError::ForbiddenScopeOverlap {
                path: forbidden.as_str().to_owned(),
            });
        }
    }
    Ok(())
}

/// Regel 6: `write_scope` muss disjunkt zu allen aktiven Knoten sein.
///
/// # Arguments
/// - `plan` (`&Plan`): aktueller Planzustand.
/// - `write_scope` (`&[PathOrSymbol]`): der zu prüfende Schreibbereich.
/// - `exclude` (`Option<&TaskId>`): Knoten, der von der Prüfung ausgenommen
///   wird — nötig bei `UpdateNode`, damit ein Knoten nicht mit sich selbst
///   kollidiert (Regel 14).
///
/// # Errors
/// - [`PlanError::ScopeConflict`]: ein aktiver Knoten beansprucht denselben
///   oder einen hierarchisch überlappenden Pfad.
fn ensure_write_scope_free(
    plan: &Plan,
    write_scope: &[PathOrSymbol],
    exclude: Option<&TaskId>,
) -> PlanResult<()> {
    for existing in &plan.nodes {
        if !ACTIVE_STATUSES.contains(&existing.status) {
            continue;
        }
        if exclude.is_some_and(|excluded| excluded == &existing.id) {
            continue;
        }
        for candidate in write_scope {
            for existing_scope in &existing.write_scope {
                if ScopeMatcher::conflicts(candidate.as_str(), existing_scope.as_str()) {
                    return Err(PlanError::ScopeConflict {
                        conflicting_path: candidate.as_str().to_owned(),
                        existing_node: existing.id.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Hierarchie-Hilfsfunktionen (Regeln 10, 11)
// ──────────────────────────────────────────────────────────────────────────────

/// Ermittelt die Expand-Tiefe eines Knotens über seine `parent`-Kette.
///
/// # Description
/// Ein Wurzelknoten (`parent == None`) hat Tiefe 0; jedes `Expand` erhöht die
/// Tiefe seiner Kinder um 1. Die Traversierung ist gegen zyklische
/// `parent`-Ketten in beschädigten Daten abgesichert (Besuchsmenge) und
/// terminiert deshalb immer.
///
/// # Returns
/// Die Länge der `parent`-Kette ab `id`.
fn expand_depth(plan: &Plan, id: &TaskId) -> u32 {
    let mut visited: HashSet<&TaskId> = HashSet::new();
    let mut depth: u32 = 0;
    let mut current = id;

    while let Some(node) = find_node(plan, current) {
        if !visited.insert(&node.id) {
            // Zyklische parent-Kette: abbrechen statt endlos laufen.
            break;
        }
        match node.parent.as_ref() {
            Some(parent) => {
                depth = depth.saturating_add(1);
                current = parent;
            }
            None => break,
        }
    }

    depth
}

/// Sammelt alle Knoten, deren `parent` auf `id` zeigt (Regel 11).
fn children_of<'a>(plan: &'a Plan, id: &TaskId) -> Vec<&'a PlanNode> {
    plan.nodes
        .iter()
        .filter(|node| node.parent.as_ref().is_some_and(|parent| parent == id))
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Explorations-Hilfsfunktionen (Regel 12)
// ──────────────────────────────────────────────────────────────────────────────

/// Prüft, ob ein Evidenz-Nachweis eine frische Exploration belegt (Regel 12).
///
/// # Description
/// Frisch ist ein `EvidenceKind::Finding`, dessen `attached_at` im Intervall
/// `[now - ttl_secs, now]` liegt. Ein Zeitstempel in der **Zukunft** gilt als
/// nicht frisch (F-013 §5.1 Punkt 3): sonst bliebe ein Nachweis dauerhaft
/// frisch. Der Store setzt `attached_at` selbst (`AttachEvidence`) bzw. kappt
/// Payload-Zeitstempel auf `now` (`AddNode`/`Expand`/`Condense`), legitime
/// Nachweise liegen deshalb nie in der Zukunft.
///
/// Crate-weit geteilt mit `graph::missing_explorations` (eine Frist-Semantik).
pub(crate) fn is_fresh_finding(
    evidence: &EvidenceRef,
    now: OffsetDateTime,
    ttl_secs: u64,
) -> bool {
    if evidence.kind != EvidenceKind::Finding || evidence.attached_at > now {
        return false;
    }
    // `u64` → `i64` kann nur bei absurd großen TTLs überlaufen; dann gilt der
    // Nachweis unbegrenzt als frisch, was der TTL-Intention entspricht.
    let ttl = i64::try_from(ttl_secs).unwrap_or(i64::MAX);
    (now - evidence.attached_at).whole_seconds() <= ttl
}

/// Einfügende Aktionen akzeptieren ausschließlich `Draft` (F-013 §5.1 Punkt 1).
///
/// # Errors
/// - [`PlanError::IllegalTransition`]: der Payload-Knoten trägt einen anderen
///   Status als `Draft`.
fn ensure_inserted_as_draft(node: &PlanNode) -> PlanResult<()> {
    if node.status != PlanNodeStatus::Draft {
        return Err(PlanError::IllegalTransition {
            id: node.id.clone(),
            from: "(neu)".to_owned(),
            to: format!("{:?} (neue Knoten beginnen als Draft)", node.status),
        });
    }
    Ok(())
}

/// Siegel- und Statusprüfung einer neuen Kante `child → parent`
/// (F-013 §5.1 Punkt 5).
///
/// # Errors
/// - [`PlanError::NodeSealed`]: `child` ist versiegelt.
/// - [`PlanError::IllegalTransition`]: `parent` ist `Superseded` und kann nie
///   abschließen.
/// - [`PlanError::DependencyNotCompleted`]: `child` ist aktiv
///   (`Ready`/`InProgress`), `parent` aber nicht `Completed`.
fn ensure_edge_allowed(child: &PlanNode, parent: &PlanNode) -> PlanResult<()> {
    if SEALED_STATUSES.contains(&child.status) {
        return Err(PlanError::NodeSealed {
            id: child.id.clone(),
        });
    }
    if parent.status == PlanNodeStatus::Superseded {
        return Err(PlanError::IllegalTransition {
            id: parent.id.clone(),
            from: format!("{:?}", parent.status),
            to: format!("Dependency von '{}'", child.id),
        });
    }
    if ACTIVE_STATUSES.contains(&child.status) && parent.status != PlanNodeStatus::Completed {
        return Err(PlanError::DependencyNotCompleted {
            id: child.id.clone(),
            dependency: parent.id.clone(),
            status: format!("{:?}", parent.status),
        });
    }
    Ok(())
}

/// Regel 12: Explore-before-implement.
///
/// # Description
/// Knoten, deren `kind` in `cfg.require_exploration_for` steht, dürfen erst
/// dann nach `Ready`/`InProgress` wechseln, wenn eine frische Exploration
/// vorliegt. Als Nachweis zählt entweder
/// - eine abgeschlossene Dependency mit `kind ∈ {Explore, Research}`, oder
/// - ein eigener `EvidenceRef` mit `kind == EvidenceKind::Finding`, der jünger
///   als `cfg.exploration_ttl_secs` ist.
///
/// # Arguments
/// - `plan` (`&Plan`): aktueller Planzustand (für die Dependency-Auflösung).
/// - `node` (`&PlanNode`): der zu startende Knoten — bei `AddNode` der noch
///   nicht eingefügte Kandidat.
/// - `cfg` (`&PlanToolConfig`): Regelkonfiguration.
/// - `now` (`OffsetDateTime`): Bezugszeitpunkt für die TTL-Prüfung.
///
/// # Errors
/// - [`PlanError::ExplorationRequired`]: keine frische Exploration vorhanden.
fn ensure_exploration_fresh(
    plan: &Plan,
    node: &PlanNode,
    cfg: &PlanToolConfig,
    now: OffsetDateTime,
) -> PlanResult<()> {
    if !cfg.require_exploration_for.contains(&node.kind) {
        return Ok(());
    }

    let has_completed_exploration_dependency = node.dependencies.iter().any(|dependency_id| {
        find_node(plan, dependency_id).is_some_and(|dependency| {
            EXPLORATION_KINDS.contains(&dependency.kind)
                && dependency.status == PlanNodeStatus::Completed
        })
    });
    if has_completed_exploration_dependency {
        return Ok(());
    }

    let has_fresh_finding = node
        .evidence
        .iter()
        .any(|evidence| is_fresh_finding(evidence, now, cfg.exploration_ttl_secs));
    if has_fresh_finding {
        return Ok(());
    }

    Err(PlanError::ExplorationRequired {
        id: node.id.clone(),
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// Öffentliche API
// ──────────────────────────────────────────────────────────────────────────────

/// Validiert eine Plan-Aktion gegen den aktuellen Planzustand.
///
/// # Description
/// Kompatibilitätsfassade über [`validate_with`] für Aufrufer ohne
/// Konfigurations- und Zeitkontext. Sie benutzt Default-Config-Semantik
/// (`PlanToolConfig::enabled_defaults()`) mit **leerer**
/// `require_exploration_for`-Liste, wodurch Regel 12 deaktiviert ist. Alle
/// übrigen 15 Regeln greifen unverändert.
///
/// Neue Aufrufer — insbesondere die Store-Implementierungen — sollen
/// [`validate_with`] verwenden, damit die Explorationspflicht durchgesetzt wird.
///
/// # Arguments
/// - `plan` (`&Plan`): aktueller Planzustand.
/// - `action` (`&PlanAction`): zu prüfende Aktion.
///
/// # Returns
/// `Ok(())` wenn die Aktion gültig ist.
///
/// # Errors
/// Gibt `Err(PlanError::*)` zurück, wenn eine Regel verletzt ist — siehe
/// [`validate_with`].
///
/// # Concurrency
/// Rein funktional, keine Seiteneffekte, keine Locks; aus mehreren Threads
/// gefahrlos aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::actions::PlanAction;
/// use harw_plan::validate::validate;
/// # fn demo(plan: &harw_plan::types::Plan) -> harw_plan::error::PlanResult<()> {
/// validate(plan, &PlanAction::Inspect)
/// # }
/// ```
pub fn validate(plan: &Plan, action: &PlanAction) -> PlanResult<()> {
    let cfg = compat_config();
    // Regel 12 ist in dieser Fassade deaktiviert, daher ist der Zeitpunkt
    // bedeutungslos — ein fester Wert hält die Fassade deterministisch.
    validate_with(plan, action, &cfg, OffsetDateTime::UNIX_EPOCH)
}

/// Liefert die Konfiguration der Kompatibilitätsfassade [`validate`].
///
/// Default-Semantik, aber ohne Explorationspflicht (Regel 12 aus).
fn compat_config() -> PlanToolConfig {
    PlanToolConfig {
        require_exploration_for: Vec::new(),
        ..PlanToolConfig::enabled_defaults()
    }
}

/// Validiert eine Plan-Aktion gegen Planzustand, Konfiguration und Zeitpunkt.
///
/// # Description
/// Führt alle 16 Validierungsregeln aus Design-Doc §5 aus (siehe
/// Modul-Dokumentation). `cfg` steuert die Regeln 10 (`max_expand_depth`) und
/// 12 (`require_exploration_for`, `exploration_ttl_secs`); `now` ist der
/// Bezugszeitpunkt der TTL-Prüfung aus Regel 12.
///
/// # Arguments
/// - `plan` (`&Plan`): aktueller Planzustand.
/// - `action` (`&PlanAction`): zu prüfende Aktion.
/// - `cfg` (`&PlanToolConfig`): Regelkonfiguration des Plan-Tools.
/// - `now` (`OffsetDateTime`): Bezugszeitpunkt (üblicherweise der Zeitstempel,
///   den der Store auch in das resultierende `PlanEvent` schreibt).
///
/// # Returns
/// `Ok(())` wenn die Aktion gültig ist.
///
/// # Errors
/// - [`PlanError::NodeMissing`]: referenzierter Knoten fehlt (Regel 1).
/// - [`PlanError::DuplicateNode`]: ID bereits vergeben (Regeln 1, 10, 16).
/// - [`PlanError::InvalidId`]: leerer/whitespace-only Pflichtwert oder
///   `Create` mit einer `PlanId`, die die Grammatik verletzt.
/// - [`PlanError::NodeSealed`]: `AddDependency`/Patch-Dependency auf einem
///   versiegelten Knoten.
/// - [`PlanError::CycleDetected`]: Kante würde einen Zyklus schließen (Regeln 2, 14).
/// - [`PlanError::IllegalTransition`]: Statuswechsel, Expand-Tiefe oder
///   Knotenart nicht erlaubt (Regeln 3, 10, 15, 16).
/// - [`PlanError::EvidenceMissing`]: `Completed` ohne Nachweis (Regel 4).
/// - [`PlanError::ForbiddenScopeOverlap`]: verbotener Pfad im Schreibbereich (Regeln 5, 14).
/// - [`PlanError::ScopeConflict`]: Schreibbereichs-Kollision (Regeln 6, 10, 14).
/// - [`PlanError::RevisionRegressed`]: Revision sinkt (Regel 7).
/// - [`PlanError::InvalidateCompleted`]: `Invalidate` auf `Completed` (Regel 8).
/// - [`PlanError::ExpandScopeEscapes`]: Kind-Scope außerhalb des Parents (Regel 10).
/// - [`PlanError::CompositeIncomplete`]: offene Kind-Knoten (Regel 11).
/// - [`PlanError::ExplorationRequired`]: fehlende frische Exploration (Regel 12).
/// - [`PlanError::NodeSealed`]: Patch auf versiegeltem Knoten (Regel 13).
///
/// # Concurrency
/// Rein funktional, keine Seiteneffekte, keine Locks; aus mehreren Threads
/// gefahrlos aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::actions::PlanAction;
/// use harw_plan::config::PlanToolConfig;
/// use harw_plan::validate::validate_with;
/// use time::OffsetDateTime;
/// # fn demo(plan: &harw_plan::types::Plan) -> harw_plan::error::PlanResult<()> {
/// let cfg = PlanToolConfig::enabled_defaults();
/// validate_with(plan, &PlanAction::Inspect, &cfg, OffsetDateTime::UNIX_EPOCH)
/// # }
/// ```
pub fn validate_with(
    plan: &Plan,
    action: &PlanAction,
    cfg: &PlanToolConfig,
    now: OffsetDateTime,
) -> PlanResult<()> {
    match action {
        PlanAction::Create { plan_id, .. } => validate_create(plan_id),

        PlanAction::AddNode { node } => validate_add_node(plan, node),

        PlanAction::UpdateNode { id, patch } => validate_update_node(plan, id, patch),

        PlanAction::AddDependency { child, parent } => validate_add_dependency(plan, child, parent),

        PlanAction::SetStatus { id, status, .. } => {
            validate_set_status(plan, id, *status, cfg, now)
        }

        PlanAction::AttachEvidence { id, evidence } => validate_attach_evidence(plan, id, evidence),

        PlanAction::Invalidate { ids, .. } => validate_invalidate(plan, ids),

        PlanAction::Supersede {
            new_parent_revision,
        } => validate_supersede(plan, *new_parent_revision),

        PlanAction::Expand { parent, children } => validate_expand(plan, parent, children, cfg),

        PlanAction::Condense {
            superseded,
            replacement,
            summary,
        } => validate_condense(plan, superseded, replacement, summary),

        PlanAction::Reopen { id, reason } => validate_reopen(plan, id, reason),

        PlanAction::BindGoal { goal_id } => validate_bind_goal(goal_id),

        PlanAction::Inspect => Ok(()),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Aktions-Validatoren
// ──────────────────────────────────────────────────────────────────────────────

/// Validiert [`PlanAction::Create`].
fn validate_create(plan_id: &PlanId) -> PlanResult<()> {
    // Regel 1: eine leere Plan-ID ist keine Identität.
    ensure_not_blank("plan_id", plan_id.as_str())?;
    // F-013/G-032: die ID wird Pfadsegment — Grammatik auch für per
    // `PlanId::new` erzeugte Werte erzwingen.
    PlanId::parse(plan_id.as_str()).map(|_| ())
}

/// Validiert [`PlanAction::AddNode`] (Regeln 1, 5, 6; nur `Draft`).
fn validate_add_node(plan: &Plan, node: &PlanNode) -> PlanResult<()> {
    // Regel 1: eine leere Task-ID ist keine Identität.
    ensure_not_blank("node.id", node.id.as_str())?;

    // Regel 1: IDs sind innerhalb eines Plans eindeutig.
    if find_node(plan, &node.id).is_some() {
        return Err(PlanError::DuplicateNode {
            id: node.id.clone(),
        });
    }

    // F-013 §5.1 Punkt 1: neue Knoten beginnen als Draft. Damit entfallen
    // Completed ohne Evidenz, eingeschleuste Terminalzustände und die
    // Umgehung von Dependency-/Explorationsregeln beim Einfügen — alle
    // weiteren Zustände führen über `SetStatus`.
    ensure_inserted_as_draft(node)?;

    // Regel 1: neue Knoten dürfen keine nicht vorhandenen Dependencies referenzieren.
    for dependency_id in &node.dependencies {
        let dependency = find_node(plan, dependency_id).ok_or_else(|| PlanError::NodeMissing {
            id: dependency_id.clone(),
        })?;
        ensure_edge_allowed(node, dependency)?;
    }

    // Draft-Knoten brauchen weder abgeschlossene Dependencies noch
    // Exploration (Regel 12 greift beim Übergang nach `Ready`).

    // Regel 5: forbidden_scope ∩ write_scope == ∅
    ensure_forbidden_disjoint(&node.write_scope, &node.forbidden_scope)?;

    // Regel 6: write_scope disjunkt zu aktiven Ready|InProgress-Nodes
    ensure_write_scope_free(plan, &node.write_scope, None)
}

/// Validiert [`PlanAction::UpdateNode`] (Regeln 13, 14).
fn validate_update_node(plan: &Plan, id: &TaskId, patch: &NodePatch) -> PlanResult<()> {
    // Regel 1: Knoten muss existieren.
    let node = find_node(plan, id).ok_or_else(|| PlanError::NodeMissing { id: id.clone() })?;

    // Regel 13: versiegelte Knoten sind unveränderlich. Der einzige Weg zurück
    // führt über `Reopen` (Regel 15) bzw. `Supersede` (Regel 7). Auch ein
    // leerer Patch wird abgewiesen: er signalisiert einen Aufrufer, der den
    // versiegelten Zustand nicht kennt.
    if SEALED_STATUSES.contains(&node.status) {
        return Err(PlanError::NodeSealed { id: id.clone() });
    }

    // Regel 14: Patch-Revalidierung der Scopes. Beide Listen werden auf den
    // resultierenden Knoten aufgelöst — ein Patch, der nur eine der beiden
    // Listen setzt, wird gegen die unveränderte andere geprüft.
    if patch.write_scope.is_some() || patch.forbidden_scope.is_some() {
        let write_scope = patch
            .write_scope
            .as_deref()
            .unwrap_or(node.write_scope.as_slice());
        let forbidden_scope = patch
            .forbidden_scope
            .as_deref()
            .unwrap_or(node.forbidden_scope.as_slice());

        // Regel 5 (erneut) auf dem resultierenden Knoten.
        ensure_forbidden_disjoint(write_scope, forbidden_scope)?;

        // Regel 6 (erneut) — der Knoten selbst ist ausgenommen, sonst würde er
        // mit seinem eigenen bisherigen Scope kollidieren.
        ensure_write_scope_free(plan, write_scope, Some(id))?;
    }

    // Regel 14: neue Dependencies müssen existieren (Regel 1) und dürfen
    // keinen Zyklus erzeugen (Regel 2). F-013 §5.1 Punkt 5: ein aktiver
    // Knoten bekommt nur abgeschlossene, keine abgelösten Dependencies.
    if let Some(dependencies) = patch.dependencies.as_deref() {
        for dependency_id in dependencies {
            let dependency =
                find_node(plan, dependency_id).ok_or_else(|| PlanError::NodeMissing {
                    id: dependency_id.clone(),
                })?;
            ensure_edge_allowed(node, dependency)?;

            // Die neue Kante lautet id→dependency_id. Ein Zyklus entsteht,
            // wenn `id` von `dependency_id` aus bereits erreichbar ist. Der
            // Selbstbezug (`dependency_id == id`) fällt hierunter.
            if has_path(plan, dependency_id, id) {
                return Err(PlanError::CycleDetected {
                    child: id.clone(),
                    parent: dependency_id.clone(),
                });
            }
        }
    }

    Ok(())
}

/// Validiert [`PlanAction::AddDependency`] (Regeln 1, 2; Siegel/Status).
fn validate_add_dependency(plan: &Plan, child: &TaskId, parent: &TaskId) -> PlanResult<()> {
    // Regel 1: beide Knoten müssen existieren
    let child_node =
        find_node(plan, child).ok_or_else(|| PlanError::NodeMissing { id: child.clone() })?;
    let parent_node =
        find_node(plan, parent).ok_or_else(|| PlanError::NodeMissing { id: parent.clone() })?;

    // F-013 §5.1 Punkt 5: Siegel- und Statusprüfung der neuen Kante. Sonst
    // bräche die Invariante „Ready/InProgress ⇒ Dependencies Completed“
    // nachträglich.
    ensure_edge_allowed(child_node, parent_node)?;

    // Regel 2: Zyklus-Erkennung via DFS
    // Die neue Kante lautet child→parent (child hängt zukünftig von parent ab).
    // Ein Zyklus entsteht, wenn parent (transitiv via bestehende Deps) bereits
    // vom child erreichbar ist — d. h. parent kann child bereits erreichen.
    if has_path(plan, parent, child) {
        return Err(PlanError::CycleDetected {
            child: child.clone(),
            parent: parent.clone(),
        });
    }
    Ok(())
}

/// Validiert [`PlanAction::SetStatus`] (Regeln 1, 3, 4, 11, 12, 15).
fn validate_set_status(
    plan: &Plan,
    id: &TaskId,
    status: PlanNodeStatus,
    cfg: &PlanToolConfig,
    now: OffsetDateTime,
) -> PlanResult<()> {
    // Regel 1: eine leere Task-ID ist keine Identität.
    ensure_not_blank("id", id.as_str())?;

    // Regel 1: Knoten muss existieren
    let node = find_node(plan, id).ok_or_else(|| PlanError::NodeMissing { id: id.clone() })?;

    // Regel 15: `Invalidated → Draft` steht in der Matrix, ist aber
    // reopen-exklusiv. `SetStatus` darf ihn nicht gehen, damit jede
    // Wiedereröffnung eine Begründung trägt.
    if is_reopen_only(node.status, status) {
        return Err(PlanError::IllegalTransition {
            id: id.clone(),
            from: format!("{:?}", node.status),
            to: format!("{status:?} (nur über Reopen erreichbar)"),
        });
    }

    // Regel 3: Statusübergang muss legal sein
    if !is_legal_transition(node.status, status) {
        return Err(PlanError::IllegalTransition {
            id: id.clone(),
            from: format!("{:?}", node.status),
            to: format!("{status:?}"),
        });
    }

    if status == PlanNodeStatus::Completed {
        // Regel 4: Completed verlangt mindestens ein EvidenceRef
        if node.evidence.is_empty() {
            return Err(PlanError::EvidenceMissing { id: id.clone() });
        }

        // Regel 11: Composite-Abschluss verlangt abgeschlossene Kinder.
        ensure_composite_children_completed(plan, node)?;
    }

    if ACTIVE_STATUSES.contains(&status) {
        ensure_dependencies_completed(plan, node)?;

        // Regel 6 auch beim Übergang (F-013 §5.1 Punkt 4): zwei Draft-Knoten
        // mit gleichem `write_scope` dürfen nicht beide aktiv werden. Der
        // Knoten selbst ist ausgenommen (`Ready → InProgress`).
        ensure_write_scope_free(plan, &node.write_scope, Some(id))?;
    }

    // Regel 12: Explore-before-implement beim Eintritt nach `Ready`.
    // `InProgress` ist nur aus `Ready` erreichbar und wurde dort geprüft.
    if status == PlanNodeStatus::Ready {
        ensure_exploration_fresh(plan, node, cfg, now)?;
    }

    Ok(())
}

/// Regel 11: Ein Composite-Knoten darf erst abschließen, wenn alle Kinder
/// abgeschlossen sind.
///
/// # Description
/// Greift für Knoten mit `kind == PlanNodeKind::Composite` **oder** für jeden
/// Knoten, auf den mindestens ein anderer Knoten per `parent` zeigt — die
/// zweite Bedingung fängt Knoten ab, die per `Expand` zerlegt wurden, ohne dass
/// ihr `kind` nachgeführt wurde.
///
/// Ein `Composite`-Knoten ohne Kinder hat keine offenen Kinder und darf
/// abschließen; ein leeres `open` würde eine sinnlose Fehlermeldung erzeugen.
///
/// # Errors
/// - [`PlanError::CompositeIncomplete`]: mindestens ein Kind ist nicht `Completed`.
fn ensure_composite_children_completed(plan: &Plan, node: &PlanNode) -> PlanResult<()> {
    let children = children_of(plan, &node.id);
    if node.kind != PlanNodeKind::Composite && children.is_empty() {
        return Ok(());
    }

    let open: Vec<TaskId> = children
        .iter()
        .filter(|child| child.status != PlanNodeStatus::Completed)
        .map(|child| child.id.clone())
        .collect();

    if !open.is_empty() {
        return Err(PlanError::CompositeIncomplete {
            id: node.id.clone(),
            open,
        });
    }

    Ok(())
}

/// Validiert [`PlanAction::AttachEvidence`] (Regeln 1, 9).
fn validate_attach_evidence(plan: &Plan, id: &TaskId, evidence: &EvidenceRef) -> PlanResult<()> {
    // Regel 1: Knoten muss existieren
    ensure_node_exists(plan, id)?;

    // Ein Nachweis ohne Lokator belegt nichts und ist nicht auffindbar.
    ensure_not_blank("evidence.locator", &evidence.locator)?;

    // Regel 9: Idempotenz — ein Duplikat (kind, locator) ist ausdrücklich kein
    // Fehler; der Store ignoriert es still.
    Ok(())
}

/// Validiert [`PlanAction::Invalidate`] (Regeln 1, 3, 8).
///
/// # Errors
/// - [`PlanError::NodeMissing`]: Knoten fehlt.
/// - [`PlanError::InvalidateCompleted`]: Knoten ist `Completed`.
/// - [`PlanError::IllegalTransition`]: Knoten ist `Superseded` oder bereits
///   `Invalidated` (F-013 §5.1 Punkt 2 — sonst wäre `Superseded` über
///   `Invalidate → Reopen` wiederbelebbar).
fn validate_invalidate(plan: &Plan, ids: &[TaskId]) -> PlanResult<()> {
    for id in ids {
        // Regel 1: Knoten muss existieren
        let node = find_node(plan, id).ok_or_else(|| PlanError::NodeMissing { id: id.clone() })?;

        // Regel 8: Completed-Knoten dürfen nicht invalidiert werden
        if node.status == PlanNodeStatus::Completed {
            return Err(PlanError::InvalidateCompleted { id: id.clone() });
        }

        // Regel 3: Invalidierung folgt der Status-Matrix.
        if !is_legal_transition(node.status, PlanNodeStatus::Invalidated) {
            return Err(PlanError::IllegalTransition {
                id: id.clone(),
                from: format!("{:?}", node.status),
                to: format!("{:?}", PlanNodeStatus::Invalidated),
            });
        }
    }
    Ok(())
}

/// Validiert [`PlanAction::Supersede`] (Regel 7).
fn validate_supersede(plan: &Plan, new_parent_revision: RevisionId) -> PlanResult<()> {
    // Regel 7: Revision muss monoton steigen
    if new_parent_revision <= plan.revision {
        return Err(PlanError::RevisionRegressed {
            current: plan.revision,
            attempted: new_parent_revision,
        });
    }
    Ok(())
}

/// Validiert [`PlanAction::Expand`] (Regel 10, plus Regeln 1 und 5).
///
/// # Description
/// Reihenfolge der Prüfungen:
/// 1. Parent existiert und ist nicht versiegelt (`Completed`/`Superseded`).
/// 2. Die Verschachtelungstiefe der neuen Kinder überschreitet
///    `cfg.max_expand_depth` nicht.
/// 3. Je Kind: nicht-leere ID, ID im Plan und unter den Geschwistern frei,
///    jeder `write_scope`-Eintrag liegt in einem `write_scope`-Eintrag des
///    Parents, `forbidden_scope ∩ write_scope == ∅`.
/// 4. Die Kinder sind untereinander write-disjunkt.
///
/// Eine Disjunktheitsprüfung der Kinder gegen die übrigen aktiven Knoten des
/// Plans findet bewusst **nicht** statt: die Kind-Scopes liegen per
/// Konstruktion im Parent-Scope, der seine Regel-6-Prüfung beim Einfügen
/// bereits bestanden hat. Eine erneute Prüfung würde den Parent selbst als
/// Kollisionspartner melden.
///
/// # Errors
/// - [`PlanError::NodeMissing`]: Parent existiert nicht.
/// - [`PlanError::IllegalTransition`]: Parent ist versiegelt oder die
///   Expand-Tiefe überschreitet `cfg.max_expand_depth`.
/// - [`PlanError::InvalidId`]: leere Kind-ID.
/// - [`PlanError::DuplicateNode`]: Kind-ID bereits vergeben.
/// - [`PlanError::ExpandScopeEscapes`]: Kind-Scope außerhalb des Parent-Scopes.
/// - [`PlanError::ForbiddenScopeOverlap`]: verbotener Pfad im Kind-Schreibbereich.
/// - [`PlanError::ScopeConflict`]: zwei Kinder beanspruchen denselben Pfad.
fn validate_expand(
    plan: &Plan,
    parent_id: &TaskId,
    children: &[PlanNode],
    cfg: &PlanToolConfig,
) -> PlanResult<()> {
    // Regel 1: Parent muss existieren.
    let parent = find_node(plan, parent_id).ok_or_else(|| PlanError::NodeMissing {
        id: parent_id.clone(),
    })?;

    // Regel 10: ein abgeschlossener oder abgelöster Knoten wird nicht mehr
    // zerlegt. `NodeSealed` bleibt laut Fehler-Doku dem `UpdateNode`-Patch
    // vorbehalten, daher wird hier die allgemeine Übergangs-Ablehnung genutzt.
    if matches!(
        parent.status,
        PlanNodeStatus::Completed | PlanNodeStatus::Superseded
    ) {
        return Err(PlanError::IllegalTransition {
            id: parent_id.clone(),
            from: format!("{:?}", parent.status),
            to: "Composite (Expand)".to_owned(),
        });
    }

    // Regel 10: Verschachtelungstiefe begrenzen. Die Kinder liegen eine Ebene
    // unter dem Parent.
    let child_depth = expand_depth(plan, parent_id).saturating_add(1);
    if child_depth > cfg.max_expand_depth {
        return Err(PlanError::IllegalTransition {
            id: parent_id.clone(),
            from: format!("Expand-Tiefe {}", child_depth.saturating_sub(1)),
            to: format!(
                "Expand-Tiefe {child_depth} (max_expand_depth={})",
                cfg.max_expand_depth
            ),
        });
    }

    let mut seen_child_ids: HashSet<&TaskId> = HashSet::new();
    for child in children {
        // Regel 1: eine leere Task-ID ist keine Identität.
        ensure_not_blank("child.id", child.id.as_str())?;

        // F-013 §5.1 Punkt 1: Kinder beginnen als Draft.
        ensure_inserted_as_draft(child)?;

        // Regel 10: Kind-IDs dürfen weder im Plan noch unter den Geschwistern
        // bereits vergeben sein.
        if find_node(plan, &child.id).is_some() || !seen_child_ids.insert(&child.id) {
            return Err(PlanError::DuplicateNode {
                id: child.id.clone(),
            });
        }

        // Regel 10: jeder Kind-Schreibpfad muss innerhalb eines
        // Parent-Schreibpfads liegen.
        for path in &child.write_scope {
            let contained = parent
                .write_scope
                .iter()
                .any(|parent_scope| ScopeMatcher::contains(parent_scope.as_str(), path.as_str()));
            if !contained {
                return Err(PlanError::ExpandScopeEscapes {
                    child: child.id.clone(),
                    path: path.as_str().to_owned(),
                });
            }
        }

        // Regel 5 (erneut): sonst wäre `Expand` ein Schlupfloch, um einen
        // Knoten mit widersprüchlichen Scopes in den Plan zu bekommen.
        ensure_forbidden_disjoint(&child.write_scope, &child.forbidden_scope)?;
    }

    // Regel 10: Kinder untereinander write-disjunkt.
    for (index, child) in children.iter().enumerate() {
        for sibling in &children[index.saturating_add(1)..] {
            for path in &child.write_scope {
                for sibling_path in &sibling.write_scope {
                    if ScopeMatcher::conflicts(path.as_str(), sibling_path.as_str()) {
                        return Err(PlanError::ScopeConflict {
                            conflicting_path: path.as_str().to_owned(),
                            existing_node: sibling.id.clone(),
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

/// Validiert [`PlanAction::Condense`] (Regel 16, plus Regeln 5 und 6).
///
/// # Description
/// Alle `superseded`-Knoten müssen existieren, `Completed` sein und eine
/// verdichtbare Art tragen (`Research`, `Explore`, `Analysis`). Der
/// `replacement`-Knoten muss eine freie, nicht-leere ID sowie
/// `kind == PlanNodeKind::Contract` haben; `summary` darf nicht leer sein.
///
/// Zusätzlich werden auf `replacement` die Regeln 5 und 6 angewandt: `Condense`
/// erzeugt einen neuen Knoten und darf deshalb kein Schlupfloch an der
/// Scope-Prüfung von `AddNode` vorbei sein.
///
/// # Errors
/// - [`PlanError::InvalidId`]: leere `summary` oder leere `replacement.id`.
/// - [`PlanError::NodeMissing`]: ein `superseded`-Knoten existiert nicht.
/// - [`PlanError::IllegalTransition`]: ein `superseded`-Knoten ist nicht
///   `Completed` oder trägt eine nicht verdichtbare Art; oder
///   `replacement.kind != Contract`.
/// - [`PlanError::DuplicateNode`]: `replacement.id` ist bereits vergeben.
/// - [`PlanError::ForbiddenScopeOverlap`] / [`PlanError::ScopeConflict`]:
///   Scope-Verletzung des `replacement`-Knotens.
fn validate_condense(
    plan: &Plan,
    superseded: &[TaskId],
    replacement: &PlanNode,
    summary: &str,
) -> PlanResult<()> {
    // Regel 16: eine Verdichtung ohne Zusammenfassung verliert genau die
    // Information, um derentwillen verdichtet wird.
    ensure_not_blank("summary", summary)?;

    for id in superseded {
        // Regel 1: Knoten muss existieren.
        let node = find_node(plan, id).ok_or_else(|| PlanError::NodeMissing { id: id.clone() })?;

        // Regel 16: nur abgeschlossene Knoten sind verdichtbar.
        if node.status != PlanNodeStatus::Completed {
            return Err(PlanError::IllegalTransition {
                id: id.clone(),
                from: format!("{:?}", node.status),
                to: "Superseded (Condense verlangt Completed)".to_owned(),
            });
        }

        // Regel 16: nur Research/Explore/Analysis sind verdichtbar.
        if !CONDENSABLE_KINDS.contains(&node.kind) {
            return Err(PlanError::IllegalTransition {
                id: id.clone(),
                from: format!("Completed (kind={:?})", node.kind),
                to: "Superseded (Condense verlangt Research/Explore/Analysis)".to_owned(),
            });
        }
    }

    // Regel 16: der Ersatzknoten ist neu.
    ensure_not_blank("replacement.id", replacement.id.as_str())?;
    // F-013 §5.1 Punkt 1: der Ersatzknoten beginnt als Draft.
    ensure_inserted_as_draft(replacement)?;
    if find_node(plan, &replacement.id).is_some() {
        return Err(PlanError::DuplicateNode {
            id: replacement.id.clone(),
        });
    }

    // Regel 16: die Verdichtung mündet in einen Vertrag.
    if replacement.kind != PlanNodeKind::Contract {
        return Err(PlanError::IllegalTransition {
            id: replacement.id.clone(),
            from: format!("kind={:?}", replacement.kind),
            to: format!("kind={:?}", PlanNodeKind::Contract),
        });
    }

    // Regeln 5 und 6 auf dem neuen Knoten — `Condense` legt einen Knoten an und
    // unterliegt denselben Scope-Grenzen wie `AddNode`.
    ensure_forbidden_disjoint(&replacement.write_scope, &replacement.forbidden_scope)?;
    ensure_write_scope_free(plan, &replacement.write_scope, None)
}

/// Validiert [`PlanAction::Reopen`] (Regel 15).
///
/// # Errors
/// - [`PlanError::InvalidId`]: leere ID oder leere Begründung.
/// - [`PlanError::NodeMissing`]: Knoten existiert nicht.
/// - [`PlanError::IllegalTransition`]: der Knoten ist nicht `Invalidated`.
fn validate_reopen(plan: &Plan, id: &TaskId, reason: &str) -> PlanResult<()> {
    ensure_not_blank("id", id.as_str())?;

    // Regel 15: eine Wiedereröffnung ohne Begründung ist nicht nachvollziehbar.
    ensure_not_blank("reason", reason)?;

    // Regel 1: Knoten muss existieren.
    let node = find_node(plan, id).ok_or_else(|| PlanError::NodeMissing { id: id.clone() })?;

    // Regel 15: ausschließlich `Invalidated → Draft`. Die Matrix bleibt die
    // einzige Quelle der Wahrheit für die Kante selbst.
    if node.status != PlanNodeStatus::Invalidated
        || !is_legal_transition(node.status, PlanNodeStatus::Draft)
    {
        return Err(PlanError::IllegalTransition {
            id: id.clone(),
            from: format!("{:?}", node.status),
            to: format!("{:?} (Reopen nur aus Invalidated)", PlanNodeStatus::Draft),
        });
    }

    Ok(())
}

/// Validiert [`PlanAction::BindGoal`].
fn validate_bind_goal(goal_id: &str) -> PlanResult<()> {
    // Regel 1: eine leere Goal-ID bindet an nichts.
    ensure_not_blank("goal_id", goal_id)
}

#[cfg(test)]
mod tests {
    // `use super::*` bringt bereits `Plan`, `PlanNode`, `PlanNodeStatus`,
    // `PlanNodeKind`, `EvidenceKind`, `EvidenceRef`, alle IDs, `PlanToolConfig`,
    // `PlanError`, `PlanResult` und `OffsetDateTime` mit; hier folgt nur, was
    // dort nicht sichtbar ist.
    use super::*;
    use crate::types::InvalidationCondition;
    use time::Duration;

    // ── Fixtures ─────────────────────────────────────────────────────────────

    fn make_plan(nodes: Vec<PlanNode>) -> Plan {
        Plan {
            id: PlanId::new("p-test"),
            revision: RevisionId::new(1),
            parent_revision: None,
            goal_statement: "Test".to_owned(),
            goal_id: None,
            nodes,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn make_node(id: &str, status: PlanNodeStatus) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
            objective: "obj".to_owned(),
            dependencies: vec![],
            input_contracts: vec![],
            output_contracts: vec![],
            read_scope: vec![],
            write_scope: vec![PathOrSymbol::new(format!("src/{id}.rs"))],
            forbidden_scope: vec![],
            acceptance_criteria: vec![],
            invalidation_conditions: vec![],
            status,
            evidence: vec![],
            kind: PlanNodeKind::Coding,
            wave: None,
            assignment: None,
            parent: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn make_kind_node(id: &str, status: PlanNodeStatus, kind: PlanNodeKind) -> PlanNode {
        PlanNode {
            kind,
            ..make_node(id, status)
        }
    }

    fn make_evidence() -> EvidenceRef {
        EvidenceRef {
            kind: EvidenceKind::CargoTest,
            locator: "test-run-001".to_owned(),
            attached_at: OffsetDateTime::UNIX_EPOCH,
            actor: "ci".to_owned(),
            digest: None,
        }
    }

    fn make_finding(attached_at: OffsetDateTime) -> EvidenceRef {
        EvidenceRef {
            kind: EvidenceKind::Finding,
            locator: "finding-001".to_owned(),
            attached_at,
            actor: "explorer".to_owned(),
            digest: None,
        }
    }

    /// Fester Bezugszeitpunkt für die TTL-Prüfung aus Regel 12.
    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(10)
    }

    /// Konfiguration mit explizit gesetzten Regel-10/12-Parametern, damit die
    /// Tests unabhängig von den Defaults aus `PlanToolConfig` bleiben.
    fn cfg_with(
        require_exploration_for: Vec<PlanNodeKind>,
        exploration_ttl_secs: u64,
        max_expand_depth: u32,
    ) -> PlanToolConfig {
        PlanToolConfig {
            require_exploration_for,
            exploration_ttl_secs,
            max_expand_depth,
            ..PlanToolConfig::enabled_defaults()
        }
    }

    /// Konfiguration ohne Explorationspflicht, großzügige Expand-Tiefe.
    fn permissive_cfg() -> PlanToolConfig {
        cfg_with(Vec::new(), 3_600, 8)
    }

    /// Kurzform für `validate_with` mit `permissive_cfg()` und `now()`.
    fn check(plan: &Plan, action: &PlanAction) -> PlanResult<()> {
        validate_with(plan, action, &permissive_cfg(), now())
    }

    // ── Regeln 1–9: bestehende Regeln ────────────────────────────────────────

    #[test]
    fn test_node_missing() {
        let plan = make_plan(vec![]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("nonexistent"),
            status: PlanNodeStatus::Ready,
            reason: None,
        };
        let err = validate(&plan, &action).unwrap_err();
        assert!(
            matches!(err, PlanError::NodeMissing { .. }),
            "Erwartet NodeMissing"
        );
    }

    #[test]
    fn test_cycle_detected() {
        // A hängt von B ab; dann B→A würde Zyklus erzeugen
        let mut node_a = make_node("A", PlanNodeStatus::Draft);
        node_a.dependencies = vec![TaskId::new("B")];
        let node_b = make_node("B", PlanNodeStatus::Draft);
        let plan = make_plan(vec![node_a, node_b]);

        let action = PlanAction::AddDependency {
            child: TaskId::new("B"),
            parent: TaskId::new("A"),
        };
        let err = validate(&plan, &action).unwrap_err();
        assert!(
            matches!(err, PlanError::CycleDetected { .. }),
            "Erwartet CycleDetected"
        );
    }

    #[test]
    fn test_illegal_transition() {
        // Draft → Completed ist nicht erlaubt
        let node = make_node("t1", PlanNodeStatus::Draft);
        let plan = make_plan(vec![node]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Completed,
            reason: None,
        };
        let err = validate(&plan, &action).unwrap_err();
        assert!(
            matches!(err, PlanError::IllegalTransition { .. }),
            "Erwartet IllegalTransition"
        );
    }

    #[test]
    fn test_evidence_missing_on_complete() {
        // InProgress → Completed ohne Evidence
        let node = make_node("t1", PlanNodeStatus::InProgress);
        let plan = make_plan(vec![node]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Completed,
            reason: None,
        };
        let err = validate(&plan, &action).unwrap_err();
        assert!(
            matches!(err, PlanError::EvidenceMissing { .. }),
            "Erwartet EvidenceMissing"
        );
    }

    #[test]
    fn test_scope_conflict() {
        let existing = make_node("A", PlanNodeStatus::Ready);
        let plan = make_plan(vec![existing]);

        let mut new_node = make_node("B", PlanNodeStatus::Draft);
        new_node.write_scope = vec![PathOrSymbol::new("src/A.rs")];
        let action = PlanAction::AddNode { node: new_node };
        let err = validate(&plan, &action).unwrap_err();
        assert!(
            matches!(err, PlanError::ScopeConflict { .. }),
            "Erwartet ScopeConflict"
        );
    }

    #[test]
    fn test_scope_conflict_when_new_scope_contains_active_scope() {
        let mut existing = make_node("A", PlanNodeStatus::Ready);
        existing.write_scope = vec![PathOrSymbol::new("src/validate.rs")];
        let plan = make_plan(vec![existing]);

        let mut new_node = make_node("B", PlanNodeStatus::Draft);
        new_node.write_scope = vec![PathOrSymbol::new("src")];
        let action = PlanAction::AddNode { node: new_node };

        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::ScopeConflict { .. })
            ),
            "ein Eltern-Scope muss mit dem aktiven Kind-Scope kollidieren"
        );
    }

    #[test]
    fn test_forbidden_scope_overlap() {
        let plan = make_plan(vec![]);
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        node.write_scope = vec![PathOrSymbol::new("src/secret.rs")];
        node.forbidden_scope = vec![PathOrSymbol::new("src/secret.rs")];
        let action = PlanAction::AddNode { node };
        let err = validate(&plan, &action).unwrap_err();
        assert!(
            matches!(err, PlanError::ForbiddenScopeOverlap { .. }),
            "Erwartet ForbiddenScopeOverlap"
        );
    }

    #[test]
    fn test_forbidden_scope_overlap_when_forbidden_scope_contains_write_scope() {
        let plan = make_plan(vec![]);
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        node.write_scope = vec![PathOrSymbol::new("src/secret.rs")];
        node.forbidden_scope = vec![PathOrSymbol::new("src")];
        let action = PlanAction::AddNode { node };

        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::ForbiddenScopeOverlap { .. })
            ),
            "ein verbotener Eltern-Scope muss mit dem Kind-Scope kollidieren"
        );
    }

    #[test]
    fn test_add_node_rejects_duplicate_id() {
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::AddNode {
            node: make_node("t1", PlanNodeStatus::Draft),
        };

        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::DuplicateNode { .. })
            ),
            "ein bereits vorhandener TaskId darf nicht erneut hinzugefügt werden"
        );
    }

    #[test]
    fn test_add_node_rejects_missing_dependency() {
        let plan = make_plan(vec![]);
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        node.dependencies = vec![TaskId::new("missing")];
        let action = PlanAction::AddNode { node };

        assert!(
            matches!(validate(&plan, &action), Err(PlanError::NodeMissing { .. })),
            "neue Nodes dürfen keine fehlenden Dependencies referenzieren"
        );
    }

    #[test]
    fn test_ready_transition_rejects_incomplete_dependency() {
        let dependency = make_node("dep", PlanNodeStatus::InProgress);
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        node.dependencies = vec![TaskId::new("dep")];
        let plan = make_plan(vec![dependency, node]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Ready,
            reason: None,
        };

        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::DependencyNotCompleted { .. })
            ),
            "Ready verlangt abgeschlossene Dependencies"
        );
    }

    #[test]
    fn test_in_progress_transition_rejects_incomplete_dependency() {
        let dependency = make_node("dep", PlanNodeStatus::Blocked);
        let mut node = make_node("t1", PlanNodeStatus::Ready);
        node.dependencies = vec![TaskId::new("dep")];
        let plan = make_plan(vec![dependency, node]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::InProgress,
            reason: None,
        };

        let err = validate(&plan, &action).unwrap_err();
        assert!(
            matches!(
                &err,
                PlanError::DependencyNotCompleted { id, dependency, status }
                    if id == &TaskId::new("t1")
                        && dependency == &TaskId::new("dep")
                        && status == "Blocked"
            ),
            "InProgress verlangt abgeschlossene Dependencies, err={err}"
        );
    }

    /// F-013 §5.1 Punkt 1: ein direkt als `Ready` eingefügter Knoten würde
    /// Dependency- und Explorationsregeln umgehen — er wird abgewiesen.
    #[test]
    fn test_add_ready_node_is_rejected_as_non_draft() {
        let dependency = make_node("dep", PlanNodeStatus::Draft);
        let plan = make_plan(vec![dependency]);
        let mut node = make_node("t1", PlanNodeStatus::Ready);
        node.dependencies = vec![TaskId::new("dep")];
        let action = PlanAction::AddNode { node };

        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::IllegalTransition { .. })
            ),
            "AddNode akzeptiert nur Draft"
        );
    }

    #[test]
    fn test_revision_regressed_on_supersede() {
        let plan = make_plan(vec![]); // revision = 1
        let action = PlanAction::Supersede {
            new_parent_revision: RevisionId::new(1),
        };
        let err = validate(&plan, &action).unwrap_err();
        assert!(
            matches!(err, PlanError::RevisionRegressed { .. }),
            "Erwartet RevisionRegressed"
        );
    }

    #[test]
    fn test_supersede_accepts_higher_revision() {
        let plan = make_plan(vec![]);
        let action = PlanAction::Supersede {
            new_parent_revision: RevisionId::new(2),
        };
        assert!(validate(&plan, &action).is_ok());
    }

    #[test]
    fn test_attach_evidence_idempotent() {
        let node = make_node("t1", PlanNodeStatus::InProgress);
        let plan = make_plan(vec![node]);
        let action = PlanAction::AttachEvidence {
            id: TaskId::new("t1"),
            evidence: make_evidence(),
        };
        assert!(
            validate(&plan, &action).is_ok(),
            "AttachEvidence muss idempotent Ok sein"
        );
        let action2 = PlanAction::AttachEvidence {
            id: TaskId::new("t1"),
            evidence: make_evidence(),
        };
        assert!(
            validate(&plan, &action2).is_ok(),
            "Zweites AttachEvidence muss Ok sein"
        );
    }

    #[test]
    fn test_invalidate_completed_node_fails() {
        let mut node = make_node("t1", PlanNodeStatus::Completed);
        node.evidence = vec![make_evidence()];
        let plan = make_plan(vec![node]);
        let action = PlanAction::Invalidate {
            ids: vec![TaskId::new("t1")],
            condition: InvalidationCondition::ManualInvalidate,
        };
        let err = validate(&plan, &action).unwrap_err();
        assert!(
            matches!(err, PlanError::InvalidateCompleted { .. }),
            "Erwartet InvalidateCompleted"
        );
    }

    // ── ID-Hygiene (InvalidId) ───────────────────────────────────────────────

    #[test]
    fn test_create_rejects_blank_plan_id() {
        let plan = make_plan(vec![]);
        let action = PlanAction::Create {
            plan_id: PlanId::new("   "),
            goal: "Ziel".to_owned(),
        };
        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::InvalidId {
                    field: "plan_id",
                    ..
                })
            ),
            "leere Plan-ID muss InvalidId liefern"
        );
    }

    #[test]
    fn test_create_accepts_non_blank_plan_id() {
        let plan = make_plan(vec![]);
        let action = PlanAction::Create {
            plan_id: PlanId::new("p-1"),
            goal: "Ziel".to_owned(),
        };
        assert!(validate(&plan, &action).is_ok());
    }

    #[test]
    fn test_add_node_rejects_blank_id() {
        let plan = make_plan(vec![]);
        let action = PlanAction::AddNode {
            node: make_node("", PlanNodeStatus::Draft),
        };
        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::InvalidId {
                    field: "node.id",
                    ..
                })
            ),
            "leere Task-ID muss InvalidId liefern"
        );
    }

    #[test]
    fn test_set_status_rejects_blank_id() {
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("\t "),
            status: PlanNodeStatus::Ready,
            reason: None,
        };
        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::InvalidId { field: "id", .. })
            ),
            "whitespace-only Task-ID muss InvalidId liefern (nicht NodeMissing)"
        );
    }

    #[test]
    fn test_attach_evidence_rejects_blank_locator() {
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::InProgress)]);
        let action = PlanAction::AttachEvidence {
            id: TaskId::new("t1"),
            evidence: EvidenceRef {
                locator: "  ".to_owned(),
                ..make_evidence()
            },
        };
        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::InvalidId {
                    field: "evidence.locator",
                    ..
                })
            ),
            "leerer Lokator muss InvalidId liefern"
        );
    }

    #[test]
    fn test_bind_goal_rejects_blank_goal_id() {
        let plan = make_plan(vec![]);
        let action = PlanAction::BindGoal {
            goal_id: String::new(),
        };
        assert!(matches!(
            validate(&plan, &action),
            Err(PlanError::InvalidId {
                field: "goal_id",
                ..
            })
        ));
    }

    #[test]
    fn test_bind_goal_accepts_goal_id() {
        let plan = make_plan(vec![]);
        let action = PlanAction::BindGoal {
            goal_id: "g-1".to_owned(),
        };
        assert!(validate(&plan, &action).is_ok());
    }

    #[test]
    fn test_inspect_is_always_ok() {
        let plan = make_plan(vec![]);
        assert!(validate(&plan, &PlanAction::Inspect).is_ok());
    }

    // ── Regel 10: Expand ─────────────────────────────────────────────────────

    /// Parent mit einem Verzeichnis-Scope, in dem die Kinder Platz finden.
    fn expand_parent() -> PlanNode {
        let mut parent = make_kind_node("parent", PlanNodeStatus::Draft, PlanNodeKind::Composite);
        parent.write_scope = vec![PathOrSymbol::new("src/feature")];
        parent
    }

    fn expand_child(id: &str, path: &str) -> PlanNode {
        let mut child = make_node(id, PlanNodeStatus::Draft);
        child.write_scope = vec![PathOrSymbol::new(path)];
        child.parent = Some(TaskId::new("parent"));
        child
    }

    #[test]
    fn test_expand_accepts_children_within_parent_scope() {
        let plan = make_plan(vec![expand_parent()]);
        let action = PlanAction::Expand {
            parent: TaskId::new("parent"),
            children: vec![
                expand_child("c1", "src/feature/a.rs"),
                expand_child("c2", "src/feature/b.rs"),
            ],
        };
        assert!(
            check(&plan, &action).is_ok(),
            "Kinder innerhalb des Parent-Scopes müssen akzeptiert werden"
        );
    }

    #[test]
    fn test_expand_rejects_child_scope_outside_parent() {
        let plan = make_plan(vec![expand_parent()]);
        let action = PlanAction::Expand {
            parent: TaskId::new("parent"),
            children: vec![expand_child("c1", "src/other.rs")],
        };
        let err = check(&plan, &action).unwrap_err();
        assert!(
            matches!(
                &err,
                PlanError::ExpandScopeEscapes { child, path }
                    if child == &TaskId::new("c1") && path == "src/other.rs"
            ),
            "Kind-Scope außerhalb des Parents muss ExpandScopeEscapes liefern, err={err}"
        );
    }

    #[test]
    fn test_expand_rejects_missing_parent() {
        let plan = make_plan(vec![]);
        let action = PlanAction::Expand {
            parent: TaskId::new("parent"),
            children: vec![expand_child("c1", "src/feature/a.rs")],
        };
        assert!(matches!(
            check(&plan, &action),
            Err(PlanError::NodeMissing { .. })
        ));
    }

    #[test]
    fn test_expand_rejects_completed_parent() {
        let mut parent = expand_parent();
        parent.status = PlanNodeStatus::Completed;
        parent.evidence = vec![make_evidence()];
        let plan = make_plan(vec![parent]);
        let action = PlanAction::Expand {
            parent: TaskId::new("parent"),
            children: vec![expand_child("c1", "src/feature/a.rs")],
        };
        assert!(
            matches!(
                check(&plan, &action),
                Err(PlanError::IllegalTransition { .. })
            ),
            "ein abgeschlossener Parent darf nicht mehr zerlegt werden"
        );
    }

    #[test]
    fn test_expand_rejects_existing_child_id() {
        let mut existing = make_node("c1", PlanNodeStatus::Draft);
        existing.write_scope = vec![PathOrSymbol::new("src/elsewhere.rs")];
        let plan = make_plan(vec![expand_parent(), existing]);
        let action = PlanAction::Expand {
            parent: TaskId::new("parent"),
            children: vec![expand_child("c1", "src/feature/a.rs")],
        };
        assert!(matches!(
            check(&plan, &action),
            Err(PlanError::DuplicateNode { .. })
        ));
    }

    #[test]
    fn test_expand_rejects_duplicate_child_ids_among_siblings() {
        let plan = make_plan(vec![expand_parent()]);
        let action = PlanAction::Expand {
            parent: TaskId::new("parent"),
            children: vec![
                expand_child("c1", "src/feature/a.rs"),
                expand_child("c1", "src/feature/b.rs"),
            ],
        };
        assert!(matches!(
            check(&plan, &action),
            Err(PlanError::DuplicateNode { .. })
        ));
    }

    #[test]
    fn test_expand_rejects_overlapping_children() {
        let plan = make_plan(vec![expand_parent()]);
        let action = PlanAction::Expand {
            parent: TaskId::new("parent"),
            children: vec![
                expand_child("c1", "src/feature"),
                expand_child("c2", "src/feature/b.rs"),
            ],
        };
        assert!(
            matches!(check(&plan, &action), Err(PlanError::ScopeConflict { .. })),
            "Geschwister müssen write-disjunkt sein"
        );
    }

    #[test]
    fn test_expand_rejects_child_with_forbidden_write_overlap() {
        let plan = make_plan(vec![expand_parent()]);
        let mut child = expand_child("c1", "src/feature/a.rs");
        child.forbidden_scope = vec![PathOrSymbol::new("src/feature")];
        let action = PlanAction::Expand {
            parent: TaskId::new("parent"),
            children: vec![child],
        };
        assert!(matches!(
            check(&plan, &action),
            Err(PlanError::ForbiddenScopeOverlap { .. })
        ));
    }

    #[test]
    fn test_expand_rejects_depth_beyond_max() {
        // root → parent → (neue Kinder) ⇒ Kind-Tiefe 2.
        let mut root = make_kind_node("root", PlanNodeStatus::Draft, PlanNodeKind::Composite);
        root.write_scope = vec![PathOrSymbol::new("src")];
        let mut parent = expand_parent();
        parent.parent = Some(TaskId::new("root"));
        let plan = make_plan(vec![root, parent]);

        let action = PlanAction::Expand {
            parent: TaskId::new("parent"),
            children: vec![expand_child("c1", "src/feature/a.rs")],
        };

        let cfg = cfg_with(Vec::new(), 3_600, 1);
        assert!(
            matches!(
                validate_with(&plan, &action, &cfg, now()),
                Err(PlanError::IllegalTransition { .. })
            ),
            "Kind-Tiefe 2 muss bei max_expand_depth=1 abgelehnt werden"
        );

        let cfg_ok = cfg_with(Vec::new(), 3_600, 2);
        assert!(
            validate_with(&plan, &action, &cfg_ok, now()).is_ok(),
            "Kind-Tiefe 2 muss bei max_expand_depth=2 akzeptiert werden"
        );
    }

    // ── Regel 11: Composite-Abschluss ────────────────────────────────────────

    #[test]
    fn test_composite_complete_rejects_open_children() {
        let mut parent = make_kind_node(
            "parent",
            PlanNodeStatus::InProgress,
            PlanNodeKind::Composite,
        );
        parent.evidence = vec![make_evidence()];
        let mut open_child = make_node("c1", PlanNodeStatus::InProgress);
        open_child.parent = Some(TaskId::new("parent"));
        let plan = make_plan(vec![parent, open_child]);

        let action = PlanAction::SetStatus {
            id: TaskId::new("parent"),
            status: PlanNodeStatus::Completed,
            reason: None,
        };

        let err = check(&plan, &action).unwrap_err();
        assert!(
            matches!(
                &err,
                PlanError::CompositeIncomplete { id, open }
                    if id == &TaskId::new("parent") && open == &vec![TaskId::new("c1")]
            ),
            "offene Kind-Knoten müssen CompositeIncomplete liefern, err={err}"
        );
    }

    #[test]
    fn test_composite_complete_accepts_all_children_completed() {
        let mut parent = make_kind_node(
            "parent",
            PlanNodeStatus::InProgress,
            PlanNodeKind::Composite,
        );
        parent.evidence = vec![make_evidence()];
        let mut child = make_node("c1", PlanNodeStatus::Completed);
        child.parent = Some(TaskId::new("parent"));
        child.evidence = vec![make_evidence()];
        let plan = make_plan(vec![parent, child]);

        let action = PlanAction::SetStatus {
            id: TaskId::new("parent"),
            status: PlanNodeStatus::Completed,
            reason: None,
        };
        assert!(check(&plan, &action).is_ok());
    }

    #[test]
    fn test_non_composite_with_open_children_also_rejected() {
        // Auch ein Knoten, dessen `kind` nach `Expand` nicht nachgeführt wurde,
        // darf nicht vor seinen Kindern abschließen.
        let mut parent = make_node("parent", PlanNodeStatus::InProgress);
        parent.evidence = vec![make_evidence()];
        let mut child = make_node("c1", PlanNodeStatus::Draft);
        child.parent = Some(TaskId::new("parent"));
        let plan = make_plan(vec![parent, child]);

        let action = PlanAction::SetStatus {
            id: TaskId::new("parent"),
            status: PlanNodeStatus::Completed,
            reason: None,
        };
        assert!(matches!(
            check(&plan, &action),
            Err(PlanError::CompositeIncomplete { .. })
        ));
    }

    #[test]
    fn test_childless_composite_may_complete() {
        let mut parent = make_kind_node(
            "parent",
            PlanNodeStatus::InProgress,
            PlanNodeKind::Composite,
        );
        parent.evidence = vec![make_evidence()];
        let plan = make_plan(vec![parent]);

        let action = PlanAction::SetStatus {
            id: TaskId::new("parent"),
            status: PlanNodeStatus::Completed,
            reason: None,
        };
        assert!(check(&plan, &action).is_ok());
    }

    // ── Regel 12: Explore-before-implement ───────────────────────────────────

    fn exploration_cfg() -> PlanToolConfig {
        cfg_with(vec![PlanNodeKind::Coding], 3_600, 8)
    }

    #[test]
    fn test_coding_node_without_exploration_is_rejected() {
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Ready,
            reason: None,
        };
        let err = validate_with(&plan, &action, &exploration_cfg(), now()).unwrap_err();
        assert!(
            matches!(&err, PlanError::ExplorationRequired { id } if id == &TaskId::new("t1")),
            "Coding-Knoten ohne Exploration muss abgelehnt werden, err={err}"
        );
    }

    #[test]
    fn test_coding_node_with_fresh_finding_is_accepted() {
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        node.evidence = vec![make_finding(now() - Duration::seconds(30))];
        let plan = make_plan(vec![node]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Ready,
            reason: None,
        };
        assert!(
            validate_with(&plan, &action, &exploration_cfg(), now()).is_ok(),
            "frisches Finding muss die Explorationspflicht erfüllen"
        );
    }

    #[test]
    fn test_coding_node_with_stale_finding_is_rejected() {
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        // TTL = 3600 s; das Finding ist 2 Tage alt.
        node.evidence = vec![make_finding(now() - Duration::days(2))];
        let plan = make_plan(vec![node]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Ready,
            reason: None,
        };
        assert!(
            matches!(
                validate_with(&plan, &action, &exploration_cfg(), now()),
                Err(PlanError::ExplorationRequired { .. })
            ),
            "abgelaufenes Finding darf die Explorationspflicht nicht erfüllen"
        );
    }

    #[test]
    fn test_coding_node_with_non_finding_evidence_is_rejected() {
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        // Frisch, aber falsche Art: `CargoTest` belegt keine Exploration.
        node.evidence = vec![EvidenceRef {
            attached_at: now(),
            ..make_evidence()
        }];
        let plan = make_plan(vec![node]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Ready,
            reason: None,
        };
        assert!(matches!(
            validate_with(&plan, &action, &exploration_cfg(), now()),
            Err(PlanError::ExplorationRequired { .. })
        ));
    }

    #[test]
    fn test_coding_node_with_completed_explore_dependency_is_accepted() {
        let mut explore =
            make_kind_node("explore", PlanNodeStatus::Completed, PlanNodeKind::Explore);
        explore.evidence = vec![make_evidence()];
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        node.dependencies = vec![TaskId::new("explore")];
        let plan = make_plan(vec![explore, node]);

        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Ready,
            reason: None,
        };
        assert!(
            validate_with(&plan, &action, &exploration_cfg(), now()).is_ok(),
            "abgeschlossene Explore-Dependency muss die Explorationspflicht erfüllen"
        );
    }

    #[test]
    fn test_coding_node_with_incomplete_explore_dependency_is_rejected() {
        let explore = make_kind_node("explore", PlanNodeStatus::InProgress, PlanNodeKind::Explore);
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        node.dependencies = vec![TaskId::new("explore")];
        let plan = make_plan(vec![explore, node]);

        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Ready,
            reason: None,
        };
        // Die Dependency-Regel greift bereits vor Regel 12.
        assert!(matches!(
            validate_with(&plan, &action, &exploration_cfg(), now()),
            Err(PlanError::DependencyNotCompleted { .. })
        ));
    }

    #[test]
    fn test_add_ready_coding_node_is_rejected_before_exploration_check() {
        let plan = make_plan(vec![]);
        let action = PlanAction::AddNode {
            node: make_node("t1", PlanNodeStatus::Ready),
        };
        assert!(matches!(
            validate_with(&plan, &action, &exploration_cfg(), now()),
            Err(PlanError::IllegalTransition { .. })
        ));
    }

    #[test]
    fn test_add_draft_coding_node_does_not_require_exploration() {
        let plan = make_plan(vec![]);
        let action = PlanAction::AddNode {
            node: make_node("t1", PlanNodeStatus::Draft),
        };
        assert!(
            validate_with(&plan, &action, &exploration_cfg(), now()).is_ok(),
            "Draft löst die Explorationspflicht noch nicht aus"
        );
    }

    #[test]
    fn test_node_kind_outside_require_list_skips_exploration() {
        let plan = make_plan(vec![make_kind_node(
            "t1",
            PlanNodeStatus::Draft,
            PlanNodeKind::Docs,
        )]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Ready,
            reason: None,
        };
        assert!(validate_with(&plan, &action, &exploration_cfg(), now()).is_ok());
    }

    #[test]
    fn test_compat_validate_skips_exploration_rule() {
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Ready,
            reason: None,
        };
        assert!(
            validate(&plan, &action).is_ok(),
            "die Kompatibilitätsfassade erzwingt Regel 12 nicht"
        );
    }

    // ── Regel 13: versiegelte Knoten ─────────────────────────────────────────

    #[test]
    fn test_update_node_on_completed_is_sealed() {
        let mut node = make_node("t1", PlanNodeStatus::Completed);
        node.evidence = vec![make_evidence()];
        let plan = make_plan(vec![node]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                objective: Some("neu".to_owned()),
                ..NodePatch::default()
            },
        };
        assert!(
            matches!(validate(&plan, &action), Err(PlanError::NodeSealed { .. })),
            "Patch auf Completed muss NodeSealed liefern"
        );
    }

    #[test]
    fn test_update_node_on_superseded_and_invalidated_is_sealed() {
        for status in [PlanNodeStatus::Superseded, PlanNodeStatus::Invalidated] {
            let plan = make_plan(vec![make_node("t1", status)]);
            let action = PlanAction::UpdateNode {
                id: TaskId::new("t1"),
                patch: NodePatch {
                    objective: Some("neu".to_owned()),
                    ..NodePatch::default()
                },
            };
            assert!(
                matches!(validate(&plan, &action), Err(PlanError::NodeSealed { .. })),
                "Patch auf {status:?} muss NodeSealed liefern"
            );
        }
    }

    #[test]
    fn test_update_node_on_draft_is_allowed() {
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                objective: Some("neu".to_owned()),
                ..NodePatch::default()
            },
        };
        assert!(validate(&plan, &action).is_ok());
    }

    #[test]
    fn test_update_node_rejects_missing_node() {
        let plan = make_plan(vec![]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch::default(),
        };
        assert!(matches!(
            validate(&plan, &action),
            Err(PlanError::NodeMissing { .. })
        ));
    }

    // ── Regel 14: Patch-Revalidierung ────────────────────────────────────────

    #[test]
    fn test_patch_write_scope_conflicts_with_active_node() {
        let mut active = make_node("A", PlanNodeStatus::Ready);
        active.write_scope = vec![PathOrSymbol::new("src/shared.rs")];
        let plan = make_plan(vec![active, make_node("t1", PlanNodeStatus::Draft)]);

        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                write_scope: Some(vec![PathOrSymbol::new("src/shared.rs")]),
                ..NodePatch::default()
            },
        };
        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::ScopeConflict { .. })
            ),
            "ein Patch darf nicht in den Scope eines aktiven Knotens greifen"
        );
    }

    #[test]
    fn test_patch_write_scope_ignores_own_node() {
        // Der Knoten ist selbst aktiv; sein eigener Scope darf ihn nicht blockieren.
        let mut node = make_node("t1", PlanNodeStatus::Ready);
        node.write_scope = vec![PathOrSymbol::new("src/t1.rs")];
        let plan = make_plan(vec![node]);

        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                write_scope: Some(vec![PathOrSymbol::new("src/t1.rs")]),
                ..NodePatch::default()
            },
        };
        assert!(
            validate(&plan, &action).is_ok(),
            "der Knoten darf nicht mit sich selbst kollidieren"
        );
    }

    #[test]
    fn test_patch_forbidden_scope_overlap() {
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        node.write_scope = vec![PathOrSymbol::new("src/secret.rs")];
        let plan = make_plan(vec![node]);

        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                forbidden_scope: Some(vec![PathOrSymbol::new("src")]),
                ..NodePatch::default()
            },
        };
        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::ForbiddenScopeOverlap { .. })
            ),
            "ein neu gesetzter forbidden_scope wird gegen den bestehenden write_scope geprüft"
        );
    }

    #[test]
    fn test_patch_dependencies_rejects_missing_node() {
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                dependencies: Some(vec![TaskId::new("missing")]),
                ..NodePatch::default()
            },
        };
        assert!(matches!(
            validate(&plan, &action),
            Err(PlanError::NodeMissing { .. })
        ));
    }

    #[test]
    fn test_patch_dependencies_rejects_cycle() {
        // B hängt von A ab; ein Patch, der A von B abhängen lässt, schließt den Zyklus.
        let node_a = make_node("A", PlanNodeStatus::Draft);
        let mut node_b = make_node("B", PlanNodeStatus::Draft);
        node_b.dependencies = vec![TaskId::new("A")];
        let plan = make_plan(vec![node_a, node_b]);

        let action = PlanAction::UpdateNode {
            id: TaskId::new("A"),
            patch: NodePatch {
                dependencies: Some(vec![TaskId::new("B")]),
                ..NodePatch::default()
            },
        };
        assert!(matches!(
            validate(&plan, &action),
            Err(PlanError::CycleDetected { .. })
        ));
    }

    #[test]
    fn test_patch_dependencies_rejects_self_reference() {
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                dependencies: Some(vec![TaskId::new("t1")]),
                ..NodePatch::default()
            },
        };
        assert!(matches!(
            validate(&plan, &action),
            Err(PlanError::CycleDetected { .. })
        ));
    }

    #[test]
    fn test_patch_dependencies_accepts_acyclic_edge() {
        let plan = make_plan(vec![
            make_node("A", PlanNodeStatus::Draft),
            make_node("B", PlanNodeStatus::Draft),
        ]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("A"),
            patch: NodePatch {
                dependencies: Some(vec![TaskId::new("B")]),
                ..NodePatch::default()
            },
        };
        assert!(validate(&plan, &action).is_ok());
    }

    // ── Regel 15: Reopen ─────────────────────────────────────────────────────

    #[test]
    fn test_reopen_from_invalidated_is_ok() {
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::Invalidated)]);
        let action = PlanAction::Reopen {
            id: TaskId::new("t1"),
            reason: "Contract geändert".to_owned(),
        };
        assert!(validate(&plan, &action).is_ok());
    }

    #[test]
    fn test_reopen_from_draft_is_rejected() {
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::Reopen {
            id: TaskId::new("t1"),
            reason: "warum auch immer".to_owned(),
        };
        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::IllegalTransition { .. })
            ),
            "Reopen ist nur aus Invalidated erlaubt"
        );
    }

    #[test]
    fn test_reopen_from_completed_is_rejected() {
        let mut node = make_node("t1", PlanNodeStatus::Completed);
        node.evidence = vec![make_evidence()];
        let plan = make_plan(vec![node]);
        let action = PlanAction::Reopen {
            id: TaskId::new("t1"),
            reason: "nochmal".to_owned(),
        };
        assert!(matches!(
            validate(&plan, &action),
            Err(PlanError::IllegalTransition { .. })
        ));
    }

    #[test]
    fn test_reopen_rejects_blank_reason() {
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::Invalidated)]);
        let action = PlanAction::Reopen {
            id: TaskId::new("t1"),
            reason: "   ".to_owned(),
        };
        assert!(matches!(
            validate(&plan, &action),
            Err(PlanError::InvalidId {
                field: "reason",
                ..
            })
        ));
    }

    #[test]
    fn test_reopen_rejects_missing_node() {
        let plan = make_plan(vec![]);
        let action = PlanAction::Reopen {
            id: TaskId::new("t1"),
            reason: "Grund".to_owned(),
        };
        assert!(matches!(
            validate(&plan, &action),
            Err(PlanError::NodeMissing { .. })
        ));
    }

    #[test]
    fn test_set_status_cannot_reopen_invalidated() {
        // Regel 15: `Invalidated → Draft` steht in der Matrix, ist aber
        // reopen-exklusiv.
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::Invalidated)]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Draft,
            reason: Some("Umweg".to_owned()),
        };
        let err = validate(&plan, &action).unwrap_err();
        assert!(
            matches!(&err, PlanError::IllegalTransition { to, .. } if to.contains("Reopen")),
            "SetStatus darf den Reopen-Übergang nicht gehen, err={err}"
        );
    }

    #[test]
    fn test_status_matrix_contains_reopen_edge() {
        assert!(
            is_legal_transition(PlanNodeStatus::Invalidated, PlanNodeStatus::Draft),
            "die Matrix muss den Reopen-Übergang enthalten"
        );
        assert!(is_reopen_only(
            PlanNodeStatus::Invalidated,
            PlanNodeStatus::Draft
        ));
        assert!(!is_reopen_only(
            PlanNodeStatus::Draft,
            PlanNodeStatus::Ready
        ));
    }

    // ── Regel 16: Condense ───────────────────────────────────────────────────

    fn condensable(id: &str, kind: PlanNodeKind) -> PlanNode {
        let mut node = make_kind_node(id, PlanNodeStatus::Completed, kind);
        node.evidence = vec![make_evidence()];
        node
    }

    fn replacement_contract() -> PlanNode {
        let mut node = make_kind_node("contract", PlanNodeStatus::Draft, PlanNodeKind::Contract);
        node.write_scope = vec![PathOrSymbol::new("docs/contract.md")];
        node
    }

    #[test]
    fn test_condense_ok() {
        let plan = make_plan(vec![
            condensable("r1", PlanNodeKind::Research),
            condensable("e1", PlanNodeKind::Explore),
            condensable("a1", PlanNodeKind::Analysis),
        ]);
        let action = PlanAction::Condense {
            superseded: vec![TaskId::new("r1"), TaskId::new("e1"), TaskId::new("a1")],
            replacement: replacement_contract(),
            summary: "Drei Recherchen zu einem Vertrag verdichtet.".to_owned(),
        };
        assert!(validate(&plan, &action).is_ok());
    }

    #[test]
    fn test_condense_rejects_blank_summary() {
        let plan = make_plan(vec![condensable("r1", PlanNodeKind::Research)]);
        let action = PlanAction::Condense {
            superseded: vec![TaskId::new("r1")],
            replacement: replacement_contract(),
            summary: "  ".to_owned(),
        };
        assert!(matches!(
            validate(&plan, &action),
            Err(PlanError::InvalidId {
                field: "summary",
                ..
            })
        ));
    }

    #[test]
    fn test_condense_rejects_missing_superseded() {
        let plan = make_plan(vec![]);
        let action = PlanAction::Condense {
            superseded: vec![TaskId::new("r1")],
            replacement: replacement_contract(),
            summary: "Zusammenfassung".to_owned(),
        };
        assert!(matches!(
            validate(&plan, &action),
            Err(PlanError::NodeMissing { .. })
        ));
    }

    #[test]
    fn test_condense_rejects_incomplete_superseded() {
        let plan = make_plan(vec![make_kind_node(
            "r1",
            PlanNodeStatus::InProgress,
            PlanNodeKind::Research,
        )]);
        let action = PlanAction::Condense {
            superseded: vec![TaskId::new("r1")],
            replacement: replacement_contract(),
            summary: "Zusammenfassung".to_owned(),
        };
        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::IllegalTransition { .. })
            ),
            "nur abgeschlossene Knoten sind verdichtbar"
        );
    }

    #[test]
    fn test_condense_rejects_wrong_superseded_kind() {
        let plan = make_plan(vec![condensable("c1", PlanNodeKind::Coding)]);
        let action = PlanAction::Condense {
            superseded: vec![TaskId::new("c1")],
            replacement: replacement_contract(),
            summary: "Zusammenfassung".to_owned(),
        };
        let err = validate(&plan, &action).unwrap_err();
        assert!(
            matches!(&err, PlanError::IllegalTransition { from, .. } if from.contains("Coding")),
            "ein Coding-Knoten ist nicht verdichtbar, err={err}"
        );
    }

    #[test]
    fn test_condense_rejects_existing_replacement_id() {
        let mut existing = make_node("contract", PlanNodeStatus::Draft);
        existing.write_scope = vec![PathOrSymbol::new("docs/other.md")];
        let plan = make_plan(vec![condensable("r1", PlanNodeKind::Research), existing]);
        let action = PlanAction::Condense {
            superseded: vec![TaskId::new("r1")],
            replacement: replacement_contract(),
            summary: "Zusammenfassung".to_owned(),
        };
        assert!(matches!(
            validate(&plan, &action),
            Err(PlanError::DuplicateNode { .. })
        ));
    }

    #[test]
    fn test_condense_rejects_non_contract_replacement() {
        let plan = make_plan(vec![condensable("r1", PlanNodeKind::Research)]);
        let mut replacement = replacement_contract();
        replacement.kind = PlanNodeKind::Coding;
        let action = PlanAction::Condense {
            superseded: vec![TaskId::new("r1")],
            replacement,
            summary: "Zusammenfassung".to_owned(),
        };
        let err = validate(&plan, &action).unwrap_err();
        assert!(
            matches!(&err, PlanError::IllegalTransition { to, .. } if to.contains("Contract")),
            "der Ersatzknoten muss ein Contract sein, err={err}"
        );
    }

    #[test]
    fn test_condense_rejects_replacement_scope_conflict() {
        let mut active = make_node("active", PlanNodeStatus::Ready);
        active.write_scope = vec![PathOrSymbol::new("docs")];
        let plan = make_plan(vec![condensable("r1", PlanNodeKind::Research), active]);
        let action = PlanAction::Condense {
            superseded: vec![TaskId::new("r1")],
            replacement: replacement_contract(),
            summary: "Zusammenfassung".to_owned(),
        };
        assert!(
            matches!(
                validate(&plan, &action),
                Err(PlanError::ScopeConflict { .. })
            ),
            "Condense darf die Scope-Prüfung nicht umgehen"
        );
    }

    // ── F-013 §5.1: geschlossene Validierungslücken (W3/C-PLAN) ─────────────

    /// Punkt 1: `Completed` ohne Evidenz lässt sich nicht einfügen.
    #[test]
    fn test_gap1_add_node_completed_without_evidence_is_rejected() {
        for status in [
            PlanNodeStatus::Completed,
            PlanNodeStatus::Superseded,
            PlanNodeStatus::Invalidated,
            PlanNodeStatus::InProgress,
            PlanNodeStatus::Blocked,
        ] {
            let result = check(
                &make_plan(vec![]),
                &PlanAction::AddNode {
                    node: make_node("t1", status),
                },
            );
            assert!(
                matches!(result, Err(PlanError::IllegalTransition { .. })),
                "{status:?} darf nicht eingefügt werden, war: {result:?}"
            );
        }
    }

    /// Punkt 1: auch `Expand`-Kinder und `Condense`-Ersatz beginnen als Draft.
    #[test]
    fn test_gap1_expand_child_and_condense_replacement_must_be_draft() {
        let plan = make_plan(vec![expand_parent()]);
        let mut child = expand_child("c1", "src/feature/a.rs");
        child.status = PlanNodeStatus::Completed;
        let expand = PlanAction::Expand {
            parent: TaskId::new("parent"),
            children: vec![child],
        };
        assert!(matches!(
            check(&plan, &expand),
            Err(PlanError::IllegalTransition { .. })
        ));

        let mut research = make_kind_node("r1", PlanNodeStatus::Completed, PlanNodeKind::Research);
        research.evidence = vec![make_evidence()];
        let plan = make_plan(vec![research]);
        let mut replacement =
            make_kind_node("c1", PlanNodeStatus::Completed, PlanNodeKind::Contract);
        replacement.evidence = vec![make_evidence()];
        let condense = PlanAction::Condense {
            superseded: vec![TaskId::new("r1")],
            replacement,
            summary: "Ergebnis".to_owned(),
        };
        assert!(matches!(
            check(&plan, &condense),
            Err(PlanError::IllegalTransition { .. })
        ));
    }

    /// Punkt 2: `Superseded` ist terminal — kein `Invalidate` (und damit kein
    /// `Reopen`) mehr; ebenso keine doppelte Invalidierung.
    #[test]
    fn test_gap2_invalidate_superseded_and_invalidated_is_rejected() {
        for status in [PlanNodeStatus::Superseded, PlanNodeStatus::Invalidated] {
            let plan = make_plan(vec![make_node("t1", status)]);
            let result = check(
                &plan,
                &PlanAction::Invalidate {
                    ids: vec![TaskId::new("t1")],
                    condition: InvalidationCondition::ManualInvalidate,
                },
            );
            assert!(
                matches!(result, Err(PlanError::IllegalTransition { .. })),
                "{status:?} darf nicht invalidiert werden, war: {result:?}"
            );
        }
    }

    /// Punkt 2: `Draft → Invalidated` steht jetzt explizit in der Matrix.
    #[test]
    fn test_gap2_invalidate_draft_follows_matrix() {
        assert!(is_legal_transition(
            PlanNodeStatus::Draft,
            PlanNodeStatus::Invalidated
        ));
        let plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        assert!(check(
            &plan,
            &PlanAction::Invalidate {
                ids: vec![TaskId::new("t1")],
                condition: InvalidationCondition::ManualInvalidate,
            }
        )
        .is_ok());
    }

    /// Punkt 3: ein Finding mit Zukunfts-Zeitstempel ist keine frische
    /// Exploration.
    #[test]
    fn test_gap3_future_finding_is_not_fresh() {
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        node.evidence = vec![make_finding(now() + Duration::days(3650))];
        let plan = make_plan(vec![node]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Ready,
            reason: None,
        };
        assert!(matches!(
            validate_with(&plan, &action, &exploration_cfg(), now()),
            Err(PlanError::ExplorationRequired { .. })
        ));
        assert!(!is_fresh_finding(
            &make_finding(now() + Duration::seconds(1)),
            now(),
            3_600
        ));
        assert!(is_fresh_finding(&make_finding(now()), now(), 3_600));
    }

    /// Regel 12 greift beim Eintritt nach `Ready`, nicht erneut bei
    /// `Ready → InProgress` (sonst hinge ein zugelassener Knoten nach
    /// Fristablauf fest).
    #[test]
    fn test_rule12_in_progress_after_ready_does_not_recheck_freshness() {
        let mut node = make_node("t1", PlanNodeStatus::Ready);
        node.evidence = vec![make_finding(now() - Duration::days(2))];
        let plan = make_plan(vec![node]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::InProgress,
            reason: None,
        };
        assert!(validate_with(&plan, &action, &exploration_cfg(), now()).is_ok());
    }

    /// Punkt 4: `SetStatus → Ready` prüft Regel 6 gegen aktive Knoten.
    #[test]
    fn test_gap4_set_status_ready_checks_write_scope_conflict() {
        let mut active = make_node("a", PlanNodeStatus::Ready);
        active.write_scope = vec![PathOrSymbol::new("src/shared.rs")];
        let mut draft = make_node("b", PlanNodeStatus::Draft);
        draft.write_scope = vec![PathOrSymbol::new("src/shared.rs")];
        let plan = make_plan(vec![active, draft]);

        let result = check(
            &plan,
            &PlanAction::SetStatus {
                id: TaskId::new("b"),
                status: PlanNodeStatus::Ready,
                reason: None,
            },
        );
        assert!(
            matches!(
                &result,
                Err(PlanError::ScopeConflict { existing_node, .. })
                    if existing_node == &TaskId::new("a")
            ),
            "war: {result:?}"
        );

        // Der Knoten kollidiert nicht mit sich selbst.
        let result = check(
            &plan,
            &PlanAction::SetStatus {
                id: TaskId::new("a"),
                status: PlanNodeStatus::InProgress,
                reason: None,
            },
        );
        assert!(result.is_ok(), "war: {result:?}");
    }

    /// Punkt 5: `AddDependency` prüft Siegel und Status.
    #[test]
    fn test_gap5_add_dependency_checks_seal_and_status() {
        let mut completed = make_node("done", PlanNodeStatus::Completed);
        completed.evidence = vec![make_evidence()];
        let plan = make_plan(vec![
            completed,
            make_node("draft", PlanNodeStatus::Draft),
            make_node("ready", PlanNodeStatus::Ready),
            make_node("old", PlanNodeStatus::Superseded),
        ]);
        let edge = |child: &str, parent: &str| PlanAction::AddDependency {
            child: TaskId::new(child),
            parent: TaskId::new(parent),
        };

        assert!(
            matches!(check(&plan, &edge("done", "draft")), Err(PlanError::NodeSealed { .. })),
            "versiegelter Kind-Knoten"
        );
        assert!(
            matches!(
                check(&plan, &edge("ready", "draft")),
                Err(PlanError::DependencyNotCompleted { .. })
            ),
            "aktiver Kind-Knoten mit offener Dependency"
        );
        assert!(
            matches!(
                check(&plan, &edge("draft", "old")),
                Err(PlanError::IllegalTransition { .. })
            ),
            "abgelöster Parent kann nie abschließen"
        );
        assert!(check(&plan, &edge("ready", "done")).is_ok());
        assert!(check(&plan, &edge("draft", "ready")).is_ok());
    }

    /// Punkt 5: dieselbe Prüfung für `UpdateNode.dependencies`.
    #[test]
    fn test_gap5_patch_dependencies_on_active_node_requires_completed() {
        let plan = make_plan(vec![
            make_node("ready", PlanNodeStatus::Ready),
            make_node("draft", PlanNodeStatus::Draft),
        ]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("ready"),
            patch: NodePatch {
                dependencies: Some(vec![TaskId::new("draft")]),
                ..NodePatch::default()
            },
        };
        assert!(matches!(
            check(&plan, &action),
            Err(PlanError::DependencyNotCompleted { .. })
        ));
    }

    /// F-013/G-032: `Create` mit Traversal-ID wird abgewiesen, auch wenn sie
    /// über `PlanId::new` gebaut wurde.
    #[test]
    fn test_create_rejects_traversal_plan_id() {
        for raw in ["../../x", "a/b", "p\u{202E}1", "P-1"] {
            let result = check(
                &make_plan(vec![]),
                &PlanAction::Create {
                    plan_id: PlanId::new(raw),
                    goal: "Ziel".to_owned(),
                },
            );
            assert!(
                matches!(result, Err(PlanError::InvalidId { field: "PlanId", .. })),
                "{raw:?} war: {result:?}"
            );
        }
    }
}
