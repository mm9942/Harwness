//! Fehlertypen für `harw-plan`.
//!
//! Verantwortungsbereich: Zentrales `PlanError`-Enum mit allen Fehlervarianten,
//! die aus Validation, Store-Operationen, Autorisierung oder I/O entstehen
//! können.
//!
//! `Display`, `std::error::Error` und die `From`-Konvertierungen werden nicht
//! mehr von Hand geschrieben, sondern von `#[derive(harw_macros::HarwError)]`
//! erzeugt (Muster: `harw-provider/src/error.rs`). Das Makro liefert außerdem
//! automatisch den `PlanResult<T>`-Typalias, weil der Enum-Name auf `Error`
//! endet — eine manuelle `pub type PlanResult<T> = …`-Definition entfällt.
//!
//! Exportierte Typen: [`PlanError`], [`PlanResult`], [`PlanToolConfigError`],
//! [`PlanToolConfigResult`].
//!
//! Kein `anyhow`, kein `thiserror`. Alle Konvertierungen entstehen über
//! `#[from]` auf dem jeweiligen Tupel-Feld.

use harw_macros::HarwError;

use crate::ids::{PlanId, RevisionId, TaskId};

/// Fehler beim Anwenden einer [`crate::config::PlanToolConfig`].
///
/// Dieser Typ hält Konfigurations- und Kapazitätsfehler getrennt von
/// [`PlanError`], damit Aufrufer die Tool-Grenze vor dem Aufruf eines Stores
/// ausdrücklich prüfen können.
#[derive(Debug, Clone, PartialEq, Eq, HarwError)]
pub enum PlanToolConfigError {
    /// Das Plan-Tool ist deaktiviert.
    #[msg("Plan-Tool ist deaktiviert")]
    Disabled,

    /// `max_nodes` darf nicht null sein.
    #[msg("Plan-Tool-Konfiguration hat max_nodes = 0")]
    ZeroMaxNodes,

    /// Ein Plan oder eine Aktion würde das konfigurierte Knotenlimit überschreiten.
    ///
    /// # Arguments
    /// - `max_nodes` (`usize`): konfiguriertes Limit.
    /// - `attempted_nodes` (`usize`): Anzahl der Knoten nach der geprüften Operation.
    #[msg("Plan-Knotenlimit überschritten: erlaubt={max_nodes}, versucht={attempted_nodes}")]
    NodeLimitExceeded {
        max_nodes: usize,
        attempted_nodes: usize,
    },
}

/// Alle möglichen Fehler in `harw-plan`.
///
/// # Description
/// Varianten tragen den vollständigen Kontext, der zum Verstehen des Fehlers
/// ohne Quellcode-Lektüre nötig ist (Design-Doc §5, coding-philosophy §Error
/// Handling). `IllegalTransition` bleibt allgemein für nicht erlaubte
/// Statuswechsel reserviert und wird nicht mehr für fehlende
/// Abhängigkeits-Abschlüsse missbraucht — dafür existiert
/// [`PlanError::DependencyNotCompleted`].
#[derive(Debug, HarwError)]
pub enum PlanError {
    /// Ein referenzierter Knoten existiert nicht im Plan.
    ///
    /// # Arguments
    /// - `id` (`TaskId`): fehlender Knoten.
    #[msg("Plan-Knoten '{id}' existiert nicht")]
    NodeMissing { id: TaskId },

    /// Eine `AddDependency`-Aktion würde einen Zyklus erzeugen.
    ///
    /// # Arguments
    /// - `child` / `parent` (`TaskId`): die Kante, die den Zyklus schließen würde.
    #[msg("Kante {child}→{parent} würde einen Zyklus im Dependency-Graph erzeugen")]
    CycleDetected { child: TaskId, parent: TaskId },

    /// Ein Statusübergang ist nicht erlaubt.
    ///
    /// # Arguments
    /// - `id` (`TaskId`): betroffener Knoten.
    /// - `from` / `to` (`String`): Ausgangs- und Zielstatus.
    #[msg("Statusübergang {from}->{to} für Knoten '{id}' ist nicht erlaubt")]
    IllegalTransition {
        id: TaskId,
        from: String,
        to: String,
    },

