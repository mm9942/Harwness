//! systemd-Unit-Beschreibung der vier DoD-Binaries (Knoten **AW7-03**).
//!
//! Spezifikationsquelle: der AW7-03-Auftrag selbst (kein Contract-Master-
//! Dokument existiert für diesen Knoten) sowie die vier gelandeten Binaries
//! `harw-sentinel`, `harw-probe-fs`, `harw-probe-bpf`, `harw-warden` — dieses
//! Modul liest keines ihrer Quelltexte zur Laufzeit, sondern beschreibt nur,
//! was ihre jeweilige Moduldokumentation als Erwartung an den Betrieb
//! festhält.
//!
//! # Die vier Klassen
//! | Binary | Klasse | Faehigkeit dieser Unit |
//! |---|---|---|
//! | `harw-sentinel` | unprivilegiert | keine (`CapabilityBoundingSet=` leer) |
//! | `harw-probe-fs` | `CAP_SYS_ADMIN` | genau `CAP_SYS_ADMIN` |
//! | `harw-probe-bpf` | `CAP_BPF` | genau `CAP_BPF` |
//! | `harw-warden` | systemd-Socket, kein Netz | `CAP_SYS_ADMIN` (Annahme, siehe unten) |
//!
//! Für `harw-warden` nennt die Auftragstabelle keine Capability — dieser
//! Knoten hat trotzdem eine gewählt (siehe die Begründung am Kopf von
//! `deploy/systemd/harw-warden.service`): das Binary schreibt in
//! `cgroup.freeze`/`cgroup.kill` unterhalb eines konfigurierbaren
//! `--cgroup-root`, potenziell außerhalb einer ihm selbst delegierten
//! cgroup-v2-Teilhierarchie — laut Kernel-Dokumentation verlangt das
//! `CAP_SYS_ADMIN` im initialen User-Namespace. Das ist eine **Annahme
//! dieses Knotens**, keine Vorgabe aus dem Auftrag.
//!
//! # Die Landlock-Asymmetrie (Entscheidung Nr. 4)
//! `harw-sentinel` **degradiert** bei fehlender Landlock-Unterstützung
//! (`SensorDegraded`-Ereignis, `harw-sentinel/src/sandbox.rs`) und läuft
//! ohne die zusätzliche Schranke weiter — sein Berechtigungsumfang ist von
//! vornherein leer, und ein Sammler, der gar nicht erst startet, verliert
//! jede Beobachtung. Die drei privilegierten Binaries (`harw-probe-fs`,
//! `harw-probe-bpf`, `harw-warden`) brechen dagegen **hart** ab, sobald
//! `landlock::RulesetStatus::FullyEnforced` nicht erreicht wird — ein
//! privilegierter Prozess ohne wirksame Selbstbeschränkung darf nicht
//! laufen. Diese Units erzwingen dieselbe Landlock-Vorbedingung nicht noch
//! einmal auf Unit-Ebene: das Binary selbst ist bereits der harte
//! Torwächter (siehe die jeweilige `landlock.rs`-Moduldoku).
//!
//! # Wohin die Units gehören
//! `/etc/systemd/system/` ist Systemverwaltung; `~/.config/systemd/user/`
//! ist eine Nutzerinstanz. Eine Nutzerinstanz **kann keine Capabilities
//! vergeben** — `AmbientCapabilities=`/`CapabilityBoundingSet=` existieren
//! dort nicht in einer Form, die dem Prozess tatsächlich eine erhöhte
//! Kernel-Fähigkeit gäbe. Die drei privilegierten Binaries brauchen daher
//! zwingend Systemunits (`/etc/systemd/system/`); der unprivilegierte
//! Sentinel bräuchte sie für sich genommen nicht, ist aber die Gegenstelle
//! der drei privilegierten Sonden über ein gemeinsames
//! `RuntimeDirectory=harw` unter `/run/` — ein Pfad, den nur eine
//! Systeminstanz für mehrere unterschiedliche Systemnutzer gemeinsam
//! bereitstellen kann. Alle vier Units dieses Knotens sind deshalb
//! Systemunits, unter `deploy/systemd/` im Repository-Wurzelverzeichnis
//! abgelegt.
//!
//! # Befund: `harw-install` arbeitet heute ausschließlich im Nutzerbereich
//! [`crate::service_systemd::SystemdServiceManager`] legt seine Unit
//! ausschließlich unter `~/.config/systemd/user/<name>.service` ab
//! (`unit_path()`, `systemctl --user …`) — das reicht für einen generischen
//! `ServiceSpec`-Dienst ohne Capability-Bedarf, aber **nicht** für die drei
//! privilegierten Binaries dieses Knotens: eine Nutzerinstanz kann ihnen
//! nie `CAP_SYS_ADMIN`/`CAP_BPF` verleihen. Dieser Knoten fügt deshalb
//! **keine** fünfte, konkurrierende Installationslogik hinzu — er trägt nur
//! die (statischen, geprüften) Unit-Texte unter `deploy/systemd/` und die
//! hier festgehaltene Zuordnungstabelle ([`UNIT_CLASSES`]). Eine spätere
//! Installationslogik für Systemunits (Schreiben nach
//! `/etc/systemd/system/`, `systemctl daemon-reload`, `systemctl enable`)
//! ist ein **eigener, hier nicht gebauter Knoten** — dieser Auftrag verbietet
//! ausdrücklich, irgendetwas zu installieren, zu aktivieren oder zu
//! starten.
//!
//! # Warum diese Units nicht über [`crate::service::ServiceSpec`] gerendert
//! werden
//! [`crate::service_systemd::render_systemd_unit`] kennt weder Capabilities
//! noch Landlock-Wurzeln noch Socket-Aktivierung — es rendert eine
//! einfache `Type=simple`-Unit aus Exec/WorkingDirectory/Env. Die vier
//! Units dieses Knotens haben eine feste, je Binary unterschiedliche
//! Rechtematrix, die keine Laufzeit-Eingabe eines Aufrufers verändern darf
//! (Rule 1 des Auftrags: „genau die Fähigkeiten, die ihre Klasse nennt —
//! und nicht mehr"). Sie sind deshalb als statische Textdateien unter
//! `deploy/systemd/` abgelegt, nicht als von einer generischen Funktion zur
//! Laufzeit zusammengesetzter Text — derselbe Grund, aus dem
//! [`crate::service::ServiceSpec`] hier absichtlich nicht wiederverwendet
//! wird.
//!
//! # Exportierte Typen
//! [`UnitClass`], [`UNIT_CLASSES`], [`parse_unit`], [`ParsedUnit`].
//!
//! # Concurrency
//! Reine, zustandslose Funktionen und `'static`-Daten; `Send + Sync`.
//!
//! # Fehler
//! Keine — [`parse_unit`] ist tolerant (fehlende Abschnitte/Schlüssel liefern
//! `None`/leere Mengen statt eines `Err`), weil dieses Modul ausschließlich
//! zur Prüfung bereits im Repository liegender, von Menschen geschriebener
//! Unit-Texte dient (siehe `tests` unten), nicht zur Fehlerbehandlung einer
//! Laufzeit-Eingabe.

