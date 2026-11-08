//! Routing-Tabelle (README §3.3): welche Regel an welche Triage-Rolle geht.
//!
//! # Zweck
//! [`RouteTable`] ist die **vollständige**, feste Zuordnung jeder bekannten
//! Regel-Kennung zu einer [`Route`]. Eine Regel, die hier fehlt, gilt als
//! unbekannt und wird verworfen ([`crate::DropReason::UnknownRule`]).
//!
//! # Regel-Kennungen
//! Schreibweise wie die bestehenden Regeln in `harw-dod-rules` (kebab-case:
//! `egress-flow`, `structure-drift`, `baseline-deviation`,
//! `advisory-correlate`). Die Phase-1-Regeln (README §1.6 schreibt sie mit
//! Unterstrich) werden hier in derselben kebab-case-Konvention festgelegt:
//! [`RULE_LISTENER_NEW`], [`RULE_EXEC_WRITABLE_PATH`],
//! [`RULE_SENSOR_DEGRADED`], [`RULE_PERSISTENCE_WRITE`].
//!
//! Scanner-Berichte (`harw-dod-scanreport`) haben keine eigene Regel: sie
//! laufen als Drift-Ereignisse durch `structure-drift` (siehe
//! `harw-dod-rules/src/rules/structure_drift.rs`) und landen damit in der
//! Struktur-Triage. Raten (WP0.14) sind eine Stufe **vor** den Regeln und
//! erscheinen als `baseline-deviation`; eine eigene Raten-Regel kommt mit
//! einer neuen Konstante in [`ALL_RULE_IDS`] und einer Zeile in der Tabelle.
//!
//! # Routen
//! | Regel | Route |
//! |---|---|
//! | `egress-flow`, `listener-new` | [`TriageRole::Egress`] |
//! | `exec-writable-path`, `persistence-write` | [`TriageRole::Endpoint`] |
//! | `structure-drift`, `advisory-correlate` | [`TriageRole::Structure`], `Contain` wird zu `Escalate` |
//! | `baseline-deviation` | [`TriageRole::Baseline`] |
//! | `sensor-degraded`, `spool-degraded`, `dispatch-starved` | [`Route::NoAi`] |
//!
//! # Nebenläufigkeit und Fehler
//! Konstante Daten, `Copy`; keine Fehlerfälle.
//!
//! # Examples
//! ```
//! use harw_dod_dispatch::{Route, RouteTable, TriageRole, RULE_LISTENER_NEW, RULE_SENSOR_DEGRADED};
//!
//! let table = RouteTable::new();
//! assert_eq!(
//!     table.lookup(RULE_LISTENER_NEW),
//!     Some(Route::Triage { role: TriageRole::Egress, contain_downgraded_to_escalate: false })
//! );
//! assert_eq!(table.lookup(RULE_SENSOR_DEGRADED), Some(Route::NoAi));
//! assert_eq!(table.lookup("listener_new"), None);
//! ```

use serde::{Deserialize, Serialize};

/// Ausgehende Verbindung außerhalb der Allowlist (`harw-dod-rules`).
pub const RULE_EGRESS_FLOW: &str = "egress-flow";
/// Neuer Listener je Cgroup/uid (Phase 1, WP1.6).
pub const RULE_LISTENER_NEW: &str = "listener-new";
/// Ausführung aus einem beschreibbaren Pfad (Phase 1, WP1.6).
pub const RULE_EXEC_WRITABLE_PATH: &str = "exec-writable-path";
/// Schreibzugriff auf Persistenz-Orte (Phase 1, WP1.6).
pub const RULE_PERSISTENCE_WRITE: &str = "persistence-write";
/// Strukturdrift inklusive Scanner-Berichten (`harw-dod-rules`).
pub const RULE_STRUCTURE_DRIFT: &str = "structure-drift";
/// Advisory-Korrelation (`harw_dod_rules::advisory::RULE_ID`).
pub const RULE_ADVISORY_CORRELATE: &str = "advisory-correlate";
/// Abweichung von einer Baseline, inklusive Raten (`harw-dod-rules`).
pub const RULE_BASELINE_DEVIATION: &str = "baseline-deviation";
/// Ein Sensor meldet sich degradiert (Phase 1, WP1.6).
pub const RULE_SENSOR_DEGRADED: &str = "sensor-degraded";
/// Der Befund-Spool ist degradiert (README §3.3).
pub const RULE_SPOOL_DEGRADED: &str = "spool-degraded";
/// Breaker- oder Budget-Trip des Dispatchers (README §3.3, I10). Der
/// Aufrufer bildet aus [`crate::DispatchAction::Starved`] einen
/// `High`-Befund mit dieser Regel.
pub const RULE_DISPATCH_STARVED: &str = "dispatch-starved";