    /// `SetStatus(Completed)` verlangt mindestens einen `EvidenceRef`.
    ///
    /// # Arguments
    /// - `id` (`TaskId`): betroffener Knoten.
    #[msg("Knoten '{id}' kann nicht auf Completed gesetzt werden: kein EvidenceRef vorhanden")]
    EvidenceMissing { id: TaskId },

    /// `write_scope` eines neuen Knotens überschneidet sich mit aktiven Knoten.
    ///
    /// # Arguments
    /// - `conflicting_path` (`String`): der konfligierende Pfad/Symbol.
    /// - `existing_node` (`TaskId`): Knoten, der den Pfad bereits beansprucht.
    #[msg(
        "WriteScope-Konflikt: '{conflicting_path}' wird bereits von Knoten '{existing_node}' beansprucht"
    )]
    ScopeConflict {
        conflicting_path: String,
        existing_node: TaskId,
    },

    /// `forbidden_scope ∩ write_scope ≠ ∅` für einen neuen Knoten.
    ///
    /// # Arguments
    /// - `path` (`String`): der überlappende Pfad/Symbol.
    #[msg("Verbotener Scope '{path}' überschneidet sich mit write_scope")]
    ForbiddenScopeOverlap { path: String },

    /// Eine `Supersede`-Aktion senkt die Revision (muss monoton steigen).
    ///
    /// # Arguments
    /// - `current` / `attempted` (`RevisionId`): aktuelle und versuchte Revision.
    #[msg("Revision muss monoton steigen: aktuelle={current}, versucht={attempted}")]
    RevisionRegressed {
        current: RevisionId,
        attempted: RevisionId,
    },

    /// `Invalidate` wurde für einen abgeschlossenen Knoten versucht.
    ///
    /// # Arguments
    /// - `id` (`TaskId`): betroffener Knoten.
    #[msg("Knoten '{id}' ist Completed und kann nicht invalidiert werden (nur via Supersede)")]
    InvalidateCompleted { id: TaskId },

    /// `AddNode` mit einer ID, die im Plan bereits vergeben ist.
    ///
    /// # Auslöser
    /// `AddNode`-Aktion, deren `TaskId` bereits einem vorhandenen Knoten
    /// gehört. Zuvor wurde dieser Fall fälschlich über `ScopeConflict`
    /// gemeldet — das ist ein eigenständiger Fehlerfall.
    ///
    /// # Arguments
    /// - `id` (`TaskId`): die bereits vergebene ID.
    #[msg("Plan-Knoten '{id}' ist bereits vergeben")]
    DuplicateNode { id: TaskId },

    /// Eine übergebene ID ist leer, besteht nur aus Leerraum oder verletzt
    /// eine feste ID-Grammatik.
    ///
    /// # Auslöser
    /// Validierung einer neuen `TaskId`/`PlanId` (oder eines anderen
    /// ID-artigen Feldes), deren Rohwert nach dem Trimmen leer ist — oder bei
    /// [`PlanId`] nicht der Grammatik `^[a-z0-9][a-z0-9-]{0,63}$` entspricht
    /// (Pfad-Traversal-Schutz, F-013).
    ///
    /// # Arguments
    /// - `field` (`&'static str`): Name des betroffenen Feldes.
    /// - `value` (`String`): der ungültige Rohwert.
    #[msg(
        "Ungültiger Wert für '{field}': {value:?} (leer, nur Leerzeichen oder unzulässige Zeichen)"
    )]
    InvalidId { field: &'static str, value: String },

    /// `Create` mit einer Plan-ID, die im Store bereits vergeben ist.
    ///
    /// # Auslöser
    /// `Create` für eine ID, unter der schon ein Plan liegt (aktiv, inaktiv
    /// oder archiviert). Seit Runde 5, Teil P hält ein Store mehrere Pläne;
    /// ein `Create` mit **neuer** ID ist immer erlaubt und macht den neuen
    /// Plan aktiv. Nur die doppelte ID wird abgelehnt — mit dem Hinweis auf
    /// `switch`, denn meist ist genau das gemeint.
    ///
    /// # Arguments
    /// - `id` (`PlanId`): die bereits vergebene ID.
    #[msg(
        "Plan '{id}' existiert bereits; wechsle mit `plan switch {id}` zu ihm oder wähle eine andere id"
    )]
    PlanExists { id: PlanId },

    /// Ein per ID angesprochener Plan liegt nicht im Store (Runde 5, Teil P).
    ///
    /// # Auslöser
    /// `switch`, `archive` oder `inspect <id>` mit einer unbekannten ID.
    ///
    /// # Arguments
    /// - `id` (`PlanId`): die unbekannte ID.
    #[msg("Plan '{id}' ist im Store nicht vorhanden; `plan plans` listet alle Pläne")]
    PlanUnknown { id: PlanId },

    /// Der Store verwaltet nur einen Plan und kann die Katalog-Operation
    /// nicht ausführen (Runde 5, Teil P; nur Test-/Fremd-Stores, die die
    /// Standardmethoden von `PlanStore` nicht überschreiben).
    ///
    /// # Arguments
    /// - `operation` (`&'static str`): Name der Katalog-Operation.
    #[msg("Dieser Plan-Store verwaltet nur einen Plan; '{operation}' wird nicht unterstützt")]
    CatalogUnsupported { operation: &'static str },

    /// Ein Knoten soll gestartet werden, obwohl eine Abhängigkeit noch nicht
    /// abgeschlossen ist.
    ///
    /// # Auslöser
    /// Statuswechsel eines Knotens, dessen Dependency-Knoten noch nicht
    /// `Completed` ist. Ersetzt die frühere Missbrauchsnutzung von
    /// `IllegalTransition` für diesen Fall.
    ///
    /// # Arguments
    /// - `id` (`TaskId`): Knoten, der gestartet werden soll.
    /// - `dependency` (`TaskId`): die blockierende Abhängigkeit.
    /// - `status` (`String`): aktueller Status der Abhängigkeit.
    #[msg(
        "Knoten '{id}' kann nicht gestartet werden: Abhängigkeit '{dependency}' hat Status '{status}' statt Completed"
    )]
    DependencyNotCompleted {
        id: TaskId,
        dependency: TaskId,
        status: String,
    },

    /// Ein Knoten wechselt zu `Coding`/`Integration`, ohne dass zuvor eine
    /// frische Exploration stattgefunden hat (Regel 12).
    ///
    /// # Auslöser
    /// Statuswechsel nach `Coding` oder `Integration`, während der zuletzt
    /// erfasste Explorations-Nachweis fehlt oder veraltet ist.
    ///
    /// # Arguments
    /// - `id` (`TaskId`): betroffener Knoten.
    #[msg("Knoten '{id}' benötigt eine frische Exploration vor Coding/Integration (Regel 12)")]
    ExplorationRequired { id: TaskId },

    /// Der `write_scope` eines Kind-Knotens verlässt den `write_scope` seines
    /// Parent-Knotens (Regel 10).
    ///
    /// # Auslöser
    /// `AddNode`/`Patch` eines Kind-Knotens, dessen `write_scope` einen Pfad
    /// enthält, der außerhalb des Parent-Scopes liegt.
    ///
    /// # Arguments
    /// - `child` (`TaskId`): der Kind-Knoten.
    /// - `path` (`String`): der überschreitende Pfad/Symbol.
    #[msg(
        "write_scope-Pfad '{path}' von Kind-Knoten '{child}' liegt außerhalb des Parent-Scopes (Regel 10)"
    )]
    ExpandScopeEscapes { child: TaskId, path: String },

    /// Ein Composite-Knoten soll auf `Completed` gesetzt werden, obwohl noch
    /// Kind-Knoten offen sind (Regel 11).
    ///
    /// # Auslöser
    /// `SetStatus(Completed)` auf einem Composite-Knoten, dessen
    /// Kind-Knoten-Liste noch nicht abgeschlossene Einträge enthält.
    ///
    /// # Arguments
    /// - `id` (`TaskId`): der Composite-Knoten.
    /// - `open` (`Vec<TaskId>`): die noch offenen Kind-Knoten.
    #[msg(
        "Composite-Knoten '{id}' kann nicht auf Completed gesetzt werden: offene Kind-Knoten {open:?}"
    )]
    CompositeIncomplete { id: TaskId, open: Vec<TaskId> },

    /// Ein `Patch` wurde für einen bereits abgeschlossenen (`Completed`)
    /// Knoten versucht.
    ///
    /// # Auslöser
    /// `Patch`-Aktion auf einem Knoten mit Status `Completed`; abgeschlossene
    /// Knoten sind versiegelt und nur über `Supersede`/`Invalidate`
    /// veränderbar.
    ///
    /// # Arguments
    /// - `id` (`TaskId`): der versiegelte Knoten.
    #[msg(
        "Knoten '{id}' ist versiegelt (Completed/Superseded/Invalidated) und kann nicht mehr verändert werden"
    )]
    NodeSealed { id: TaskId },

    /// Ein Akteur versucht eine Aktion, für die er nicht autorisiert ist.
    ///
    /// # Auslöser
    /// Zum Beispiel ein Modell-Akteur, der `Goal::Achieved` oder eine andere
    /// Owner-reservierte Aktion auslösen will.
    ///
    /// # Arguments
    /// - `action` (`String`): die verweigerte Aktion.
    /// - `actor` (`String`): der ausführende Akteur.
    #[msg("Aktion '{action}' ist für Akteur '{actor}' nicht autorisiert")]
    ActorNotAuthorized { action: String, actor: String },

    /// Kein `Goal` für den aktuellen Plan definiert.
    ///
    /// # Auslöser
    /// Zugriff auf das Plan-Goal, bevor eines gesetzt wurde.
    #[msg("Kein Goal für diesen Plan definiert")]
    GoalNotFound,

    /// Ein Goal-Statusübergang (`Achieved`/`Abandoned`) wurde außerhalb eines
    /// Owner-Commands versucht.
    ///
    /// # Auslöser
    /// Jeder Versuch, das Goal auf einen reservierten Status zu setzen, ohne
    /// dass die Aktion als Owner-Command markiert ist.
    ///
    /// # Arguments
    /// - `status` (`String`): der angeforderte, reservierte Zielstatus.
    #[msg("Statusübergang zu '{status}' ist ausschließlich per Owner-Command erlaubt")]
    GoalTransitionReserved { status: String },

    /// Optimistische Revisionsprüfung eines Batches ist gescheitert.
    ///
    /// # Auslöser
    /// `PlanStore::apply_batch` mit `expected_rev`, das nicht der aktuellen
    /// Revision des Plans entspricht — ein anderer Akteur hat den Plan
    /// zwischenzeitlich verändert. Es wurde nichts angewendet.
    ///
    /// # Arguments
    /// - `plan` (`PlanId`): betroffener Plan.
    /// - `expected` (`RevisionId`): vom Aufrufer erwartete Revision.
    /// - `actual` (`RevisionId`): tatsächliche aktuelle Revision.
    #[msg("Revisionskonflikt für Plan '{plan}': erwartet={expected}, aktuell={actual}")]
    RevisionConflict {
        plan: PlanId,
        expected: RevisionId,
        actual: RevisionId,
    },

    /// Eine Aktion innerhalb eines Batches wurde abgewiesen; der gesamte
    /// Batch wurde verworfen (alles oder nichts).
    ///
    /// # Arguments
    /// - `index` (`usize`): 0-basierter Index der abgewiesenen Aktion.
    /// - `source` (`Box<PlanError>`): der Fehler dieser Aktion.
    #[msg("Batch verworfen: Aktion #{index} abgewiesen: {source}")]
    BatchActionRejected {
        index: usize,
        source: Box<PlanError>,
    },

    /// Das Integritätssiegel eines Snapshots passt nicht zu seinem Inhalt
    /// oder zur Siegelkette.
    ///
    /// # Auslöser
    /// Laden eines `rev-<n>.json`, dessen `rev-<n>.seal` fehlt (obwohl das
    /// Verzeichnis versiegelt ist), unlesbar ist, einen anderen Digest trägt
    /// oder dessen Kettenwert nicht zum Vorgänger passt. Das Siegel ist
    /// ungeschlüsselt: es erkennt Beschädigung und naive Manipulation, nicht
    /// einen Angreifer mit Schreibrecht auf `HARW_HOME`.
    ///
    /// # Arguments
    /// - `path` (`String`): betroffene Datei.
    /// - `expected` (`String`): erwarteter Wert (laut Siegel bzw. Kette).
    /// - `actual` (`String`): tatsächlich berechneter/gefundener Wert.
    #[msg("Siegelprüfung fehlgeschlagen für '{path}': erwartet={expected}, gefunden={actual}")]
    SealMismatch {
        path: String,
        expected: String,
        actual: String,
    },

    /// I/O-Fehler (für `FilePlanStore`).
    #[msg("I/O-Fehler: {0}")]
    #[from]
    Io(std::io::Error),

    /// JSON-Serialisierungsfehler.
    #[msg("Serialisierungsfehler: {0}")]
    #[from]
    Serde(serde_json::Error),

    /// Konfigurations- oder Policy-Fehler des Plan-Tools.
    #[msg("Plan-Tool-Konfiguration: {0}")]
    #[from]
    Config(PlanToolConfigError),

    /// Der Plan wurde noch nicht mit `Create` angelegt.
    #[msg("Kein aktiver Plan vorhanden (Create fehlt)")]
    PlanNotFound,
}