use std::collections::BTreeSet;

/// Beschreibt eine der vier Berechtigungsklassen dieses Knotens.
///
/// # Description
/// Ein reiner, `'static` Werttyp — die Zuordnung Binary → Unit-Dateiname →
/// erwartete Fähigkeitsmenge, wie sie [`UNIT_CLASSES`] für jedes der vier
/// Binaries hält. `expected_capabilities` ist die Menge, die sowohl
/// `CapabilityBoundingSet=` als auch `AmbientCapabilities=` der
/// zugehörigen Unit **exakt** tragen müssen (siehe Moduldoku, Rule 1).
#[derive(Debug, Clone, Copy)]
pub struct UnitClass {
    /// Name des Binaries, das diese Unit betreibt.
    pub binary: &'static str,
    /// Dateiname der `.service`-Unit unter `deploy/systemd/`.
    pub service_file: &'static str,
    /// Exakte Menge der Linux-Capabilities dieser Klasse, ohne führendes
    /// `CAP_`-Präfix wegzulassen (z. B. `"CAP_SYS_ADMIN"`). Leer für die
    /// unprivilegierte Klasse.
    pub expected_capabilities: &'static [&'static str],
}

/// Die vier Klassen dieses Knotens, in der Reihenfolge der Auftragstabelle.
///
/// # Description
/// `harw-warden`s `CAP_SYS_ADMIN`-Eintrag ist eine Annahme dieses Knotens,
/// keine Vorgabe der Auftragstabelle (siehe Moduldoku, Abschnitt „Die vier
/// Klassen").
pub const UNIT_CLASSES: &[UnitClass] = &[
    UnitClass {
        binary: "harw-sentinel",
        service_file: "harw-sentinel.service",
        expected_capabilities: &[],
    },
    UnitClass {
        binary: "harw-probe-fs",
        service_file: "harw-probe-fs.service",
        expected_capabilities: &["CAP_SYS_ADMIN"],
    },
    UnitClass {
        binary: "harw-probe-bpf",
        service_file: "harw-probe-bpf.service",
        expected_capabilities: &["CAP_BPF"],
    },
    UnitClass {
        binary: "harw-warden",
        service_file: "harw-warden.service",
        expected_capabilities: &["CAP_SYS_ADMIN"],
    },
];