/// Jede Regel-Kennung, die die [`RouteTable`] kennt.
pub const ALL_RULE_IDS: [&str; 10] = [
    RULE_EGRESS_FLOW,
    RULE_LISTENER_NEW,
    RULE_EXEC_WRITABLE_PATH,
    RULE_PERSISTENCE_WRITE,
    RULE_STRUCTURE_DRIFT,
    RULE_ADVISORY_CORRELATE,
    RULE_BASELINE_DEVIATION,
    RULE_SENSOR_DEGRADED,
    RULE_SPOOL_DEGRADED,
    RULE_DISPATCH_STARVED,
];

/// Triage-Rolle, an die ein Befund geht.
///
/// # Description
/// Die Rollennamen stimmen mit `harw_registry_defaults::role_names`
/// überein ([`TriageRole::role_name`]).
///
/// # Examples
/// ```
/// use harw_dod_dispatch::TriageRole;
///
/// assert_eq!(TriageRole::Endpoint.role_name(), "security-endpoint-triage");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriageRole {
    /// `security-egress-triage`.
    Egress,
    /// `security-endpoint-triage`.
    Endpoint,
    /// `security-structure-triage`.
    Structure,
    /// `security-baseline-triage`.
    Baseline,
}

impl TriageRole {
    /// Alle Rollen.
    pub const ALL: [Self; 4] = [
        Self::Egress,
        Self::Endpoint,
        Self::Structure,
        Self::Baseline,
    ];

    /// Name der Agentenrolle in der Registry.
    ///
    /// # Returns
    /// Einen festen ASCII-Namen, z. B. `"security-egress-triage"`.
    #[must_use]
    pub const fn role_name(self) -> &'static str {
        match self {
            Self::Egress => "security-egress-triage",
            Self::Endpoint => "security-endpoint-triage",
            Self::Structure => "security-structure-triage",
            Self::Baseline => "security-baseline-triage",
        }
    }
}

/// Wohin eine Regel geht.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::{Route, TriageRole};
///
/// let route = Route::Triage { role: TriageRole::Structure, contain_downgraded_to_escalate: true };
/// assert!(route.is_ai());
/// assert!(!Route::NoAi.is_ai());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    /// KI-Triage durch eine Rolle.
    Triage {
        /// Zuständige Rolle.
        role: TriageRole,
        /// `Contain` aus dem Verdict wird zu `Escalate` herabgestuft
        /// (Hardness `Inferred`, README §3.3).
        contain_downgraded_to_escalate: bool,
    },
    /// Keine KI, nur der deterministische Hinweis.
    NoAi,
}

impl Route {
    /// Ob die Route ein Modell beteiligt.
    ///
    /// # Returns
    /// `true` für [`Route::Triage`].
    #[must_use]
    pub const fn is_ai(self) -> bool {
        matches!(self, Self::Triage { .. })
    }
}

// Kurzform für Zeilen der Tabelle.
const fn triage(role: TriageRole) -> Route {
    Route::Triage {
        role,
        contain_downgraded_to_escalate: false,
    }
}

// Die vollständige Tabelle; `ALL_RULE_IDS` nennt dieselben Kennungen.
static ROUTES: [(&str, Route); 10] = [
    (RULE_EGRESS_FLOW, triage(TriageRole::Egress)),
    (RULE_LISTENER_NEW, triage(TriageRole::Egress)),
    (RULE_EXEC_WRITABLE_PATH, triage(TriageRole::Endpoint)),
    (RULE_PERSISTENCE_WRITE, triage(TriageRole::Endpoint)),
    (
        RULE_STRUCTURE_DRIFT,
        Route::Triage {
            role: TriageRole::Structure,
            contain_downgraded_to_escalate: true,
        },
    ),
    (
        RULE_ADVISORY_CORRELATE,
        Route::Triage {
            role: TriageRole::Structure,
            contain_downgraded_to_escalate: true,
        },
    ),
    (RULE_BASELINE_DEVIATION, triage(TriageRole::Baseline)),
    (RULE_SENSOR_DEGRADED, Route::NoAi),
    (RULE_SPOOL_DEGRADED, Route::NoAi),
    (RULE_DISPATCH_STARVED, Route::NoAi),
];

/// Die feste, vollständige Routing-Tabelle.
///
/// # Description
/// Nicht konfigurierbar: welche Regel ein Modell sieht, ist Teil des
/// Sicherheitsvertrags (README §3.3). Ein exakter Vergleich der Kennung;
/// Groß-/Kleinschreibung, Unterstriche und Leerraum ergeben keinen Treffer.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::{RouteTable, ALL_RULE_IDS};
///
/// let table = RouteTable::new();
/// assert!(ALL_RULE_IDS.iter().all(|rule| table.lookup(rule).is_some()));
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RouteTable;

impl RouteTable {
    /// Die Tabelle aus README §3.3.
    ///
    /// # Returns
    /// Die (zustandslose) Tabelle.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Route einer Regel.
    ///
    /// # Arguments
    /// - `rule_id` (`&str`): Regel-Kennung aus dem Befund (untrusted).
    ///
    /// # Returns
    /// `None` für eine unbekannte Regel.
    #[must_use]
    pub fn lookup(&self, rule_id: &str) -> Option<Route> {
        ROUTES
            .iter()
            .find(|(known, _)| *known == rule_id)
            .map(|(_, route)| *route)
    }

