//! Gate 1: verbotene Kanten im Abhängigkeitsgraphen.
//!
//! # Verantwortungsbereich
//! Prüft, dass bestimmte Kanten **nicht** existieren. Die Regeln stehen als
//! Daten in [`FORBIDDEN`] und [`PURE_CRATES`], nicht verstreut im Code: eine
//! neue verbotene Kante soll ein Listeneintrag sein, keine neue Funktion.
//!
//! # Die Regeln und warum es sie gibt
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
//! [`PURE_CRATES`] nutzt dieselbe Hülle wie [`FORBIDDEN_REACH`]: ab der
//! ersten externen Crate folgt sie `Cargo.lock`, damit auch ein `uuid` mit
//! `v4`-Feature auffällt, das `getrandom` erst zwei Ecken weiter zieht. Fehlt
//! die Wurzel-Crate selbst im Graphen (umbenannt, entfernt), ist das ein
//! Verstoß, kein stilles Überspringen.
//!
//! **Keine Krypto- und HTTP-Stapel in Warden, Probes und Sensoren.**
//! Crypto Masterplan v2 §20/§21: die TCBs von `harw-warden`,
//! `harw-dod-warden*`, `harw-probe-fs`, `harw-probe-bpf` und jeder Crate aus
//! [`SENSOR_CRATES`] dürfen in ihrer **normalen Abhängigkeitshülle** weder
//! CryptGuard (`crypt_guard`, `crypt_guard_core`, `crypt_guard_service`,
//! `crypt_guard_hyper`) noch `hyper` oder `tower` noch `harw-dod-encrypt`
//! erreichen. Der kleine, geschlüsselte Beleg-Pfad des Warden bleibt klein;
//! neue Kryptographie gehört an die Stelle, an der Sicherheitsinformation
//! die lokale Grenze verlässt. Diese Regeln stehen in [`FORBIDDEN_REACH`]
//! und nennen **externe** Paketnamen — [`FORBIDDEN`] kennt nur Kanten
//! zwischen Workspace-Crates. Die Hülle folgt den internen `[dependencies]`
//! aus dem Graphen und ab der ersten externen Crate den Kanten in
//! `Cargo.lock`; damit fällt auch ein `hyper` auf, das über `reqwest` drei
//! Ecken weiter hereinkommt. `Cargo.lock` kennt keine Feature- und
//! Plattformfilter — ab der ersten externen Crate ist die Hülle eine
//! Überschätzung. Innerhalb des Workspace sieht der Graph keine
//! `[target.'cfg(..)'.dependencies]`; dort deklarierte Kanten fehlen der
//! Hülle (dieselbe Grenze wie in `gate_arch.rs`). Ein erreichter externer
//! Name ohne `Cargo.lock`-Eintrag ist ein Verstoß, kein stilles Blatt.
//!
//! **Die Fassade bleibt krypto-frei.** `harw-dod` darf `harw-dod-encrypt`
//! weder direkt ([`FORBIDDEN`]) noch transitiv ([`FORBIDDEN_REACH`])
//! erreichen (Masterplan v2 §3.3).
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

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use harw_code_graph::{CrateNode, WorkspaceGraph};

use super::GateReport;
use super::arch::LockIndex;

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
    ForbiddenEdge {
        from: Pattern::Exact("harw-dod"),
        to: Pattern::Exact("harw-dod-encrypt"),
        rule: "die DoD-Fassade darf harw-dod-encrypt nicht reexportieren \
               (Crypto-Masterplan v2 §3.3)",
    },
];

/// Namen, die eine Crate in ihrer normalen Abhängigkeitshülle nicht
/// erreichen darf — intern **oder extern**.
#[derive(Debug, Clone, Copy)]
pub struct ForbiddenReach {
    /// Wessen Hülle geprüft wird.
    pub from: Pattern,
    /// Paketnamen (wie in den `Cargo.toml`-Schlüsseln und in `Cargo.lock`),
    /// die dort nicht vorkommen dürfen.
    pub names: &'static [&'static str],
    /// Warum — erscheint wörtlich in der Fehlermeldung.
    pub rule: &'static str,
}

/// CryptGuard, der HTTP-/Tower-Stapel und die Harw-Kryptoschicht: nichts
/// davon gehört in Warden, Probes oder Sensoren (Masterplan v2 §20/§21).
/// Unterstrich-Namen, wie die Pakete tatsächlich heißen
/// (`docs/architecture/crypto-drift-report.md` §1).
pub const CRYPTO_AND_HTTP_STACK: &[&str] = &[
    "crypt_guard",
    "crypt_guard_core",
    "crypt_guard_service",
    "crypt_guard_hyper",
    "hyper",
    "tower",
    "harw-dod-encrypt",
];