impl PlanError {
    /// Erzeugt den Fehler für einen leeren ID-Rohwert.
    ///
    /// # Beschreibung
    /// Dies ist der Konstruktor, den `#[derive(harw_macros::HarwId)]` über
    /// `#[harw_id(ctor = "empty_id")]` aufruft. Der Vertrag des Makros gibt die
    /// Signatur vor: es reicht **nur** den Feldnamen durch, nicht den Rohwert.
    ///
    /// Das kostet hier nichts: `try_new` ruft diesen Konstruktor ausschließlich,
    /// wenn der getrimmte Rohwert leer ist — `value` wäre also ohnehin die leere
    /// Zeichenkette. Für jede andere Ungültigkeit bleibt
    /// [`PlanError::InvalidId`] direkt konstruierbar, mit vollem Rohwert.
    ///
    /// # Argumente
    /// - `field` (`&'static str`): Name des ID-Typs bzw. Feldes, das den
    ///   ungültigen Wert bekommen hat (z. B. `"PlanId"`).
    ///
    /// # Rückgabe
    /// [`PlanError::InvalidId`] mit leerem `value`.
    ///
    /// # Nebenläufigkeit
    /// Rein; keine Sperren, kein gemeinsamer Zustand.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_plan::error::PlanError;
    ///
    /// let error = PlanError::empty_id("PlanId");
    /// assert!(matches!(error, PlanError::InvalidId { field: "PlanId", .. }));
    /// ```
    #[must_use]
    pub fn empty_id(field: &'static str) -> Self {
        Self::InvalidId {
            field,
            value: String::new(),
        }
    }