/// Eine geparste systemd-Unit: Abschnittsnamen auf ihre `Schlüssel=Wert`-
/// Zeilen abgebildet, in Auftrittsreihenfolge.
///
/// # Description
/// Absichtlich kein `HashMap`, damit doppelte Schlüssel innerhalb eines
/// Abschnitts (in systemd-Units ausdrücklich zulässig, z. B. mehrere
/// `Environment=`-Zeilen) nicht still verlorengehen — [`ParsedUnit::values`]
/// liefert alle Vorkommen eines Schlüssels in Auftrittsreihenfolge.
#[derive(Debug, Clone, Default)]
pub struct ParsedUnit {
    sections: Vec<(String, Vec<(String, String)>)>,
}

impl ParsedUnit {
    /// Prüft, ob ein Abschnitt (z. B. `"Service"`) mindestens einmal
    /// vorkommt.
    ///
    /// # Returns
    /// `true`, wenn `[section]` mindestens eine Zeile besitzt.
    #[must_use]
    pub fn has_section(&self, section: &str) -> bool {
        self.sections.iter().any(|(name, _)| name == section)
    }

    /// Liefert alle Werte eines Schlüssels innerhalb eines Abschnitts, in
    /// Auftrittsreihenfolge.
    ///
    /// # Arguments
    /// - `section` (`&str`): Abschnittsname ohne eckige Klammern.
    /// - `key` (`&str`): Schlüsselname links des `=`.
    ///
    /// # Returns
    /// Jeden Wert rechts eines `key=`-Vorkommens innerhalb `section`, mit
    /// führendem/nachgestelltem Leerraum entfernt.
    #[must_use]
    pub fn values(&self, section: &str, key: &str) -> Vec<&str> {
        self.sections
            .iter()
            .filter(|(name, _)| name == section)
            .flat_map(|(_, entries)| entries.iter())
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// Liefert den letzten Wert eines Schlüssels — die Zeile, die für
    /// systemd bei einer wiederholten skalaren Direktive gilt (spätere
    /// Zeilen überschreiben frühere, außer bei ausdrücklich
    /// akkumulierenden Direktiven wie `Environment=`).
    ///
    /// # Returns
    /// `Some(wert)`, wenn `key` in `section` mindestens einmal vorkommt;
    /// sonst `None`.
    #[must_use]
    pub fn last_value(&self, section: &str, key: &str) -> Option<&str> {
        self.values(section, key).into_iter().last()
    }

    /// Zählt die Zeilen in `section`, deren Schlüssel mit `prefix`
    /// beginnt.
    ///
    /// # Description
    /// Für die `[Socket]`-Prüfung: jede `Listen*=`-Direktive (`ListenStream=`,
    /// `ListenDatagram=`, `ListenSequentialPacket=`, `ListenFIFO=`, …) trägt
    /// je einen eigenen an `LISTEN_FDS` übergebenen Deskriptor.
    #[must_use]
    pub fn count_keys_with_prefix(&self, section: &str, prefix: &str) -> usize {
        self.sections
            .iter()
            .filter(|(name, _)| name == section)
            .flat_map(|(_, entries)| entries.iter())
            .filter(|(k, _)| k.starts_with(prefix))
            .count()
    }
}

/// Parst den Text einer systemd-Unit-Datei in Abschnitte und
/// `Schlüssel=Wert`-Zeilen.
///
/// # Description
/// Ein bewusst minimaler Parser für genau das Teilmenge-Format, das dieser
/// Knoten selbst schreibt: `[Abschnitt]`-Kopfzeilen, `Schlüssel=Wert`-
/// Zeilen, `#`- und `;`-Kommentarzeilen (systemd erlaubt beide
/// Kommentarzeichen), Leerzeilen. Keine Zeilenfortsetzung (`\`), kein
/// `%`-Specifier-Ersatz — für die vier statischen Units dieses Knotens
/// nicht benötigt.
///
/// # Arguments
/// - `text` (`&str`): der vollständige Unit-Text.
///
/// # Returns
/// Ein [`ParsedUnit`] mit jedem gefundenen Abschnitt und seinen Zeilen.
/// Zeilen vor dem ersten `[Abschnitt]`-Kopf werden verworfen (in einer
/// wohlgeformten Unit gibt es keine).
#[must_use]
pub fn parse_unit(text: &str) -> ParsedUnit {
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut current: Option<usize> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            sections.push((name.to_owned(), Vec::new()));
            current = Some(sections.len() - 1);
            continue;
        }
        let Some(index) = current else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        sections[index].1.push((key.trim().to_owned(), value.trim().to_owned()));
    }

    ParsedUnit { sections }
}

