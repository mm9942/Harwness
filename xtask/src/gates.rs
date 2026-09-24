//! Struktur- und Berechtigungsprüfungen über den Abhängigkeitsgraphen.
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt ausschließlich die *Verteilung* auf die einzelnen
//! Gates. Jedes Gate lebt in einer eigenen Datei, damit drei Arbeitsknoten sie
//! unabhängig füllen können, ohne sich eine Datei zu teilen:
//!
//! - [`edges`] — verbotene Kanten im Abhängigkeitsgraphen (Knoten AW0-10a)
//! - [`privileges`] — Privilegienbudget je Binary (Knoten AW0-10b)
//!
//! Ein früheres drittes Gate prüfte die Schreibbereichstabelle des
//! Ausbauplans auf Kollisionen paralleler Arbeitsknoten. Mit dem Abschluss des
//! Ausbauprogramms (siehe `docs/design/build-history.md`) entfiel die Tabelle
//! und damit dieses Gate.
//!
//! # Warum die Aufteilung
//! Der ursprüngliche Zuschnitt hatte alle drei Gates in einer Datei. Er ist
//! daran gescheitert, dass ein einzelner Arbeitsschritt drei unabhängige
//! Prüfungen samt Markdown-Zerlegung tragen sollte — zu viel für einen
//! Knoten. Die Aufteilung ist dieselbe Bewegung wie bei
//! [`crate::gates`]/[`crate::webui`] eine Ebene höher: eine Datei je Besitzer.
//!
//! # Was ein Gate schuldet
//! Jedes Gate meldet, **wie viele** Kandidaten es geprüft hat — nicht nur, ob
//! es grün ist. Ein Gate, das schweigend nichts prüft, ist von einem grünen
//! nicht zu unterscheiden, und genau diese Verwechslung hat dieses Projekt
//! mehrfach gefunden.
//!
//! # Stand
//! Verteiler aus Knoten AW0-00/AW0-10; die drei Gates entstehen in AW0-10a,
//! AW0-10b und AW0-10c.

use std::path::Path;

use harw_code_graph::WorkspaceGraph;

#[path = "gate_edges.rs"]
pub mod edges;
#[path = "gate_privileges.rs"]
pub mod privileges;
// Die zwei Warden-Gates aus dem Verifikationsplan, nachgetragen: sie standen
// als Abnahmebestandteil im Plan und waren nie gebaut worden. Beide teilen
// eine Huellenberechnung, sind aber einzeln aufrufbar, weil sie verschiedene
// Fragen stellen — Anzahl gegen Bauart.
#[path = "gate_warden.rs"]
pub mod warden;

/// Unterverzeichnis des eigenständigen DoD-Workspace, relativ zur Repo-Wurzel.
///
/// # Description
/// `dod/` ist ein eigener Cargo-Workspace (eigenes `Cargo.lock`, eigene
/// `[workspace.dependencies]`) und steht in der Wurzel-`Cargo.toml` unter
/// `exclude`. Dort liegen alle vier Binaries, die `privileges` und die
/// Warden-Gates prüfen, und alle Sensor-Crates, auf die sich die Regeln von
/// `edges` beziehen.
pub const DOD_WORKSPACE: &str = "dod";

/// Lädt Produkt- und DoD-Workspace als **einen** Graphen.
///
/// # Description
/// Vor dieser Funktion lud jedes Gate nur `.`. Seit DoD ein eigener
/// Workspace ist, fehlten damit alle vier Binaries und alle Sensoren im
/// Graphen: die Sensor- und Warden-Regeln von `edges` prüften still nichts,
/// `privileges` meldete die Binaries als „Gerüst", und die Warden-Gates
/// fanden ihre Wurzel nicht. Die DoD-Crates hängen per `path` an
/// Produkt-Crates (`harw-types`, `harw-macros`, …); erst
/// [`WorkspaceGraph::load_many`] macht diese Kanten zu internen Kanten, damit
/// die Hüllen über die Workspace-Grenze hinweg weiterlaufen.
///
/// # Arguments
/// - `repo_root` (`&Path`): Wurzel des Repositorys (die Produkt-`Cargo.toml`).
///
/// # Errors
/// Wenn einer der beiden Workspaces nicht lesbar ist. Ein fehlendes `dod/`
/// ist ein Fehler, kein leiser Rückfall auf den Produkt-Workspace allein —
/// genau dieser Rückfall hat die Gates zuvor blind gemacht.
pub fn load_all_workspaces(repo_root: &Path) -> Result<WorkspaceGraph, String> {
    let dod_root = repo_root.join(DOD_WORKSPACE);
    WorkspaceGraph::load_many(&[repo_root, dod_root.as_path()]).map_err(|error| {
        format!(
            "Workspace-Graph (Produkt '{}' + DoD '{}') nicht lesbar: {error}",
            repo_root.display(),
            dod_root.display()
        )
    })
}

