//! Aufgelöste Agentendefinition und Auflösungs-Trace (§17 DSL-Spec).
//!
//! Dieses Modul definiert die Ergebnistypen nach der vollständigen Auflösung
//! einer Agentendefinition durch den Resolver (`resolve.rs`).
//!
//! # Schlüsseltypen
//! - [`ResolvedAgentDefinition`] — vollständig aufgelöste und validierte Definition
//! - [`ResolutionTrace`] — Auditpfad der angewandten Schichten
//! - [`ResolutionStep`] — einzelner Schritt im Auflösungspfad
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync` und klonierbar.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::authority::AuthorityCeiling;
use crate::ids::{DefinitionId, Version};
use crate::roles::AgentRoleId;

/// Vollständig aufgelöste Agentendefinition (§17 Compiler-Ergebnis).
///
/// # Beschreibung
/// Enthält alle aufgelösten Felder nach dem Durchlauf durch den Compiler:
/// Basis-Definition laden, Mixins anwenden, Patches anwenden, Authority prüfen.
/// Die `config`-Tabelle enthält nicht-autoritative konfigurierbare Felder.
///
/// # Nebenläufigkeit
/// `Send + Sync`; unveränderlich nach Erstellung.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::resolve::resolve_definition;
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_agent_dsl::layers::DefinitionLayer;
/// use harw_agent_dsl::parse::parse_toml;
///
/// let src = r#"
/// schema = "harwness.agent/v1"
/// id = "harwness.agent.test@1"
/// version = "1.0.0"
/// role = "worker"
/// specialization = "test"
/// "#;
/// let raw = parse_toml(src).unwrap();
/// let id = DefinitionId::parse("harwness.agent.test@1").unwrap();
/// let layers = vec![(DefinitionLayer::BuiltIn, raw)];
/// let resolved = resolve_definition(&id, &layers, time::OffsetDateTime::now_utc()).unwrap();
/// assert_eq!(resolved.specialization, "test");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedAgentDefinition {
    /// Eindeutige Namensraum-ID dieser aufgelösten Definition.
    pub id: DefinitionId,

    /// Vollständige semantische Version.
    pub version: Version,

    /// Autoritative Rolle dieses Agenten (unveränderlich, §3).
    pub role: AgentRoleId,

    /// Spezialisierungsbezeichner (§10).
    pub specialization: String,

    /// Optionaler menschenlesbarer Name.
    pub name: Option<String>,

    /// Optionale Beschreibung.
    pub description: Option<String>,

    /// Standard-Reasoning-Effort nach Anwendung der Vererbungsregel über
    /// `extends`/Mixins/Schichten (spezifischere Definition überschreibt,
    /// analog zu `BudgetSpec::effort_cap`; siehe
    /// [`RawAgentDefinition::reasoning_effort`](crate::raw::RawAgentDefinition::reasoning_effort)).
    /// `None` = keine Ebene dieser Auflösung hat eine Aussage getroffen.
    pub reasoning_effort: Option<String>,

    /// Authority-Ceiling nach Anwendung aller Patches und Intersects (§7, §12).
    pub authority: AuthorityCeiling,

    /// Auditpfad der Auflösung.
    pub trace: ResolutionTrace,

    /// Nicht-autoritative konfigurierbare Tabellen (z. B. `[work]`, `[context]`).
    pub config: toml::Table,
}

impl ResolvedAgentDefinition {
    /// Die aufgelösten Skills dieser Definition (Vereinigung über
    /// `extends`/Mixins/Schichten, siehe [`crate::skills`]).
    ///
    /// # Beschreibung
    /// Liegt unter dem reservierten Schlüssel
    /// [`SKILLS_CONFIG_KEY`](crate::skills::SKILLS_CONFIG_KEY) in
    /// [`Self::config`], damit bestehende Struktur-Literale dieses Typs
    /// quellkompatibel bleiben. Diese Lesart ist nachsichtig (Nicht-Strings
    /// fallen weg); die strenge, fail-closed Prüfung macht [`crate::lower`].
    ///
    /// # Rückgabe
    /// Die Skill-Namen in Auflösungsreihenfolge; leer, wenn keine Ebene
    /// Skills führt.
    pub fn skills(&self) -> Vec<String> {
        crate::skills::skills_from_config_lenient(&self.config)
    }
}

/// Auditpfad der Definition-Auflösung.
///
/// # Beschreibung
/// Enthält die geordnete Kette aller angewandten Definitionsschritte:
/// Basis → Mixins → Patches. Ermöglicht vollständige Nachvollziehbarkeit.
///
/// # Nebenläufigkeit
/// `Send + Sync`.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::resolved::ResolutionTrace;
///
/// let trace = ResolutionTrace { steps: vec![] };
/// assert!(trace.steps.is_empty());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionTrace {
    /// Geordnete Kette der angewandten Definitionen (base → mixins → patches).
    pub steps: Vec<ResolutionStep>,
}

/// Einzelner Schritt im Auflösungspfad.
///
/// # Beschreibung
/// Dokumentiert welche Definition wann und in welcher Kapazität (Basis, Mixin, Patch)
/// auf die Zieldefinition angewandt wurde.
///
/// # Nebenläufigkeit
/// `Send + Sync`.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::resolved::ResolutionStep;
///
/// let step = ResolutionStep {
///     source: "harwness.agent.worker-base@1".to_owned(),
///     kind: "base".to_owned(),
///     applied_at: time::OffsetDateTime::now_utc(),
/// };
/// assert_eq!(step.kind, "base");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionStep {
    /// Kanonischer ID-String der angewandten Definition.
    pub source: String,

    /// Art der Anwendung: `"base"`, `"mixin"` oder `"patch"`.
    pub kind: String,

    /// Zeitstempel der Anwendung (RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub applied_at: OffsetDateTime,
}