/// Begründung der TCB-Regeln in [`FORBIDDEN_REACH`].
const TCB_NO_CRYPTO: &str = "Warden, Probes und Sensoren bleiben frei von CryptGuard, Hyper \
                             und Tower (Crypto-Masterplan v2 §20/§21)";

/// Die Hüllenregeln. Ein neuer Eintrag genügt.
pub const FORBIDDEN_REACH: &[ForbiddenReach] = &[
    ForbiddenReach {
        from: Pattern::Exact("harw-warden"),
        names: CRYPTO_AND_HTTP_STACK,
        rule: TCB_NO_CRYPTO,
    },
    ForbiddenReach {
        from: Pattern::Prefix("harw-dod-warden"),
        names: CRYPTO_AND_HTTP_STACK,
        rule: TCB_NO_CRYPTO,
    },
    ForbiddenReach {
        from: Pattern::Exact("harw-probe-fs"),
        names: CRYPTO_AND_HTTP_STACK,
        rule: TCB_NO_CRYPTO,
    },
    ForbiddenReach {
        from: Pattern::Exact("harw-probe-bpf"),
        names: CRYPTO_AND_HTTP_STACK,
        rule: TCB_NO_CRYPTO,
    },
    ForbiddenReach {
        from: Pattern::Sensor,
        names: CRYPTO_AND_HTTP_STACK,
        rule: TCB_NO_CRYPTO,
    },
    ForbiddenReach {
        from: Pattern::Exact("harw-dod"),
        names: &["harw-dod-encrypt"],
        rule: "die DoD-Fassade darf harw-dod-encrypt auch transitiv nicht erreichen \
               (Crypto-Masterplan v2 §3.3)",
    },
];