/// Zerlegt eine `AmbientCapabilities=`/`CapabilityBoundingSet=`-Werteliste
/// in eine Menge von Capability-Namen.
///
/// # Arguments
/// - `value` (`&str`): der rohe Zeilenwert, z. B. `"CAP_SYS_ADMIN"` oder
///   `""` (leer — keine Fähigkeit).
///
/// # Returns
/// Die whitespace-getrennten Tokens als [`BTreeSet`]; leer, wenn `value`
/// nur aus Leerraum besteht.
#[must_use]
pub fn capability_set(value: &str) -> BTreeSet<String> {
    value.split_whitespace().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::{capability_set, parse_unit, UNIT_CLASSES};

    const SENTINEL: &str = include_str!("../../deploy/systemd/harw-sentinel.service");
    const PROBE_FS: &str = include_str!("../../deploy/systemd/harw-probe-fs.service");
    const PROBE_BPF: &str = include_str!("../../deploy/systemd/harw-probe-bpf.service");
    const WARDEN_SERVICE: &str = include_str!("../../deploy/systemd/harw-warden.service");
    const WARDEN_SOCKET: &str = include_str!("../../deploy/systemd/harw-warden.socket");

    /// Ordnet jeder [`UNIT_CLASSES`]-Klasse ihren eingebetteten Unit-Text zu.
    fn service_text(service_file: &str) -> &'static str {
        match service_file {
            "harw-sentinel.service" => SENTINEL,
            "harw-probe-fs.service" => PROBE_FS,
            "harw-probe-bpf.service" => PROBE_BPF,
            "harw-warden.service" => WARDEN_SERVICE,
            other => panic!("kein eingebetteter Unit-Text für {other}"),
        }
    }

    #[test]
    fn test_every_service_unit_is_syntactically_well_formed() {
        for class in UNIT_CLASSES {
            let parsed = parse_unit(service_text(class.service_file));
            assert!(
                parsed.has_section("Unit"),
                "{} fehlt [Unit]",
                class.service_file
            );
            assert!(
                parsed.has_section("Service"),
                "{} fehlt [Service]",
                class.service_file
            );
            assert!(
                parsed.last_value("Service", "ExecStart").is_some(),
                "{} hat keine ExecStart=-Zeile",
                class.service_file
            );
        }
    }

    #[test]
    fn test_warden_socket_unit_is_syntactically_well_formed() {
        let parsed = parse_unit(WARDEN_SOCKET);
        assert!(parsed.has_section("Unit"));
        assert!(parsed.has_section("Socket"));
    }

    #[test]
    fn test_every_unit_grants_exactly_the_capabilities_of_its_class() {
        for class in UNIT_CLASSES {
            let parsed = parse_unit(service_text(class.service_file));
            let expected: std::collections::BTreeSet<String> =
                class.expected_capabilities.iter().map(|s| (*s).to_owned()).collect();

            let bounding = parsed
                .last_value("Service", "CapabilityBoundingSet")
                .map(capability_set)
                .unwrap_or_default();
            let ambient = parsed
                .last_value("Service", "AmbientCapabilities")
                .map(capability_set)
                .unwrap_or_default();

            assert_eq!(
                bounding, expected,
                "{}: CapabilityBoundingSet weicht von der Klasse ab",
                class.service_file
            );
            assert_eq!(
                ambient, expected,
                "{}: AmbientCapabilities weicht von der Klasse ab",
                class.service_file
            );
        }
    }

    #[test]
    fn test_no_unit_grants_a_capability_outside_its_class() {
        // Eigenständig von der Gleichheitsprüfung oben: prüft explizit die
        // Teilmengenbeziehung, damit ein künftiger Tippfehler (eine
        // zusätzliche Fähigkeit neben der erwarteten) hier unabhängig
        // auffällt, selbst wenn jemand die Gleichheitsprüfung oben
        // versehentlich lockert.
        for class in UNIT_CLASSES {
            let parsed = parse_unit(service_text(class.service_file));
            let expected: std::collections::BTreeSet<&str> =
                class.expected_capabilities.iter().copied().collect();

            for section_key in ["CapabilityBoundingSet", "AmbientCapabilities"] {
                let granted = parsed
                    .last_value("Service", section_key)
                    .map(capability_set)
                    .unwrap_or_default();
                for capability in &granted {
                    assert!(
                        expected.contains(capability.as_str()),
                        "{}: {section_key} gewährt {capability}, das die Klasse nicht nennt",
                        class.service_file
                    );
                }
            }
        }
    }

    #[test]
    fn test_warden_restricts_address_families_without_inet() {
        let parsed = parse_unit(WARDEN_SERVICE);
        let value = parsed
            .last_value("Service", "RestrictAddressFamilies")
            .expect("harw-warden.service muss RestrictAddressFamilies= setzen");
        let families = capability_set(value);
        assert!(
            !families.contains("AF_INET") && !families.contains("AF_INET6"),
            "harw-warden.service darf kein AF_INET/AF_INET6 zulassen (Gate: kein Netz im Warden)"
        );
        assert!(
            families.contains("AF_UNIX"),
            "harw-warden.service muss mindestens AF_UNIX zulassen (sein einziger Transport)"
        );
    }

    #[test]
    fn test_warden_socket_unit_produces_exactly_one_descriptor() {
        let parsed = parse_unit(WARDEN_SOCKET);
        let listen_directives = parsed.count_keys_with_prefix("Socket", "Listen");
        assert_eq!(
            listen_directives, 1,
            "harw-warden::systemd::acquire_listen_socket() verlangt hart genau einen \
             LISTEN_FDS-Eintrag — mehr als eine Listen*=-Zeile wäre ein Startfehler des Binaries"
        );
    }

    #[test]
    fn test_warden_socket_unit_uses_sequential_packet() {
        let parsed = parse_unit(WARDEN_SOCKET);
        assert!(
            parsed.last_value("Socket", "ListenSequentialPacket").is_some(),
            "harw-warden::ipc erwartet einen SOCK_SEQPACKET-Socket, keinen Stream-/Datagram-Socket"
        );
    }

    #[test]
    fn test_probes_order_after_the_sentinel_without_a_hard_requires() {
        for (service_file, text) in [
            ("harw-probe-fs.service", PROBE_FS),
            ("harw-probe-bpf.service", PROBE_BPF),
        ] {
            let parsed = parse_unit(text);
            assert!(
                parsed
                    .values("Unit", "After")
                    .iter()
                    .any(|v| v.contains("harw-sentinel.service")),
                "{service_file} muss nach harw-sentinel.service ordnen"
            );
            assert!(
                parsed.values("Unit", "Requires").is_empty(),
                "{service_file} darf harw-sentinel.service nicht über Requires= mitreissen \
                 (ein Sentinel-Neustart soll die Sonde nicht mit stoppen — siehe Moduldoku)"
            );
        }
    }

    #[test]
    fn test_warden_service_requires_its_own_socket() {
        let parsed = parse_unit(WARDEN_SERVICE);
        assert!(
            parsed
                .values("Unit", "Requires")
                .iter()
                .any(|v| v.contains("harw-warden.socket")),
            "harw-warden.service ist ohne seinen Socket bedeutungslos (acquire_listen_socket() \
             bricht sonst hart ab) und muss ihn deshalb über Requires= verlangen"
        );
    }

    #[test]
    fn test_warden_service_does_not_protect_control_groups() {
        // Die im Auftrag ausdrücklich benannte Falle: ProtectControlGroups=yes
        // würde /sys/fs/cgroup durch eine leere, private Instanz ersetzen und
        // damit genau den Pfad wegnehmen, den harw-warden laut
        // --cgroup-root braucht.
        let parsed = parse_unit(WARDEN_SERVICE);
        let value = parsed.last_value("Service", "ProtectControlGroups");
        assert_ne!(
            value,
            Some("yes"),
            "harw-warden.service darf ProtectControlGroups=yes nicht setzen — das nimmt dem \
             Binary seinen einzigen Schreibbereich (--cgroup-root) weg"
        );
    }

    #[test]
    fn test_unit_classes_reference_distinct_binaries() {
        let binaries: std::collections::BTreeSet<&str> =
            UNIT_CLASSES.iter().map(|class| class.binary).collect();
        assert_eq!(binaries.len(), UNIT_CLASSES.len(), "jede Klasse muss ein eigenes Binary nennen");
    }

    #[test]
    fn test_parse_unit_ignores_comments_and_blank_lines() {
        let parsed = parse_unit("# Kommentar\n\n[Unit]\n; ebenfalls Kommentar\nDescription=x\n");
        assert_eq!(parsed.last_value("Unit", "Description"), Some("x"));
    }

    #[test]
    fn test_capability_set_splits_on_whitespace() {
        let set = capability_set("CAP_SYS_ADMIN CAP_BPF");
        assert!(set.contains("CAP_SYS_ADMIN") && set.contains("CAP_BPF"));
    }

    #[test]
    fn test_capability_set_of_empty_string_is_empty() {
        assert!(capability_set("").is_empty());
        assert!(capability_set("   ").is_empty());
    }
}
