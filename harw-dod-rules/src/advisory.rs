//! Advisory-Korrelation gegen das tatsächlich gebaute `Cargo.lock` (Knoten
//! AW7-06, Intel-Scout).
//!
//! # Warum der Scout keine Advisories holt
//! [`correlate_advisories`] nimmt `advisories` als Parameter entgegen — es
//! gibt in diesem Modul keinen Aufruf, der irgendein Advisory selbst
//! *beschafft*. Das Beschaffen (ein RUSTSEC-Feed, eine OSV-Abfrage, ein
//! Datenbank-Snapshot) ist eine andere Sache mit anderen Rechten: sie
//! bräuchte Netzzugriff, dieser Knoten liegt aber auf dem Weg zum Warden
//! (Budget nach K53: 12 Crates in der Laufzeit-Hülle, ohne Dev-, Build- und
//! Proc-Macro-Deps) und trägt deshalb ausdrücklich **keine** HTTP-Crate.
//! `harw-dod-rules/Cargo.toml` listet nur Pfad-Abhängigkeiten innerhalb des
//! Workspace plus `jiff` und `semver` — keine davon spricht ein Netzwerk an
//! (siehe `test_dependency_list_contains_no_network_crate` unten, das genau
//! das gegen den Manifest-Text prüft, statt es nur zu behaupten). Ein
//! Intel-Scout, der Advisories selbst holt, wäre ein Scout mit
//! Netzrechten — die Trennung zwischen "beschaffen" und "korrelieren" ist
//! genau die Grenze, die dieses Modul nicht überschreitet.
//!
//! # Wie der Versionsbereich verglichen wird — und warum nicht als Zeichenkette
//! [`Advisory::vulnerable_ranges`] trägt [`semver::VersionReq`], nicht einen
//! rohen String, den [`correlate_advisories`] selbst parsen oder gar
//! lexikografisch vergleichen müsste. Der klassische Fehler eines
//! String-Vergleichs: `"1.10.0" < "1.9.0"` ist als Zeichenkette wahr (der
//! zweite Byte-Vergleich `'1' < '9'` entscheidet), als Version aber falsch —
//! `1.10.0` ist die *neuere* Fassung. Ein Advisory-Bereich wie `"<1.9.0"`
//! (die Lücke ist mit `1.9.0` geschlossen) dürfte für eine gebaute `1.10.0`
//! deshalb **nicht** auslösen; ein Join, der das als String prüft, würde
//! einen Fehlalarm auf eine bereits gepatchte Fassung melden — genau der
//! Fall, der laut Auftrag den Betrieb lehrt, Meldungen zu ignorieren. Diese
//! Funktion parst `locked.version` einmal über [`semver::Version::parse`]
//! und übergibt das Ergebnis an `VersionReq::matches`, das intern korrekt
//! nach Haupt-, Neben- und Patch-Version vergleicht (siehe
//! `test_1_10_0_does_not_match_range_excluding_1_9_0` unten). Ein Advisory
//! kann mehrere, disjunkte Bereiche tragen (z. B. "vor 1.9.0 ODER zwischen
//! 1.10.0 und 1.10.5") — `semver::VersionReq` selbst kennt nur UND-verknüpfte
//! Vergleiche innerhalb eines Bereichs, deshalb ist
//! [`Advisory::vulnerable_ranges`] ein `Vec`: ein Treffer in irgendeinem
//! Eintrag genügt.
//!
//! # Was bei mehrfach vorhandenen Fassungen passiert
//! Dieselbe Crate kann in `Cargo.lock` mehrfach stehen — mit unterschiedlichen
//! Hauptversionen, die als getrennte Abhängigkeitsbäume koexistieren.
//! [`harw_code_graph::lockfile::find_locked`] liefert dafür ausdrücklich nur
//! den *ersten* Treffer nach Namen (siehe dessen Dokumentation) — für einen
//! Advisory-Join ist das die Falle, vor der der Arbeitsauftrag warnt: stünde
//! die unverwundbare Fassung zuerst im Lockfile, würde ein Join, der sich auf
//! `find_locked` allein verließe, die tatsächlich verwundbare, weiter unten
//! stehende Fassung nie sehen und schweigen, obwohl eine echte Verwundbarkeit
//! gebaut wurde. [`correlate_advisories`] benutzt `find_locked` deshalb nur
//! als billigen Vorab-Check ("gibt es diese Crate überhaupt im Lockfile?"),
//! nie als den eigentlichen Join: die Übereinstimmung selbst iteriert über
//! **alle** Einträge in `locked` mit passendem Namen
//! (`locked.iter().filter(...)`) und meldet jede gebaute Fassung, die in
//! einen der Advisory-Bereiche fällt — unabhängig davon, an welcher Position
//! sie im Lockfile steht (siehe
//! `test_vulnerable_version_found_even_when_a_safe_version_of_the_same_crate_comes_first`).
//!
//! # Determinismus
//! [`correlate_advisories`] sortiert ihr Ergebnis ausdrücklich nach
//! `(crate_name, version, advisory_id)`, bevor sie es zurückgibt — dieselbe
//! Eingabe (dieselben Advisories, dasselbe Lockfile) ergibt deshalb immer
//! dieselbe Reihenfolge, unabhängig von der internen Iterationsreihenfolge
//! über `advisories` und `locked`. Die je Fund zufällig vergebene
//! [`harw_types::FindingId`] (siehe [`crate::engine::run_rules`]-Moduldoku für
//! dieselbe Begründung) ist davon ausgenommen — sie ist per Konstruktion bei
//! jedem Aufruf verschieden; der Determinismus-Test dieses Moduls vergleicht
//! deshalb die inhaltlichen Felder (Reihenfolge, Zusammenfassung, Anzahl),
//! nicht volle `Finding`-Gleichheit.
//!
//! # Kein Netz, keine Systemuhr
//! Wie jede Regel dieser Crate liest [`correlate_advisories`] `now` nur aus
//! dem übergebenen Argument, nie über `jiff::Timestamp::now()` — siehe
//! [`crate::rule`]-Moduldoku für die Begründung der Reinheitsauflage. Diese
//! Funktion ist bewusst **kein** [`crate::rule::Rule`]: [`crate::rule::RuleContext`]
//! trägt `HostSample`/`SecurityEvent`/`NetworkScope` — das Vokabular der
//! Host-Beobachtung, dem Advisories und Lockfile-Einträge fremd sind. Eine
//! Erweiterung von `RuleContext` um diese Felder hätte jeden bestehenden
//! Aufrufer (`harw-dod-escalate` eingeschlossen) zum Anpassen gezwungen — ein
//! nicht-additiver Eingriff, den der Arbeitsauftrag ausdrücklich ausschließt.
//! Diese Funktion zertifiziert ihre Befunde deshalb direkt über dieselben
//! `pub(crate)`-Konstruktoren, die auch [`crate::engine::run_rules`] benutzt
//! ([`Finding::raw`], [`Finding::check`]) — kein zweiter, nicht sanktionierter
//! Weg zu `Finding<RuleChecked>`, sondern derselbe, innerhalb derselben Crate.
//!
//! # Ein Befund, keine Aktion
//! [`correlate_advisories`] liefert `Vec<Finding<RuleChecked>>` — geprüfte,
//! aber noch nicht triagierte Befunde. Was mit ihnen geschieht (eskalieren,
//! verwerfen, zur Prüfung vormerken), entscheidet ausschließlich
//! `harw-dod-escalate` über [`crate::triage`]. Dieses Modul trifft diese
//! Entscheidung nicht.
//!
//! # Nebenläufigkeit
//! [`correlate_advisories`] liest ihre Argumente nur (`&`), schreibt nirgends
//! geteilten Zustand: sicher aus mehreren Threads mit unterschiedlichen
//! Argumenten aufrufbar.
//!
//! # Fehler
//! Keine eigenen Fehler. Ein Lockfile-Eintrag, dessen `version`-Feld sich
//! nicht als [`semver::Version`] parsen lässt, wird stillschweigend
//! übersprungen statt einen Fehler auszulösen — `Cargo.lock` wird von Cargo
//! selbst geschrieben und enthält laut seinem Format immer eine gültige
//! semantische Version; ein defekter Eintrag wäre ein Datenintegritätsproblem
//! außerhalb dessen, was ein Advisory-Join sinnvoll melden könnte.
//!
//! # Examples
//! ```rust
//! use harw_dod_rules::advisory::{Advisory, correlate_advisories};
//! use harw_code_graph::lockfile::LockedPackage;
//! use harw_dod_signals::Severity;
//! use semver::VersionReq;
//!
//! let locked = vec![LockedPackage {
//!     name: "example-crate".to_owned(),
//!     version: "1.9.0".to_owned(),
//!     source: None,
//!     checksum: None,
//! }];
//! let advisories = vec![Advisory {
//!     id: "RUSTSEC-2024-0001".to_owned(),
//!     crate_name: "example-crate".to_owned(),
//!     vulnerable_ranges: vec![VersionReq::parse("<1.10.0").unwrap()],
//!     severity: Severity::High,
//!     summary: "Beispiel-Advisory".to_owned(),
//! }];
//!
//! let findings = correlate_advisories(&advisories, &locked, jiff::Timestamp::UNIX_EPOCH);
//! assert_eq!(findings.len(), 1);
//! ```