    /// Die kanonische, statische Kennung einer bekannten Regel.
    ///
    /// # Description
    /// Damit tragen Hinweise und Templates nie die Zeichenkette aus dem
    /// Befund, sondern die Konstante dieser Crate.
    ///
    /// # Arguments
    /// - `rule_id` (`&str`): Regel-Kennung aus dem Befund (untrusted).
    ///
    /// # Returns
    /// `None` für eine unbekannte Regel.
    #[must_use]
    pub fn canonical_rule_id(&self, rule_id: &str) -> Option<&'static str> {
        ROUTES
            .iter()
            .find(|(known, _)| *known == rule_id)
            .map(|(known, _)| *known)
    }

    /// Alle Zeilen der Tabelle.
    ///
    /// # Returns
    /// `(Regel-Kennung, Route)` in Tabellenreihenfolge.
    pub fn entries(&self) -> impl Iterator<Item = (&'static str, Route)> {
        ROUTES.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, missing};

    #[test]
    fn test_route_table_covers_every_rule_id_constant() -> TestResult {
        // Siehe test_route_table_matches_readme_section_3_3 für die Zuordnung.
        let table = RouteTable::new();
        for rule in ALL_RULE_IDS {
            assert!(table.lookup(rule).is_some(), "{rule} has no route");
            assert_eq!(table.canonical_rule_id(rule), Some(rule));
            // Jede Zeile der Tabelle ist genau eine Konstante.
            assert_eq!(table.entries().filter(|(id, _)| *id == rule).count(), 1);
        }
        assert_eq!(table.entries().count(), ALL_RULE_IDS.len());
        for (rule, _) in table.entries() {
            assert!(ALL_RULE_IDS.contains(&rule), "{rule} without constant");
        }
        Ok(())
    }

    #[test]
    fn test_route_table_matches_readme_section_3_3() -> TestResult {
        let table = RouteTable::new();
        let expected = [
            (RULE_EGRESS_FLOW, Some((TriageRole::Egress, false))),
            (RULE_LISTENER_NEW, Some((TriageRole::Egress, false))),
            (RULE_EXEC_WRITABLE_PATH, Some((TriageRole::Endpoint, false))),
            (RULE_PERSISTENCE_WRITE, Some((TriageRole::Endpoint, false))),
            (RULE_STRUCTURE_DRIFT, Some((TriageRole::Structure, true))),
            (RULE_ADVISORY_CORRELATE, Some((TriageRole::Structure, true))),
            (RULE_BASELINE_DEVIATION, Some((TriageRole::Baseline, false))),
            (RULE_SENSOR_DEGRADED, None),
            (RULE_SPOOL_DEGRADED, None),
            (RULE_DISPATCH_STARVED, None),
        ];
        for (rule, triage_route) in expected {
            let route = table.lookup(rule).ok_or_else(missing("route"))?;
            let actual = match route {
                Route::Triage {
                    role,
                    contain_downgraded_to_escalate,
                } => Some((role, contain_downgraded_to_escalate)),
                Route::NoAi => None,
            };
            assert_eq!(actual, triage_route, "{rule}");
        }
        Ok(())
    }

    #[test]
    fn test_existing_rule_ids_keep_harw_dod_rules_spelling() -> TestResult {
        // Schreibweise aus harw-dod-rules (egress_flow.rs, structure_drift.rs,
        // baseline_deviation.rs, advisory.rs).
        assert_eq!(RULE_EGRESS_FLOW, "egress-flow");
        assert_eq!(RULE_STRUCTURE_DRIFT, "structure-drift");
        assert_eq!(RULE_BASELINE_DEVIATION, "baseline-deviation");
        assert_eq!(RULE_ADVISORY_CORRELATE, "advisory-correlate");
        for rule in ALL_RULE_IDS {
            assert!(
                rule.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-'),
                "{rule} is not kebab-case"
            );
        }
        Ok(())
    }

    #[test]
    fn test_unknown_and_near_miss_rule_ids_have_no_route() -> TestResult {
        let table = RouteTable::new();
        for rule in [
            "",
            "Egress-Flow",
            "egress_flow",
            " egress-flow",
            "egress-flow ",
            "listener_new",
            "scanreport",
            "advisory",
            "egress-flow\u{0}",
        ] {
            assert_eq!(table.lookup(rule), None, "{rule:?}");
            assert_eq!(table.canonical_rule_id(rule), None, "{rule:?}");
        }
        Ok(())
    }

    #[test]
    fn test_role_names_match_the_registry() -> TestResult {
        let names: Vec<&str> = TriageRole::ALL
            .iter()
            .map(|role| role.role_name())
            .collect();
        assert_eq!(
            names,
            [
                "security-egress-triage",
                "security-endpoint-triage",
                "security-structure-triage",
                "security-baseline-triage",
            ]
        );
        Ok(())
    }
}