/// Das Ergebnis eines einzelnen Gates.
///
/// # Description
/// Trägt neben dem Urteil auch die **Zahl der geprüften Kandidaten**. Ein Gate
/// ohne diese Zahl kann nicht zwischen „nichts verletzt" und „nichts geprüft"
/// unterscheiden — und der zweite Fall ist der gefährliche.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateReport {
    /// Name des Gates, wie er auf der Kommandozeile steht.
    pub name: &'static str,
    /// Wie viele Kandidaten geprüft wurden.
    pub checked: usize,
    /// Die gefundenen Verstöße, je einer als lesbare Zeile mit dem
    /// verletzten Paar oder der verletzten Kante.
    pub violations: Vec<String>,
}

impl GateReport {
    /// Ob dieses Gate grün ist.
    ///
    /// # Description
    /// Grün heißt **zwei** Dinge zugleich: keine Verstöße *und* mindestens
    /// ein geprüfter Kandidat. Vor dieser Korrektur (Befund G-102) genügte
    /// ein leerer `violations`-Vektor allein — ein Gate, das aus einem
    /// stillen Fehler heraus nichts prüft (`checked == 0`), meldete sich
    /// dadurch als grün, obwohl es gar keine Aussage getroffen hat. Das ist
    /// exakt die Verwechslung, gegen die `checked` laut Moduldoku eingeführt
    /// wurde — sie muss auch hier gelten, nicht nur beim Anzeigen der
    /// Zusammenfassung.
    #[must_use]
    pub fn is_green(&self) -> bool {
        self.checked > 0 && self.violations.is_empty()
    }

    /// Die Zusammenfassung, wie sie ausgegeben wird.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.is_green() {
            format!("{}: grün ({} geprüft)", self.name, self.checked)
        } else if self.checked == 0 {
            // R3-05: eigener Zweig für den G-102-Fall, statt in die
            // generische Verstoßzahl-Meldung zu fallen — „0 Verstöße bei 0
            // geprüften Kandidaten" liest sich wie ein Erfolg, ist aber
            // genau der stille Nicht-Prüf-Zustand, den `is_green` jetzt rot
            // meldet.
            format!("{}: rot – nichts geprüft (checked == 0)", self.name)
        } else {
            format!(
                "{}: {} Verstöße bei {} geprüften Kandidaten",
                self.name,
                self.violations.len(),
                self.checked
            )
        }
    }
}

/// Führt die Gates aus.
///
/// # Arguments
/// - `args` (`&[String]`): Namen einzelner Gates (`edges`, `privileges`,
///   `warden-deps`, `warden-cbuild`); leer bedeutet alle.
///
/// # Returns
/// `Ok(())`, wenn jedes ausgeführte Gate grün ist.
///
/// # Errors
/// Eine Beschreibung aller fehlgeschlagenen Gates, je Verstoß eine Zeile mit
/// der verletzten Regel und dem konkreten Paar — nicht nur „Gate rot".
/// Ein unbekannter Gate-Name ist ebenfalls ein Fehler: im CI soll ein
/// Tippfehler rot werden, nicht still alles überspringen.
pub fn run(args: &[String]) -> Result<(), String> {
    let selected: Vec<&str> = if args.is_empty() {
        vec!["edges", "privileges", "warden-deps", "warden-cbuild"]
    } else {
        args.iter().map(String::as_str).collect()
    };

    let mut reports = Vec::new();
    for name in selected {
        let report = match name {
            "edges" => edges::run()?,
            "privileges" => privileges::run()?,
            "warden-deps" => warden::dependency_budget::run()?,
            "warden-cbuild" => warden::c_build::run()?,
            other => return Err(format!("unbekanntes Gate '{other}'")),
        };
        println!("{}", report.summary());
        reports.push(report);
    }

    let failures: Vec<&GateReport> = reports.iter().filter(|r| !r.is_green()).collect();
    if failures.is_empty() {
        return Ok(());
    }

    Err(failure_message(&failures))
}

