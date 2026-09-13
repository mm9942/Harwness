//! Gate 1: verbotene Kanten im Abhängigkeitsgraphen.
//!
//! # Verantwortungsbereich
//! Prüft, dass bestimmte Kanten **nicht** existieren. Die Regeln stehen als
//! Daten in [`FORBIDDEN`] und [`PURE_CRATES`], nicht verstreut im Code: eine
//! neue verbotene Kante soll ein Listeneintrag sein, keine neue Funktion.
//!
//! # Die vier Regeln und warum es sie gibt
//!
//! **Keine Sensor-zu-Sensor-Kante.** Die Sensor-Crates sind Geschwister mit je
//! genau einer Quelle und je genau einer Fähigkeit. Eine Kante zwischen zweien
//! zöge die Fähigkeit der einen in die Rechtematrix der anderen — und die
//! Zusage „genau eine Quelle je Crate" wäre unwahr, ohne dass es jemandem
//! auffiele.
//!
//! **Kein Sammler zum Durchsetzer.** `harw-dod-sentinel` und `harw-sentinel`
//! dürfen `harw-dod-warden`/`harw-warden` nicht erreichen. Der Sammler ist
//! unprivilegiert und liest; der Durchsetzer handelt. Eine Kante dazwischen
//! wäre ein Weg, aus einem Lesepfad in einen Handlungspfad zu gelangen.
//!
//! **Kein Adapter zur Eskalationsleiter.** Sensoren und Sinks dürfen
//! `harw-dod-escalate` nicht erreichen. Dort entsteht `Action<Authorized>`;
//! wer die Crate importiert, sitzt am selben Tisch wie die Autorisierung.
//!
//! **Reine Crates bleiben rein.** `harw-lens-rank` darf in seiner
//! **vollständigen Hülle** nichts enthalten, das Dateien, Netz, Zeit oder
//! Zufall kann. Die Reinheit ist die Zusage, auf der seine Determinismus-Tests
//! beruhen; eine transitive Kante auf `jiff` würde sie unbemerkt aufheben.
//!
//! # Warum die Hülle und nicht die direkte Kante
//! Eine Fähigkeit wandert transitiv. Eine Crate, die nur einen harmlosen
//! Nachbarn nennt, aber über drei Ecken einen Dateileser erreicht, hat den
//! Dateileser — die direkte Kantenliste sagt das Gegenteil.
//!
//! # Was dieses Gate nicht prüft
//! Es sieht **Kanten, keine Aufrufe**. Zwei Crates ohne Kante können sich über
//! einen gemeinsamen Dritten erreichen; das ist zulässig und wird hier nicht
//! bewertet. Und es prüft nur `[dependencies]`, nicht `[dev-dependencies]` —
//! eine Testabhängigkeit auf einen Geschwistersensor ist erlaubt, weil sie
//! nicht ins ausgelieferte Binary wandert.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use harw_code_graph::{CrateNode, WorkspaceGraph};

use super::GateReport;

/// Wie ein Crate-Name in einer Regel benannt wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// Genau dieser Name.
    Exact(&'static str),
    /// Jeder Name mit diesem Präfix.
    Prefix(&'static str),
    /// Jeder Name aus [`SENSOR_CRATES`].
    Sensor,
}

impl Pattern {
    /// Ob `name` auf dieses Muster passt.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        match self {
            Self::Exact(expected) => name == *expected,
            Self::Prefix(prefix) => name.starts_with(prefix),
            Self::Sensor => SENSOR_CRATES.contains(&name),
        }
    }
}

/// Die Sensor-Crates. Namentlich aufgezählt statt über ein Präfix erkannt:
/// `harw-dod-readfs`, `harw-dod-netlink`, `harw-dod-cap` und `harw-dod-signals`
/// tragen dasselbe Präfix, sind aber **Zugriffs- und Vokabularschichten**, auf
/// die ein Sensor selbstverständlich zeigen darf.
pub const SENSOR_CRATES: &[&str] = &[
    "harw-dod-thermal",
    "harw-dod-cpu",
    "harw-dod-memory",
    "harw-dod-blockio",
    "harw-dod-netcounters",
    "harw-dod-gpu",
    "harw-dod-cgroup",
    "harw-dod-listener",
    "harw-dod-authlog",
    "harw-dod-scanreport",
    "harw-dod-workspace",
    "harw-dod-fsmon",
    "harw-dod-procmon",
    "harw-dod-flow",
];

/// Eine verbotene Kante samt ihrer Begründung.
#[derive(Debug, Clone, Copy)]
pub struct ForbiddenEdge {
    /// Wer nicht zeigen darf.
    pub from: Pattern,
    /// Worauf nicht gezeigt werden darf.
    pub to: Pattern,
    /// Warum — erscheint wörtlich in der Fehlermeldung.
    pub rule: &'static str,
}

