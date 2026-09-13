//! Struktur- und Berechtigungsprüfungen über den Abhängigkeitsgraphen.
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt ausschließlich die *Verteilung* auf die einzelnen
//! Gates. Jedes Gate lebt in einer eigenen Datei, damit drei Arbeitsknoten sie
//! unabhängig füllen können, ohne sich eine Datei zu teilen:
//!
//! - [`edges`] — verbotene Kanten im Abhängigkeitsgraphen (Knoten AW0-10a)
//! - [`privileges`] — Privilegienbudget je Binary (Knoten AW0-10b)
//! - [`writescopes`] — Schreibbereichstabelle aus dem Plan (Knoten AW0-10c)
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

#[path = "gate_edges.rs"]
pub mod edges;
#[path = "gate_privileges.rs"]
pub mod privileges;
#[path = "gate_writescopes.rs"]
pub mod writescopes;
// Die zwei Warden-Gates aus dem Verifikationsplan, nachgetragen: sie standen
// als Abnahmebestandteil im Plan und waren nie gebaut worden. Beide teilen
// eine Huellenberechnung, sind aber einzeln aufrufbar, weil sie verschiedene
// Fragen stellen — Anzahl gegen Bauart.
#[path = "gate_warden.rs"]
pub mod warden;

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
    #[must_use]
    pub fn is_green(&self) -> bool {
        self.violations.is_empty()
    }

    /// Die Zusammenfassung, wie sie ausgegeben wird.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.is_green() {
            format!("{}: grün ({} geprüft)", self.name, self.checked)
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
///   `writescopes`); leer bedeutet alle.
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
        vec!["edges", "privileges", "writescopes", "warden-deps", "warden-cbuild"]
    } else {
        args.iter().map(String::as_str).collect()
    };

    let mut reports = Vec::new();
    for name in selected {
        let report = match name {
            "edges" => edges::run()?,
            "privileges" => privileges::run()?,
            "writescopes" => writescopes::run()?,
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

    let mut message = String::new();
    for report in failures {
        for violation in &report.violations {
            message.push_str(&format!("{}: {violation}\n", report.name));
        }
    }
    Err(message.trim_end().to_owned())
}