/// Baut die `Err`-Meldung aus den rot gemeldeten Gates.
///
/// # Description
/// Getrennt von [`run`], damit der G-102-Fall (`checked == 0` ohne
/// Verstöße) ohne einen echten, dateisystemabhängigen Gate-Lauf getestet
/// werden kann. Ohne den `checked == 0`-Zweig bliebe die Verstoß-Schleife
/// für ein solches Gate leer, und `Err` trüge eine leere Zeile — ein Fehler
/// ohne jede Aussage, genau der stille Zustand, den G-102 sichtbar machen
/// soll (R3-05).
#[must_use]
fn failure_message(failures: &[&GateReport]) -> String {
    let mut message = String::new();
    for report in failures {
        if report.checked == 0 {
            message.push_str(&format!(
                "Gate {} hat nichts geprüft (checked == 0)\n",
                report.name
            ));
        }
        for violation in &report.violations {
            message.push_str(&format!("{}: {violation}\n", report.name));
        }
    }
    message.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::GateReport;
    use crate::test_support::{TestError, TestResult};

    /// Wurzel des Repositorys, unabhängig vom Arbeitsverzeichnis des
    /// Testlaufs (Cargo startet Tests im Paketverzeichnis `xtask/`).
    fn repo_root() -> TestResult<&'static std::path::Path> {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing("Repo-Wurzel über xtask/"))
    }

    /// Der Befund, der diese Tests ausgelöst hat: die Gates luden nur `.`,
    /// fanden die vier DoD-Binaries nicht und prüften still nichts. Gegen
    /// den **echten** Graphen, nicht gegen ein Fixture.
    #[test]
    fn test_load_all_workspaces_finds_all_monitored_binaries() -> TestResult {
        let graph = super::load_all_workspaces(repo_root()?).map_err(TestError::Unexpected)?;

        assert!(
            super::privileges::missing_binaries(&graph).is_empty(),
            "alle vier Binaries müssen im zusammengeführten Graphen liegen: fehlend {:?}",
            super::privileges::missing_binaries(&graph)
        );
        let warden = graph
            .get(super::warden::WARDEN_ROOT)
            .ok_or(TestError::Missing("harw-warden im Graphen"))?;
        assert!(
            warden.deps.iter().any(|dep| dep == "harw-types"),
            "die Pfad-Kante DoD → Produkt muss intern sein, sonst endet jede Hülle an der Workspace-Grenze: {:?}",
            warden.deps
        );
        Ok(())
    }

    #[test]
    fn test_edges_on_real_graph_sees_every_rule_subject() -> TestResult {
        let graph = super::load_all_workspaces(repo_root()?).map_err(TestError::Unexpected)?;

        // Jede Regel nennt Crates beim Namen. Fehlt einer davon im Graphen,
        // prüft die Regel still nichts — genau der frühere Zustand.
        for sensor in super::edges::SENSOR_CRATES {
            assert!(
                graph.get(sensor).is_some(),
                "Sensor '{sensor}' fehlt im Graphen"
            );
        }
        for (pure, _) in super::edges::PURE_CRATES {
            assert!(
                graph.get(pure).is_some(),
                "reine Crate '{pure}' fehlt im Graphen"
            );
        }
        for name in [
            "harw-sentinel",
            "harw-warden",
            "harw-dod-sentinel",
            "harw-dod-escalate",
        ] {
            assert!(
                graph.get(name).is_some(),
                "Regel-Subjekt '{name}' fehlt im Graphen"
            );
        }

        let report = super::edges::evaluate(&graph);
        assert!(report.checked > 0, "{report:?}");
        Ok(())
    }

    #[test]
    fn test_privileges_on_real_graph_checks_all_four_binaries() -> TestResult {
        let graph = super::load_all_workspaces(repo_root()?).map_err(TestError::Unexpected)?;

        let report = super::privileges::evaluate(&graph);

        assert_eq!(
            report.checked,
            super::privileges::MONITORED_BINARIES.len(),
            "{report:?}"
        );
        Ok(())
    }

    /// Befund G-102: ein Gate, das nichts geprüft hat (`checked == 0`),
    /// darf nicht als grün gelten — auch wenn `violations` zufällig leer
    /// ist, weil niemand hineingeschrieben hat.
    #[test]
    fn test_is_green_empty_report_is_red() {
        let report = GateReport {
            name: "test-gate",
            checked: 0,
            violations: Vec::new(),
        };
        assert!(
            !report.is_green(),
            "ein leerer Report (checked == 0) darf nicht grün sein"
        );
    }

    #[test]
    fn test_is_green_checked_without_violations_is_green() {
        let report = GateReport {
            name: "test-gate",
            checked: 5,
            violations: Vec::new(),
        };
        assert!(report.is_green());
    }

    #[test]
    fn test_is_green_with_violations_is_red_regardless_of_checked() {
        let report = GateReport {
            name: "test-gate",
            checked: 5,
            violations: vec!["irgendein Verstoß".to_owned()],
        };
        assert!(!report.is_green());
    }

    /// R3-05: `summary()` braucht für den G-102-Fall (`checked == 0`, keine
    /// Verstöße) eine eigene, lesbare Zeile statt „0 Verstöße bei 0
    /// geprüften Kandidaten" — letzteres liest sich wie ein Erfolg.
    #[test]
    fn test_summary_zero_checked_without_violations_says_nothing_checked() {
        let report = GateReport {
            name: "test-gate",
            checked: 0,
            violations: Vec::new(),
        };
        let summary = report.summary();
        assert!(
            summary.contains("nichts geprüft"),
            "Zusammenfassung sollte den G-102-Fall benennen: {summary:?}"
        );
        assert!(!summary.contains("grün"), "{summary:?}");
    }

    #[test]
    fn test_summary_with_violations_still_reports_violation_count() {
        let report = GateReport {
            name: "test-gate",
            checked: 3,
            violations: vec!["irgendein Verstoß".to_owned()],
        };
        assert_eq!(
            report.summary(),
            "test-gate: 1 Verstöße bei 3 geprüften Kandidaten"
        );
    }

    /// R3-05: `run()`s Fehlermeldung darf für ein rotes Gate mit
    /// `checked == 0` und ohne Verstöße nicht leer bleiben — ein Fehler
    /// ohne jede Aussage verdeckt genau den Zustand, den G-102 aufdecken
    /// soll. Getestet über [`super::failure_message`] statt über `run()`
    /// selbst, weil `run()` echte, dateisystemabhängige Gates aufruft.
    #[test]
    fn test_failure_message_zero_checked_without_violations_is_not_empty() {
        let report = GateReport {
            name: "leeres-gate",
            checked: 0,
            violations: Vec::new(),
        };
        let message = super::failure_message(&[&report]);
        assert!(
            !message.is_empty(),
            "Meldung darf bei checked == 0 nicht leer sein"
        );
        assert!(message.contains("leeres-gate"), "{message:?}");
        assert!(message.contains("nichts geprüft"), "{message:?}");
    }

    #[test]
    fn test_failure_message_with_violations_lists_each_violation() {
        let report = GateReport {
            name: "rotes-gate",
            checked: 2,
            violations: vec!["Verstoß A".to_owned(), "Verstoß B".to_owned()],
        };
        let message = super::failure_message(&[&report]);
        assert!(message.contains("rotes-gate: Verstoß A"), "{message:?}");
        assert!(message.contains("rotes-gate: Verstoß B"), "{message:?}");
        assert!(!message.contains("nichts geprüft"), "{message:?}");
    }
}