use harw_code_graph::lockfile::{LockedPackage, find_locked};
use harw_dod_signals::{Hardness, Severity};
use harw_types::FindingId;
use jiff::Timestamp;
use semver::{Version, VersionReq};

use crate::finding::{Finding, FindingKind, RuleChecked};

/// Die stabile Regel-Kennung dieses Joins.
///
/// # Description
/// Übernimmt jeden von [`correlate_advisories`] erzeugten
/// [`Finding::rule_id`], analog zu [`crate::rule::Rule::id`] bei einer
/// klassischen Regel — diese Funktion ist bewusst keine, siehe Moduldoku.
pub const RULE_ID: &str = "advisory-correlate";

/// Eine hereingereichte Sicherheitsmeldung: eine Crate und die von ihr
/// betroffenen Versionsbereiche.
///
/// # Description
/// Trägt ausschließlich, was zum Join gegen [`LockedPackage`] nötig ist.
/// Woher `id`/`summary`/`severity` stammen (ein RUSTSEC-Eintrag, ein
/// OSV-Datensatz, …) ist Sache des Aufrufers — siehe Moduldoku, warum dieses
/// Modul selbst nichts beschafft.
///
/// # Errors
/// Keine eigenen Fehler.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advisory {
    /// Stabile Kennung der Meldung (z. B. eine RUSTSEC-ID).
    pub id: String,
    /// Name der betroffenen Crate, exakt wie in `Cargo.lock`.
    pub crate_name: String,
    /// Die betroffenen Versionsbereiche. Ein Treffer in irgendeinem Eintrag
    /// genügt — siehe Moduldoku, warum dies ein `Vec` und kein einzelner
    /// [`VersionReq`] ist.
    pub vulnerable_ranges: Vec<VersionReq>,
    /// Wie schwer diese Meldung wiegt.
    pub severity: Severity,
    /// Menschenlesbare Zusammenfassung der Meldung.
    pub summary: String,
}