    /// Erzeugt den Fehler für eine unbekannte Enum-Variante.
    ///
    /// # Beschreibung
    /// Konstruktor für `#[derive(harw_macros::KebabEnum)]`, konfiguriert über
    /// `#[kebab_enum(ctor = "unknown_variant")]`. Anders als [`Self::empty_id`]
    /// bekommt dieser Konstruktor den **vollen Rohwert** — bei einer unbekannten
    /// Variante ist genau er die nützliche Information („war es ein Tippfehler
    /// oder eine Variante, die es nicht mehr gibt?").
    ///
    /// # Argumente
    /// - `type_name` (`&'static str`): Name des Enums, z. B. `"PlanNodeKind"`.
    /// - `value` (`&str`): die unbekannte, nicht normalisierte Eingabe.
    ///
    /// # Rückgabe
    /// [`PlanError::InvalidId`] mit Typname als `field` und dem Rohwert.
    ///
    /// # Nebenläufigkeit
    /// Rein; keine Sperren, kein gemeinsamer Zustand.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_plan::error::PlanError;
    ///
    /// let error = PlanError::unknown_variant("PlanNodeKind", "codeing");
    /// assert!(error.to_string().contains("codeing"));
    /// ```
    #[must_use]
    pub fn unknown_variant(type_name: &'static str, value: &str) -> Self {
        Self::InvalidId {
            field: type_name,
            value: value.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    // ── PlanToolConfigError: Display ───────────────────────────────────────

    #[test]
    fn test_config_error_disabled_display() {
        let err = PlanToolConfigError::Disabled;
        assert_eq!(err.to_string(), "Plan-Tool ist deaktiviert");
    }

    #[test]
    fn test_config_error_zero_max_nodes_display() {
        let err = PlanToolConfigError::ZeroMaxNodes;
        assert_eq!(err.to_string(), "Plan-Tool-Konfiguration hat max_nodes = 0");
    }

    #[test]
    fn test_config_error_node_limit_exceeded_display() {
        let err = PlanToolConfigError::NodeLimitExceeded {
            max_nodes: 3,
            attempted_nodes: 4,
        };
        let msg = err.to_string();
        assert!(msg.contains("erlaubt=3"), "msg={msg}");
        assert!(msg.contains("versucht=4"), "msg={msg}");
    }

    // ── PlanError: Display der bestehenden Varianten (Aussage erhalten) ────

    #[test]
    fn test_node_missing_display() {
        let err = PlanError::NodeMissing {
            id: TaskId::new("t1"),
        };
        assert_eq!(err.to_string(), "Plan-Knoten 't1' existiert nicht");
    }

    #[test]
    fn test_cycle_detected_display() {
        let err = PlanError::CycleDetected {
            child: TaskId::new("t1"),
            parent: TaskId::new("t2"),
        };
        let msg = err.to_string();
        assert!(msg.contains("t1"), "msg={msg}");
        assert!(msg.contains("t2"), "msg={msg}");
        assert!(msg.contains("Zyklus"), "msg={msg}");
    }

    #[test]
    fn test_illegal_transition_display() {
        let err = PlanError::IllegalTransition {
            id: TaskId::new("t1"),
            from: "draft".to_owned(),
            to: "completed".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("draft"), "msg={msg}");
        assert!(msg.contains("completed"), "msg={msg}");
        assert!(msg.contains("t1"), "msg={msg}");
    }

    #[test]
    fn test_evidence_missing_display() {
        let err = PlanError::EvidenceMissing {
            id: TaskId::new("t1"),
        };
        assert!(err.to_string().contains("EvidenceRef"));
    }

    #[test]
    fn test_scope_conflict_display() {
        let err = PlanError::ScopeConflict {
            conflicting_path: "src/lib.rs".to_owned(),
            existing_node: TaskId::new("t1"),
        };
        let msg = err.to_string();
        assert!(msg.contains("src/lib.rs"), "msg={msg}");
        assert!(msg.contains("t1"), "msg={msg}");
    }

    #[test]
    fn test_forbidden_scope_overlap_display() {
        let err = PlanError::ForbiddenScopeOverlap {
            path: "src/secret.rs".to_owned(),
        };
        assert!(err.to_string().contains("src/secret.rs"));
    }

    #[test]
    fn test_revision_regressed_display() {
        let err = PlanError::RevisionRegressed {
            current: RevisionId::new(5),
            attempted: RevisionId::new(3),
        };
        let msg = err.to_string();
        assert!(msg.contains("aktuelle=5"), "msg={msg}");
        assert!(msg.contains("versucht=3"), "msg={msg}");
    }

    #[test]
    fn test_invalidate_completed_display() {
        let err = PlanError::InvalidateCompleted {
            id: TaskId::new("t1"),
        };
        assert!(err.to_string().contains("t1"));
    }

    #[test]
    fn test_plan_not_found_display() {
        let err = PlanError::PlanNotFound;
        assert_eq!(
            err.to_string(),
            "Kein aktiver Plan vorhanden (Create fehlt)"
        );
    }

    // ── PlanError: Display der neuen Varianten ──────────────────────────────

    #[test]
    fn test_duplicate_node_display() {
        let err = PlanError::DuplicateNode {
            id: TaskId::new("t1"),
        };
        assert_eq!(err.to_string(), "Plan-Knoten 't1' ist bereits vergeben");
    }

    #[test]
    fn test_invalid_id_display() {
        let err = PlanError::InvalidId {
            field: "id",
            value: "   ".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("'id'"), "msg={msg}");
        assert!(msg.contains("\"   \""), "msg={msg}");
    }

    /// Steuerzeichen im Rohwert werden escaped ausgegeben (keine
    /// Log-Injektion über eine ungültige ID).
    #[test]
    fn test_invalid_id_display_escapes_control_characters() {
        let err = PlanError::InvalidId {
            field: "PlanId",
            value: "a\nb".to_owned(),
        };
        let msg = err.to_string();
        assert!(!msg.contains('\n'), "msg={msg}");
        assert!(msg.contains("a\\nb"), "msg={msg}");
    }

    #[test]
    fn test_plan_exists_display() {
        let err = PlanError::PlanExists {
            id: PlanId::new("p-1"),
        };
        let msg = err.to_string();
        assert!(msg.contains("p-1"));
        // Runde 5, Teil P: kein „nur für einen leeren Store" mehr, sondern
        // der Hinweis auf `switch`.
        assert!(msg.contains("plan switch p-1"), "msg={msg}");
        assert!(!msg.contains("leeren Store"), "msg={msg}");
    }

    #[test]
    fn test_plan_unknown_display_points_at_the_listing() {
        let err = PlanError::PlanUnknown {
            id: PlanId::new("p-weg"),
        };
        let msg = err.to_string();
        assert!(msg.contains("p-weg"), "msg={msg}");
        assert!(msg.contains("plan plans"), "msg={msg}");
    }

    #[test]
    fn test_dependency_not_completed_display() {
        let err = PlanError::DependencyNotCompleted {
            id: TaskId::new("t1"),
            dependency: TaskId::new("t0"),
            status: "InProgress".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("t1"), "msg={msg}");
        assert!(msg.contains("t0"), "msg={msg}");
        assert!(msg.contains("InProgress"), "msg={msg}");
    }

    #[test]
    fn test_exploration_required_display() {
        let err = PlanError::ExplorationRequired {
            id: TaskId::new("t1"),
        };
        let msg = err.to_string();
        assert!(msg.contains("t1"), "msg={msg}");
        assert!(msg.contains("Regel 12"), "msg={msg}");
    }

    #[test]
    fn test_expand_scope_escapes_display() {
        let err = PlanError::ExpandScopeEscapes {
            child: TaskId::new("t-child"),
            path: "src/outside.rs".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("t-child"), "msg={msg}");
        assert!(msg.contains("src/outside.rs"), "msg={msg}");
        assert!(msg.contains("Regel 10"), "msg={msg}");
    }

    #[test]
    fn test_composite_incomplete_display() {
        let err = PlanError::CompositeIncomplete {
            id: TaskId::new("t-parent"),
            open: vec![TaskId::new("t-a"), TaskId::new("t-b")],
        };
        let msg = err.to_string();
        assert!(msg.contains("t-parent"), "msg={msg}");
        assert!(msg.contains("t-a"), "msg={msg}");
        assert!(msg.contains("t-b"), "msg={msg}");
    }

    #[test]
    fn test_node_sealed_display() {
        let err = PlanError::NodeSealed {
            id: TaskId::new("t1"),
        };
        let msg = err.to_string();
        assert!(msg.contains("t1"), "msg={msg}");
        assert!(msg.contains("Completed"), "msg={msg}");
    }

    #[test]
    fn test_revision_conflict_display() {
        let err = PlanError::RevisionConflict {
            plan: PlanId::new("p-1"),
            expected: RevisionId::new(3),
            actual: RevisionId::new(4),
        };
        let msg = err.to_string();
        assert!(msg.contains("p-1"), "msg={msg}");
        assert!(msg.contains("erwartet=3"), "msg={msg}");
        assert!(msg.contains("aktuell=4"), "msg={msg}");
    }

    #[test]
    fn test_batch_action_rejected_display_names_index_and_cause() {
        let err = PlanError::BatchActionRejected {
            index: 2,
            source: Box::new(PlanError::NodeMissing {
                id: TaskId::new("t9"),
            }),
        };
        let msg = err.to_string();
        assert!(msg.contains("#2"), "msg={msg}");
        assert!(msg.contains("t9"), "msg={msg}");
    }

    #[test]
    fn test_seal_mismatch_display() {
        let err = PlanError::SealMismatch {
            path: "plans/p-1/rev-2.json".to_owned(),
            expected: "aa".to_owned(),
            actual: "bb".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("rev-2.json"), "msg={msg}");
        assert!(msg.contains("erwartet=aa"), "msg={msg}");
        assert!(msg.contains("gefunden=bb"), "msg={msg}");
    }

    #[test]
    fn test_actor_not_authorized_display() {
        let err = PlanError::ActorNotAuthorized {
            action: "Goal::Achieved".to_owned(),
            actor: "model".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Goal::Achieved"), "msg={msg}");
        assert!(msg.contains("model"), "msg={msg}");
    }

    #[test]
    fn test_goal_not_found_display() {
        let err = PlanError::GoalNotFound;
        assert_eq!(err.to_string(), "Kein Goal für diesen Plan definiert");
    }

    #[test]
    fn test_goal_transition_reserved_display() {
        let err = PlanError::GoalTransitionReserved {
            status: "Achieved".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Achieved"), "msg={msg}");
        assert!(msg.contains("Owner-Command"), "msg={msg}");
    }

    // ── From-Konvertierungen ─────────────────────────────────────────────

    #[test]
    fn test_from_io_error_wraps_and_displays() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err: PlanError = io_err.into();

        assert!(matches!(err, PlanError::Io(_)));
        assert!(err.to_string().starts_with("I/O-Fehler: "));
        assert!(err.to_string().contains("file missing"));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn test_from_serde_error_wraps_and_displays() -> TestResult {
        let Err(serde_err) = serde_json::from_str::<serde_json::Value>("!!!") else {
            return Err(TestError::Unexpected(
                "ungültiges JSON muss einen Fehler liefern".to_owned(),
            ));
        };
        let err: PlanError = serde_err.into();

        assert!(matches!(err, PlanError::Serde(_)));
        assert!(err.to_string().starts_with("Serialisierungsfehler: "));
        assert!(std::error::Error::source(&err).is_some());
        Ok(())
    }

    #[test]
    fn test_from_config_error_wraps_and_displays() {
        let err: PlanError = PlanToolConfigError::Disabled.into();

        assert!(matches!(
            err,
            PlanError::Config(PlanToolConfigError::Disabled)
        ));
        assert_eq!(
            err.to_string(),
            "Plan-Tool-Konfiguration: Plan-Tool ist deaktiviert"
        );
        assert!(std::error::Error::source(&err).is_some());
    }

    // ── source() für Leaf-Varianten ─────────────────────────────────────

    #[test]
    fn test_source_none_for_leaf_variant() {
        let err = PlanError::PlanNotFound;
        assert!(std::error::Error::source(&err).is_none());

        let err = PlanError::NodeMissing {
            id: TaskId::new("t1"),
        };
        assert!(std::error::Error::source(&err).is_none());
    }

    // ── PlanResult-Alias ─────────────────────────────────────────────────

    fn returns_plan_result_ok() -> PlanResult<()> {
        Ok(())
    }

    fn returns_plan_result_err() -> PlanResult<()> {
        Err(PlanError::PlanNotFound)
    }

    #[test]
    fn test_plan_result_alias_exists() {
        assert!(returns_plan_result_ok().is_ok());
        assert!(returns_plan_result_err().is_err());
    }
}