/// Die Regeln. Ein neuer Eintrag genügt, um eine Kante zu verbieten.
pub const FORBIDDEN: &[ForbiddenEdge] = &[
    ForbiddenEdge {
        from: Pattern::Sensor,
        to: Pattern::Sensor,
        rule: "Sensoren sind Geschwister und dürfen einander nicht kennen (C7)",
    },
    ForbiddenEdge {
        from: Pattern::Exact("harw-dod-sentinel"),
        to: Pattern::Prefix("harw-dod-warden"),
        rule: "der Sammler darf den Durchsetzer nicht erreichen (S1)",
    },
    ForbiddenEdge {
        from: Pattern::Exact("harw-sentinel"),
        to: Pattern::Prefix("harw-dod-warden"),
        rule: "der Sammler darf den Durchsetzer nicht erreichen (S1)",
    },
    ForbiddenEdge {
        from: Pattern::Exact("harw-sentinel"),
        to: Pattern::Exact("harw-warden"),
        rule: "der Sammler darf den Durchsetzer nicht erreichen (S1)",
    },
    ForbiddenEdge {
        from: Pattern::Sensor,
        to: Pattern::Exact("harw-dod-escalate"),
        rule: "ein Sensor darf die Eskalationsleiter nicht erreichen (S1)",
    },
    ForbiddenEdge {
        from: Pattern::Prefix("harw-observe-"),
        to: Pattern::Exact("harw-dod-escalate"),
        rule: "ein Telemetrie-Sink darf die Eskalationsleiter nicht erreichen (S1)",
    },
];

/// Crates, deren **vollständige Hülle** frei von Nebenwirkungen sein muss,
/// mit der Liste dessen, was dort nicht vorkommen darf.
pub const PURE_CRATES: &[(&str, &[&str])] = &[(
    "harw-lens-rank",
    &[
        "jiff", "time", "chrono", "rand", "getrandom", "tokio", "reqwest", "fs4", "tempfile",
        "std-fs",
    ],
)];

/// Führt Gate 1 aus.
///
/// # Returns
/// Einen [`GateReport`] mit der Zahl der geprüften Kandidaten — Kantenpaare
/// plus Hüllen-Einträge.
///
/// # Errors
/// Wenn der Workspace-Graph nicht gelesen werden kann. Ein **Verstoß** ist
/// kein `Err`, sondern erscheint im Bericht: das Gate hat dann erfolgreich
/// geprüft und etwas gefunden.
pub fn run() -> Result<GateReport, String> {
    let graph = WorkspaceGraph::load(Path::new("."))
        .map_err(|error| format!("Workspace-Graph nicht lesbar: {error}"))?;
    Ok(evaluate(&graph))
}

/// Prüft einen bereits geladenen Graphen.
///
/// # Description
/// Getrennt von [`run`], damit die Regeln gegen einen **konstruierten** Graphen
/// prüfbar sind. Ein Gate, dessen roter Zweig nie ausgeführt wurde, ist
/// unbewiesen — und gegen den echten Workspace lässt sich ein Verstoß nicht
/// herbeiführen, ohne ihn kaputtzumachen.
///
/// # Arguments
/// - `graph`: der zu prüfende Abhängigkeitsgraph.
///
/// # Returns
/// Den Bericht mit geprüfter Anzahl und gefundenen Verstößen.
#[must_use]
pub fn evaluate(graph: &WorkspaceGraph) -> GateReport {
    let mut violations = Vec::new();
    let mut checked = 0usize;

    for node in &graph.crates {
        for dep in &node.deps {
            checked += 1;
            for edge in FORBIDDEN {
                if edge.from.matches(&node.name) && edge.to.matches(dep) && node.name != *dep {
                    violations.push(format!(
                        "{} → {} verletzt: {}",
                        node.name, dep, edge.rule
                    ));
                }
            }
        }
    }

    let by_name: HashMap<&str, &CrateNode> =
        graph.crates.iter().map(|c| (c.name.as_str(), c)).collect();

    for (pure, forbidden_names) in PURE_CRATES {
        let Some(root) = by_name.get(pure) else {
            continue;
        };
        let hull = transitive_hull(root, &by_name);
        checked += hull.len();
        for name in &hull {
            if forbidden_names.contains(&name.as_str()) {
                violations.push(format!(
                    "{pure} erreicht '{name}' in seiner Hülle — die Crate muss frei von \
                     I/O, Zeit und Zufall bleiben (L7)"
                ));
            }
        }
    }

    GateReport {
        name: "edges",
        checked,
        violations,
    }
}