impl Advisory {
    // Prüft, ob `version` in mindestens einen der betroffenen Bereiche
    // fällt. Rein, total innerhalb dieses Moduls.
    fn matches(&self, version: &Version) -> bool {
        self.vulnerable_ranges.iter().any(|range| range.matches(version))
    }
}

/// Korreliert hereingereichte Advisories gegen ein bereits geparstes
/// `Cargo.lock`.
///
/// # Description
/// Siehe Moduldoku für die vollständige Begründung: kein Netz, korrekte
/// semantische Versionsprüfung statt Zeichenkettenvergleich, und
/// Berücksichtigung mehrfach vorhandener Fassungen derselben Crate. Für jede
/// `advisories`-Meldung wird zunächst per [`find_locked`] billig geprüft, ob
/// die genannte Crate überhaupt im Lockfile vorkommt; bei einem Treffer
/// durchsucht diese Funktion **alle** Einträge in `locked` mit passendem
/// Namen — nicht nur den ersten — und meldet jede gebaute Fassung, die in
/// einen der Advisory-Bereiche fällt.
///
/// # Arguments
/// - `advisories` (`&[Advisory]`): die zu prüfenden Meldungen, hereingereicht
///   vom Aufrufer (siehe Moduldoku: dieses Modul beschafft keine).
/// - `locked` (`&[LockedPackage]`): das bereits über
///   [`harw_code_graph::lockfile::parse_lockfile`] geparste Lockfile.
/// - `now` (`jiff::Timestamp`): injizierte Zeit für `observed_at` — nie die
///   Systemuhr dieser Funktion selbst.
///
/// # Returns
/// Einen zertifizierten Befund je (Advisory, gebaute Fassung)-Treffer,
/// sortiert nach `(crate_name, version, advisory.id)` für Determinismus.
/// Leer, wenn keine gebaute Fassung in einen Advisory-Bereich fällt.
///
/// # Errors
/// Keine — siehe Moduldoku.
///
/// # Examples
/// Siehe Moduldoku.
#[must_use]
pub fn correlate_advisories(
    advisories: &[Advisory],
    locked: &[LockedPackage],
    now: Timestamp,
) -> Vec<Finding<RuleChecked>> {
    let mut matches: Vec<(&LockedPackage, &Advisory)> = advisories
        .iter()
        .filter(|advisory| find_locked(locked, &advisory.crate_name).is_some())
        .flat_map(|advisory| {
            locked
                .iter()
                .filter(move |pkg| pkg.name == advisory.crate_name)
                // `move`, weil der innere Abschluss das aeussere `advisory` ueber
                // die Lebenszeit von `flat_map` hinaus traegt. Ohne ihn borgt er
                // eine Variable, die mit dem Aufruf endet (E0373).
                .filter_map(move |pkg| {
                    let version = Version::parse(&pkg.version).ok()?;
                    advisory.matches(&version).then_some((pkg, advisory))
                })
        })
        .collect();

    matches.sort_by(|(pkg_a, adv_a), (pkg_b, adv_b)| {
        (pkg_a.name.as_str(), pkg_a.version.as_str(), adv_a.id.as_str())
            .cmp(&(pkg_b.name.as_str(), pkg_b.version.as_str(), adv_b.id.as_str()))
    });

    matches
        .into_iter()
        .map(|(pkg, advisory)| {
            let raw = Finding::raw(
                RULE_ID,
                FindingKind::RuleTriggered,
                advisory.severity,
                Hardness::Correlated,
                format!(
                    "{}@{} matches advisory {} ({})",
                    pkg.name, pkg.version, advisory.id, advisory.summary
                ),
                now,
            );
            raw.check(FindingId::new())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, version: &str) -> LockedPackage {
        LockedPackage {
            name: name.to_owned(),
            version: version.to_owned(),
            source: None,
            checksum: None,
        }
    }

    fn advisory(id: &str, crate_name: &str, ranges: &[&str], severity: Severity) -> Advisory {
        Advisory {
            id: id.to_owned(),
            crate_name: crate_name.to_owned(),
            vulnerable_ranges: ranges
                .iter()
                .map(|r| VersionReq::parse(r).expect("test range parses"))
                .collect(),
            severity,
            summary: "test advisory".to_owned(),
        }
    }

    #[test]
    fn test_advisory_matching_built_version_yields_a_finding() {
        let locked = vec![pkg("evil-crate", "1.2.0")];
        let advisories = vec![advisory("RUSTSEC-0001", "evil-crate", &["<1.3.0"], Severity::High)];

        let findings = correlate_advisories(&advisories, &locked, Timestamp::UNIX_EPOCH);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, RULE_ID);
        assert_eq!(findings[0].kind, FindingKind::RuleTriggered);
        assert_eq!(findings[0].severity, Severity::High);
        assert!(findings[0].summary.contains("evil-crate@1.2.0"));
        assert!(findings[0].summary.contains("RUSTSEC-0001"));
    }

    #[test]
    fn test_advisory_not_matching_built_version_yields_no_finding() {
        let locked = vec![pkg("safe-crate", "2.0.0")];
        let advisories = vec![advisory("RUSTSEC-0002", "safe-crate", &["<1.0.0"], Severity::High)];

        assert!(correlate_advisories(&advisories, &locked, Timestamp::UNIX_EPOCH).is_empty());
    }

    #[test]
    fn test_1_10_0_does_not_match_range_excluding_1_9_0() {
        // Der Test, der einen Zeichenkettenvergleich ausschliesst:
        // "1.10.0" < "1.9.0" ist als String wahr, als Version falsch.
        let locked = vec![pkg("stringy-crate", "1.10.0")];
        let advisories =
            vec![advisory("RUSTSEC-0003", "stringy-crate", &["<1.9.0"], Severity::Critical)];

        assert!(
            correlate_advisories(&advisories, &locked, Timestamp::UNIX_EPOCH).is_empty(),
            "1.10.0 ist neuer als 1.9.0 und darf den Bereich \"<1.9.0\" nicht treffen"
        );
    }

    #[test]
    fn test_vulnerable_version_found_even_when_a_safe_version_of_the_same_crate_comes_first() {
        // `find_locked` allein liefert nur den ersten Treffer (hier: die
        // unverwundbare 2.0.0) — dieser Test belegt, dass der Join trotzdem
        // die weiter unten stehende, tatsächlich verwundbare 1.0.0 findet.
        let locked = vec![pkg("dual-version-crate", "2.0.0"), pkg("dual-version-crate", "1.0.0")];
        let advisories =
            vec![advisory("RUSTSEC-0004", "dual-version-crate", &["<1.5.0"], Severity::Medium)];

        let findings = correlate_advisories(&advisories, &locked, Timestamp::UNIX_EPOCH);

        assert_eq!(findings.len(), 1);
        assert!(findings[0].summary.contains("dual-version-crate@1.0.0"));
    }

    #[test]
    fn test_result_is_sorted_deterministically() {
        let locked = vec![pkg("zeta-crate", "1.0.0"), pkg("alpha-crate", "1.0.0")];
        let advisories = vec![
            advisory("RUSTSEC-Z", "zeta-crate", &["<2.0.0"], Severity::Low),
            advisory("RUSTSEC-A", "alpha-crate", &["<2.0.0"], Severity::Low),
        ];

        let first = correlate_advisories(&advisories, &locked, Timestamp::UNIX_EPOCH);
        let second = correlate_advisories(&advisories, &locked, Timestamp::UNIX_EPOCH);

        let names_first: Vec<&str> = first.iter().map(|f| f.summary.as_str()).collect();
        let names_second: Vec<&str> = second.iter().map(|f| f.summary.as_str()).collect();

        assert_eq!(first.len(), 2);
        assert!(names_first[0].starts_with("alpha-crate"), "alpha vor zeta");
        assert!(names_first[1].starts_with("zeta-crate"));
        // Determinismus: dieselbe Reihenfolge bei zwei Aufrufen mit
        // identischer Eingabe (die FindingId selbst ist je Aufruf frisch und
        // zufällig, siehe Moduldoku — deshalb Vergleich über den Inhalt).
        assert_eq!(names_first, names_second);
    }

    #[test]
    fn test_unparseable_locked_version_is_skipped_without_error() {
        let locked = vec![pkg("weird-crate", "not-a-version")];
        let advisories = vec![advisory("RUSTSEC-0005", "weird-crate", &["<9.9.9"], Severity::Low)];

        assert!(correlate_advisories(&advisories, &locked, Timestamp::UNIX_EPOCH).is_empty());
    }

    /// Liefert die Abhängigkeits-Schlüssel (Crate-Namen) aus den
    /// **laufzeitrelevanten** Tabellen `[dependencies]` und
    /// `[build-dependencies]` eines Cargo-Manifests als Text.
    ///
    /// Bewusst zeilenweise statt mit einem TOML-Parser: `harw-dod-rules`
    /// hat keine `toml`-Dev-Dependency (die Crate wurde gerade erst von 21
    /// Einträgen befreit), und für die hier nötige Prüfung — welche
    /// Crate-Namen stehen als Schlüssel in welcher Tabelle — reicht eine
    /// einfache Zeilenzerlegung. Kommentare (`#…`) werden vor der Analyse
    /// abgeschnitten, damit ein erklärender Kommentar (der z. B. den Namen
    /// einer *entfernten* Crate nennt) keinen Treffer erzeugt. `[dev-
    /// dependencies]` wird absichtlich **ausgeschlossen**: Code dort landet
    /// nie im Binary (siehe Kommentar zu `harw-knowledge` in
    /// `Cargo.toml`), ist also für die "kein Netz-Stack im unprivilegierten
    /// Sentinel-Binary"-Aussage irrelevant.
    fn runtime_dependency_keys(manifest: &str) -> Vec<String> {
        let mut in_runtime_table = false;
        let mut keys = Vec::new();
        for raw_line in manifest.lines() {
            let line = raw_line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') {
                in_runtime_table = line == "[dependencies]" || line == "[build-dependencies]";
                continue;
            }
            if !in_runtime_table {
                continue;
            }
            if let Some((key, _rest)) = line.split_once('=') {
                keys.push(key.trim().trim_matches('"').to_owned());
            }
        }
        keys
    }

    #[test]
    fn test_dependency_list_contains_no_network_crate() {
        // Vorherige Fassung dieses Tests durchsuchte den **Volltext** der
        // `Cargo.toml` nach verbotenen Substrings. Das schlug fehl, sobald
        // jemand dokumentierte, *dass* eine Netz-Crate entfernt wurde: der
        // Kommentar zu `harw-knowledge` in `[dev-dependencies]` erklärt
        // ausdrücklich, dass die `harw-knowledge`-Kette (inkl.
        // `harw-model-catalog` → `reqwest`/`tokio`) aus den produktiven
        // Abhängigkeiten herausgehalten wurde — genau diese Erklärung
        // enthält das Wort "reqwest" und ließ den alten Volltext-Test
        // anschlagen, obwohl `reqwest` nirgends als Abhängigkeit steht.
        // Dasselbe Muster wie bei K51 (`grep` nach `Surface::` traf auch
        // `InvocationSurface::`) und K61 (Suche nach `0.2.0` traf
        // `sd-listen-fds = "0.2.0"`): eine Volltextsuche über strukturierten
        // Text prüft nicht, was sie behauptet.
        //
        // Geprüft wird jetzt: die Abhängigkeits-**Schlüssel** (Crate-Namen)
        // aus den laufzeitrelevanten Tabellen `[dependencies]` und
        // `[build-dependencies]` — nicht der Dateitext, nicht `[dev-
        // dependencies]`. `harw-knowledge` steht bewusst nur unter `[dev-
        // dependencies]` (für einen Doctest) und zieht dort keinen Code ins
        // Binary; eine Netz-Crate an dieser Stelle wäre also unschädlich für
        // die geschützte Aussage "kein Netz-Stack im Sentinel-Binary" und
        // wird deshalb nicht geprüft. Ein `toml`-Parser wäre präziser
        // gewesen, ist aber keine Dev-Dependency dieser Crate — die
        // zeilenweise Prüfung genügt für die hier nötige Aussage über
        // Tabellen-Zugehörigkeit von Top-Level-Schlüsseln.
        let manifest = include_str!("../Cargo.toml");
        const FORBIDDEN: &[&str] =
            &["reqwest", "hyper", "ureq", "curl", "isahc", "surf", "tokio-tungstenite"];

        let keys = runtime_dependency_keys(manifest);
        for needle in FORBIDDEN {
            assert!(
                !keys.iter().any(|key| key == needle),
                "harw-dod-rules/Cargo.toml darf keine Netz-Crate wie '{needle}' in \
                 [dependencies] oder [build-dependencies] auflisten"
            );
        }
    }

    #[test]
    fn test_dependency_list_rejects_real_network_dependency() {
        // Gegenprobe: eine *echte* Netz-Abhängigkeit unter [dependencies]
        // muss weiterhin auffallen — sonst wäre die Präzisierung eine
        // Abschwächung statt einer Korrektur.
        let manifest = "[package]\nname = \"fake\"\n\n[dependencies]\nreqwest = \"1\"\n";
        let keys = runtime_dependency_keys(manifest);
        assert!(keys.iter().any(|key| key == "reqwest"));
    }

    #[test]
    fn test_dependency_list_ignores_dev_dependency_network_crate() {
        // Entscheidung dieser Aufgabe belegt: eine Netz-Crate unter
        // [dev-dependencies] landet nicht im Binary und wird daher nicht
        // als Verstoß gegen "kein Netz im Sentinel" gewertet.
        let manifest =
            "[package]\nname = \"fake\"\n\n[dev-dependencies]\nreqwest = \"1\"\n";
        let keys = runtime_dependency_keys(manifest);
        assert!(!keys.iter().any(|key| key == "reqwest"));
    }

    #[test]
    fn test_dependency_list_comment_mentioning_forbidden_crate_is_not_a_match() {
        // Der konkrete Fall, der den alten Volltext-Test auslöste: ein
        // erklärender Kommentar, der den Namen einer entfernten Netz-Crate
        // nennt, darf keinen Treffer erzeugen.
        let manifest = "[package]\nname = \"fake\"\n\n[dependencies]\n\
            # frueher hier: reqwest, jetzt entfernt\n\
            harw-types = { path = \"../harw-types\" }\n";
        let keys = runtime_dependency_keys(manifest);
        assert!(!keys.iter().any(|key| key == "reqwest"));
    }
}