/// Crates, deren **vollständige Hülle** frei von Nebenwirkungen sein muss,
/// mit der Liste dessen, was dort nicht vorkommen darf. Die Hülle wird wie
/// bei [`FORBIDDEN_REACH`] mit [`hull_with_paths`] gebildet und folgt daher
/// auch über externe Crates hinweg `Cargo.lock`.
pub const PURE_CRATES: &[(&str, &[&str])] = &[(
    "harw-lens-rank",
    &[
        "jiff",
        "time",
        "chrono",
        "rand",
        "getrandom",
        "tokio",
        "reqwest",
        "fs4",
        "tempfile",
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
    let repo_root = Path::new(".");
    let graph = super::load_all_workspaces(repo_root)?;
    let lock = LockIndex::load(repo_root)?;
    Ok(evaluate_with_lock(&graph, Some(&lock)))
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
#[cfg(test)]
#[must_use]
pub fn evaluate(graph: &WorkspaceGraph) -> GateReport {
    evaluate_with_lock(graph, None)
}

/// Wie [`evaluate`], aber die [`FORBIDDEN_REACH`]-Hüllen folgen externen
/// Crates über `lock` weiter.
///
/// # Description
/// Ohne `lock` (`None`, nur für konstruierte Graphen in Tests) endet eine
/// Hülle an der ersten externen Crate, und ein externer Name ohne Eintrag
/// wird nicht gemeldet. Mit `lock` ist ein solcher Name ein Verstoß: eine
/// Hülle mit Loch prüft nicht, was sie zu prüfen behauptet. [`run`] ruft
/// immer mit `Some`.
///
/// # Arguments
/// - `graph`: der zu prüfende Abhängigkeitsgraph.
/// - `lock`: die Kanten aus `Cargo.lock`, oder `None`.
///
/// # Returns
/// Den Bericht; `checked` zählt Kanten, Reinheits-Hüllen-Einträge und die
/// Einträge jeder geprüften [`FORBIDDEN_REACH`]-Hülle.
#[must_use]
pub fn evaluate_with_lock(graph: &WorkspaceGraph, lock: Option<&LockIndex>) -> GateReport {
    let mut violations = Vec::new();
    let mut checked = 0usize;

    for node in &graph.crates {
        for dep in &node.deps {
            checked += 1;
            for edge in FORBIDDEN {
                if edge.from.matches(&node.name) && edge.to.matches(dep) && node.name != *dep {
                    violations.push(format!("{} → {} verletzt: {}", node.name, dep, edge.rule));
                }
            }
        }
    }

    let by_name: HashMap<&str, &CrateNode> =
        graph.crates.iter().map(|c| (c.name.as_str(), c)).collect();

    for (pure, forbidden_names) in PURE_CRATES {
        let Some(root) = by_name.get(pure) else {
            // Eine umbenannte oder entfernte Wurzel-Crate ist kein Grund,
            // die Regel klanglos zu überspringen — genau dann ist nicht
            // geprüft, was die Regel behauptet zu prüfen.
            violations.push(format!(
                "{pure} fehlt im Workspace-Graphen — die Reinheits-Hülle (L7) kann nicht \
                 geprüft werden"
            ));
            continue;
        };
        let hull = hull_with_paths(root, &by_name, lock);
        checked += hull.paths.len();
        for name in *forbidden_names {
            if let Some(path) = hull.paths.get(*name) {
                violations.push(format!(
                    "{pure} erreicht '{name}' über {} — die Crate muss frei von I/O, Zeit \
                     und Zufall bleiben (L7)",
                    path.join(" → ")
                ));
            }
        }
        for missing in &hull.unresolved {
            violations.push(format!(
                "{pure}: '{missing}' hat keinen Eintrag in Cargo.lock — die Reinheits-Hülle \
                 wäre unvollständig (L7)"
            ));
        }
    }

    for node in &graph.crates {
        for rule in FORBIDDEN_REACH {
            if !rule.from.matches(&node.name) {
                continue;
            }
            let hull = hull_with_paths(node, &by_name, lock);
            checked += hull.paths.len();
            for name in rule.names {
                if let Some(path) = hull.paths.get(*name) {
                    violations.push(format!(
                        "{} erreicht '{}' über {} verletzt: {}",
                        node.name,
                        name,
                        path.join(" → "),
                        rule.rule
                    ));
                }
            }
            for missing in &hull.unresolved {
                violations.push(format!(
                    "{}: '{missing}' hat keinen Eintrag in Cargo.lock — die Hülle wäre \
                     unvollständig ({})",
                    node.name, rule.rule
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

/// Eine Hülle samt kürzestem Pfad zu jedem erreichten Namen.
#[derive(Debug, Default)]
struct PathHull {
    /// Erreichter Name → Pfad ab der Wurzel (Wurzel zuerst, Name zuletzt).
    paths: HashMap<String, Vec<String>>,
    /// Externe Namen ohne `Cargo.lock`-Eintrag (nur mit `lock`).
    unresolved: BTreeSet<String>,
}

/// Breitensuche über normale Abhängigkeiten ab `root`.
///
/// # Description
/// Interne Knoten folgen `deps` und `external_deps` aus dem Graphen,
/// externe Knoten den Kanten in `lock` (falls vorhanden). Die Breitensuche
/// liefert den kürzesten Pfad, damit die Meldung zeigt, **über wen** ein
/// verbotener Name hereinkommt. `root` selbst gehört nicht zur Hülle.
fn hull_with_paths(
    root: &CrateNode,
    by_name: &HashMap<&str, &CrateNode>,
    lock: Option<&LockIndex>,
) -> PathHull {
    let mut hull = PathHull::default();
    let mut seen: HashSet<String> = HashSet::from([root.name.clone()]);
    let mut queue: VecDeque<Vec<String>> = VecDeque::from([vec![root.name.clone()]]);

    while let Some(path) = queue.pop_front() {
        let Some(current) = path.last() else {
            continue;
        };
        let children: Vec<String> = if let Some(node) = by_name.get(current.as_str()) {
            node.deps
                .iter()
                .chain(node.external_deps.iter())
                .cloned()
                .collect()
        } else if let Some(lock) = lock {
            if let Some(deps) = lock.deps_of(current) {
                deps.iter().cloned().collect()
            } else {
                hull.unresolved.insert(current.clone());
                Vec::new()
            }
        } else {
            Vec::new()
        };
        for child in children {
            if seen.insert(child.clone()) {
                let mut next = path.clone();
                next.push(child.clone());
                hull.paths.insert(child, next.clone());
                queue.push_back(next);
            }
        }
    }

    hull
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
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
            node(
                "harw-dod-thermal",
                &["harw-dod-readfs", "harw-dod-cap"],
                &[],
            ),
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
    fn test_evaluate_missing_pure_crate_root_is_a_violation() {
        // Eine umbenannte oder entfernte `harw-lens-rank`: kein stilles
        // Überspringen (vormals `continue`), sondern ein benannter Verstoß.
        let g = graph(vec![node("harw-unrelated", &[], &[])]);

        let report = evaluate(&g);

        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("harw-lens-rank fehlt im Workspace-Graphen")),
            "{:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_pure_crate_reaching_getrandom_through_lockfile_is_a_violation() -> TestResult
    {
        // `transitive_hull` folgte Cargo.lock nicht und hätte diese Kante
        // nicht gesehen: lens-rank → ext-a → getrandom, wobei `ext-a` selbst
        // kein Workspace-Crate ist.
        let g = graph(vec![node("harw-lens-rank", &[], &["ext-a"])]);

        // Ohne Lockfile endet die Hülle an `ext-a` — genau die Lücke aus dem
        // Befund.
        assert!(evaluate(&g).is_green());

        let lock_text = r#"
version = 4

[[package]]
name = "ext-a"
version = "1.0.0"
dependencies = ["getrandom"]

[[package]]
name = "getrandom"
version = "0.2.0"
"#;
        let report = evaluate_with_lock(&g, Some(&lock(lock_text)?));

        assert!(
            report.violations.iter().any(|v| v.contains(
                "harw-lens-rank erreicht 'getrandom' über harw-lens-rank → ext-a → getrandom"
            )),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_empty_graph_is_red_with_zero_checked() {
        let report = evaluate(&graph(Vec::new()));

        assert!(!report.is_green(), "leerer Graph prüft nichts (G-102)");
        assert_eq!(report.checked, 0);
    }

    fn lock(text: &str) -> TestResult<LockIndex> {
        LockIndex::parse(text).map_err(TestError::Unexpected)
    }

    /// `reqwest -> hyper`, `hyper -> tower-service`: der klassische Weg, auf
    /// dem Hyper unbemerkt in eine Hülle wandert.
    const LOCK_WITH_REQWEST: &str = r#"
version = 4

[[package]]
name = "reqwest"
version = "0.12.0"
dependencies = ["hyper"]

[[package]]
name = "hyper"
version = "1.0.0"
dependencies = ["tower-service"]

[[package]]
name = "tower-service"
version = "0.3.3"

[[package]]
name = "serde"
version = "1.0.0"
"#;

    #[test]
    fn test_evaluate_warden_reaching_crypt_guard_via_internal_crate_is_a_violation() {
        let g = graph(vec![
            node("harw-warden", &["harw-dod-warden-proto"], &[]),
            node("harw-dod-warden-proto", &[], &["crypt_guard_service"]),
        ]);

        let report = evaluate(&g);

        assert!(!report.is_green());
        assert!(
            report.violations.iter().any(|v| v.contains(
                "harw-warden erreicht 'crypt_guard_service' über \
                 harw-warden → harw-dod-warden-proto → crypt_guard_service"
            )),
            "die Meldung muss den ganzen Pfad nennen: {:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_sensor_reaching_hyper_through_lockfile_is_a_violation() -> TestResult {
        let g = graph(vec![node("harw-dod-cpu", &[], &["reqwest"])]);

        // Ohne Lockfile endet die Hülle an `reqwest` — genau die Lücke.
        assert!(evaluate(&g).is_green());

        let report = evaluate_with_lock(&g, Some(&lock(LOCK_WITH_REQWEST)?));
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("harw-dod-cpu → reqwest → hyper")),
            "{:?}",
            report.violations
        );
        // `tower-service` ist nicht `tower`: exakte Paketnamen.
        assert!(
            !report
                .violations
                .iter()
                .any(|v| v.contains("'tower-service'")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_probes_and_warden_crates_reaching_encrypt_are_violations() {
        for root in [
            "harw-probe-bpf",
            "harw-probe-fs",
            "harw-dod-warden",
            "harw-warden",
            "harw-dod-thermal",
        ] {
            let g = graph(vec![
                node(root, &["harw-dod-encrypt"], &[]),
                node("harw-dod-encrypt", &[], &["crypt_guard_service"]),
            ]);

            let report = evaluate(&g);

            for forbidden in ["'harw-dod-encrypt'", "'crypt_guard_service'"] {
                assert!(
                    report
                        .violations
                        .iter()
                        .any(|v| v.starts_with(root) && v.contains(forbidden)),
                    "{root} → {forbidden}: {:?}",
                    report.violations
                );
            }
        }
    }

    #[test]
    fn test_evaluate_facade_edge_to_encrypt_is_a_violation() {
        let g = graph(vec![
            node("harw-dod", &["harw-dod-encrypt"], &[]),
            node("harw-dod-encrypt", &[], &["crypt_guard_service"]),
        ]);

        let report = evaluate(&g);

        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("harw-dod → harw-dod-encrypt verletzt")),
            "direkte Kante: {:?}",
            report.violations
        );
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("harw-dod erreicht 'harw-dod-encrypt'")),
            "Hülle: {:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_facade_reaching_encrypt_transitively_is_a_violation() {
        let g = graph(vec![
            node("harw-dod", &["harw-dod-rules"], &[]),
            node("harw-dod-rules", &["harw-dod-encrypt"], &[]),
            node("harw-dod-encrypt", &[], &[]),
        ]);

        assert!(
            evaluate(&g)
                .violations
                .iter()
                .any(|v| v.contains("harw-dod → harw-dod-rules → harw-dod-encrypt")),
        );
    }

    #[test]
    fn test_evaluate_encrypt_and_hub_may_use_crypt_guard() -> TestResult {
        // Masterplan v2 §21 "Allowed": harw-dod-encrypt -> crypt_guard
        // service; ein Nicht-TCB-Dienst darf Hyper nutzen.
        let g = graph(vec![
            node("harw-dod-encrypt", &[], &["crypt_guard_service"]),
            node("harw-auth-hub", &["harw-dod-encrypt"], &["reqwest"]),
        ]);
        let lock_text = format!(
            "{LOCK_WITH_REQWEST}\n[[package]]\nname = \"crypt_guard_service\"\nversion = \"3.1.0\"\n"
        );

        let report = evaluate_with_lock(&g, Some(&lock(&lock_text)?));

        assert!(report.is_green(), "{:?}", report.violations);
        Ok(())
    }

    #[test]
    fn test_evaluate_unresolved_external_in_guarded_hull_is_a_violation() -> TestResult {
        let g = graph(vec![node("harw-warden", &[], &["serde", "tokio"])]);

        let report = evaluate_with_lock(&g, Some(&lock(LOCK_WITH_REQWEST)?));

        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("'tokio' hat keinen Eintrag in Cargo.lock")),
            "{:?}",
            report.violations
        );
        assert!(
            !report.violations.iter().any(|v| v.contains("'serde'")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_reach_rules_count_hull_entries() -> TestResult {
        let g = graph(vec![node("harw-probe-fs", &[], &["serde"])]);

        let report = evaluate_with_lock(&g, Some(&lock(LOCK_WITH_REQWEST)?));

        assert!(report.is_green(), "{:?}", report.violations);
        assert_eq!(
            report.checked, 1,
            "die Hüllenregel muss ihre Einträge zählen, sonst ist 'grün' von \
             'nichts geprüft' nicht zu unterscheiden"
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_pure_crate_unresolved_external_in_lockfile_is_a_violation() -> TestResult {
        // `ext-a` hat keinen Eintrag in `LOCK_WITH_REQWEST` — die Hülle wäre
        // an dieser Stelle unvollständig, also ein benannter Verstoß statt
        // eines stillen Endes.
        let g = graph(vec![node("harw-lens-rank", &[], &["ext-a"])]);

        let report = evaluate_with_lock(&g, Some(&lock(LOCK_WITH_REQWEST)?));

        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("harw-lens-rank: 'ext-a' hat keinen Eintrag in Cargo.lock")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_crypto_stack_names_every_crypt_guard_package_and_the_http_stack() {
        for name in [
            "crypt_guard",
            "crypt_guard_core",
            "crypt_guard_service",
            "crypt_guard_hyper",
            "hyper",
            "tower",
            "harw-dod-encrypt",
        ] {
            assert!(CRYPTO_AND_HTTP_STACK.contains(&name), "{name}");
        }
        for subject in [
            "harw-warden",
            "harw-dod-warden",
            "harw-dod-warden-proto",
            "harw-probe-fs",
            "harw-probe-bpf",
        ]
        .into_iter()
        .chain(SENSOR_CRATES.iter().copied())
        {
            assert!(
                FORBIDDEN_REACH
                    .iter()
                    .any(|r| r.from.matches(subject) && r.names == CRYPTO_AND_HTTP_STACK),
                "{subject} ohne Krypto-Hüllenregel"
            );
        }
    }

    #[test]
    fn test_pattern_sensor_does_not_match_vocabulary_crates() {
        assert!(Pattern::Sensor.matches("harw-dod-thermal"));
        assert!(!Pattern::Sensor.matches("harw-dod-cap"));
        assert!(!Pattern::Sensor.matches("harw-dod-signals"));
        assert!(!Pattern::Sensor.matches("harw-dod-readfs"));
    }
}