/// Alle Namen, die `root` über normale Abhängigkeiten erreicht.
///
/// # Description
/// Schließt sowohl interne (`deps`) als auch externe (`external_deps`)
/// Abhängigkeiten ein — die Reinheitsprüfung interessiert sich gerade für die
/// externen. Zyklen können in einem Cargo-Workspace nicht auftreten; der
/// `HashSet` schützt trotzdem gegen eine Endlosschleife, falls der Graph
/// jemals anders geladen wird.
///
/// # Returns
/// Die erreichten Namen ohne `root` selbst, in stabiler Ordnung.
#[must_use]
fn transitive_hull(root: &CrateNode, by_name: &HashMap<&str, &CrateNode>) -> BTreeSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = BTreeSet::new();
    let mut stack: Vec<String> = root
        .deps
        .iter()
        .chain(root.external_deps.iter())
        .cloned()
        .collect();

    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        out.insert(name.clone());
        if let Some(node) = by_name.get(name.as_str()) {
            stack.extend(node.deps.iter().cloned());
            stack.extend(node.external_deps.iter().cloned());
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn node(name: &str, deps: &[&str], external: &[&str]) -> CrateNode {
        CrateNode {
            name: name.to_owned(),
            version: "0.0.0".to_owned(),
            manifest_path: PathBuf::from(format!("{name}/Cargo.toml")),
            dir: PathBuf::from(name),
            deps: deps.iter().map(|d| (*d).to_owned()).collect(),
            dev_deps: Vec::new(),
            build_deps: Vec::new(),
            external_deps: external.iter().map(|d| (*d).to_owned()).collect(),
            is_leaf: deps.is_empty(),
            level: 0,
        }
    }

    fn graph(crates: Vec<CrateNode>) -> WorkspaceGraph {
        WorkspaceGraph {
            root: PathBuf::from("."),
            crates,
        }
    }

    #[test]
    fn test_evaluate_sensor_to_sensor_edge_is_a_violation() {
        let g = graph(vec![
            node("harw-dod-thermal", &["harw-dod-cpu"], &[]),
            node("harw-dod-cpu", &[], &[]),
        ]);

        let report = evaluate(&g);

        assert!(!report.is_green());
        assert!(
            report.violations[0].contains("harw-dod-thermal → harw-dod-cpu"),
            "die Meldung muss das konkrete Paar nennen: {:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_sensor_to_access_layer_is_allowed() {
        // `harw-dod-readfs` trägt dasselbe Präfix wie ein Sensor, ist aber
        // eine Zugriffsschicht — genau der Fall, den eine Präfixregel falsch
        // erwischt hätte.
        let g = graph(vec![
            node("harw-dod-thermal", &["harw-dod-readfs", "harw-dod-cap"], &[]),
            node("harw-dod-readfs", &[], &[]),
            node("harw-dod-cap", &[], &[]),
        ]);

        assert!(evaluate(&g).is_green());
    }

    #[test]
    fn test_evaluate_sentinel_to_warden_is_a_violation() {
        let g = graph(vec![
            node("harw-dod-sentinel", &["harw-dod-warden-proto"], &[]),
            node("harw-dod-warden-proto", &[], &[]),
        ]);

        assert!(!evaluate(&g).is_green());
    }

    #[test]
    fn test_evaluate_counts_every_edge_not_only_violations() {
        let g = graph(vec![
            node("a", &["b", "c"], &[]),
            node("b", &["c"], &[]),
            node("c", &[], &[]),
        ]);

        let report = evaluate(&g);

        assert!(report.is_green());
        assert_eq!(
            report.checked, 3,
            "ein Gate ohne Zählung kann 'nichts verletzt' nicht von \
             'nichts geprüft' unterscheiden"
        );
    }

    #[test]
    fn test_evaluate_pure_crate_reaching_time_through_hull_is_a_violation() {
        // Die Kante ist nicht direkt: lens-rank → lens-types → jiff.
        let g = graph(vec![
            node("harw-lens-rank", &["harw-lens-types"], &[]),
            node("harw-lens-types", &[], &["jiff"]),
        ]);

        let report = evaluate(&g);

        assert!(!report.is_green());
        assert!(
            report.violations.iter().any(|v| v.contains("jiff")),
            "die transitive Verunreinigung muss benannt werden: {:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_pure_crate_with_clean_hull_is_green() {
        let g = graph(vec![
            node("harw-lens-rank", &["harw-lens-types"], &[]),
            node("harw-lens-types", &[], &["serde"]),
        ]);

        assert!(evaluate(&g).is_green());
    }

    #[test]
    fn test_evaluate_empty_graph_is_green_with_zero_checked() {
        let report = evaluate(&graph(Vec::new()));

        assert!(report.is_green());
        assert_eq!(report.checked, 0);
    }

    #[test]
    fn test_pattern_sensor_does_not_match_vocabulary_crates() {
        assert!(Pattern::Sensor.matches("harw-dod-thermal"));
        assert!(!Pattern::Sensor.matches("harw-dod-cap"));
        assert!(!Pattern::Sensor.matches("harw-dod-signals"));
        assert!(!Pattern::Sensor.matches("harw-dod-readfs"));
    }
}
