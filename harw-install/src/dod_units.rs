//! systemd-Unit-Beschreibung der vier DoD-Binaries (Knoten **AW7-03**,
//! abgeglichen in **H10** des Crypto-Masterplans v2).
//!
//! Die Unit-Texte selbst liegen ausschließlich unter `deploy/systemd/` und
//! werden über [`crate::deployment::DEPLOYMENT_ASSETS`] im Produktionscode
//! eingebettet (`include_str!`, nicht nur unter `cfg(test)`). Dieses Modul
//! trägt die Rechtematrix ([`UNIT_CLASSES`]) und einen minimalen Parser
//! ([`parse_unit`]); seine Tests prüfen genau den eingebetteten Text, den
//! `harw install --print-systemd` ausgibt und `dod/scripts/install.sh` aus
//! `deploy/` installiert. Die frühere zweite Quelle
//! `dod/packaging/systemd/*` ist mit H10 entfallen.
//!
//! # Die vier Klassen
//! | Binary | Klasse | Fähigkeiten dieser Unit |
//! |---|---|---|
//! | `harw-sentinel` | unprivilegiert | keine (`CapabilityBoundingSet=` leer) |
//! | `harw-probe-fs` | fanotify | genau `CAP_SYS_ADMIN` |
//! | `harw-probe-bpf` | eBPF | genau `CAP_BPF CAP_PERFMON` |
//! | `harw-warden` | systemd-Socket, kein IP | genau `CAP_DAC_OVERRIDE CAP_NET_ADMIN` |
//!
//! **H10-Entscheidungen** (jeweils die strengere Variante der beiden
//! früheren Quellen, am Code begründet):
//! - `harw-probe-bpf`: `CAP_PERFMON` ist nötig, nicht optional —
//!   `harw-dod-bpf/src/real.rs::has_attach_capabilities` verweigert das
//!   Anheften ohne `CAP_BPF` **und** `CAP_PERFMON` (Tracepoint-/FEntry-
//!   Anheftung über `perf_event_open`).
//! - `harw-warden`: eigener Systemnutzer statt `root`. Er schreibt
//!   `cgroup.freeze`/`cgroup.kill` (root-eigen, 0644; cgroup v2 prüft dort
//!   die Dateirechte → `CAP_DAC_OVERRIDE`, nicht `CAP_SYS_ADMIN`) und ruft
//!   für die Netzisolation `/usr/sbin/nft` als Kindprozess auf
//!   (`harw-warden/src/isolation.rs` → `CAP_NET_ADMIN`, über die
//!   Ambient-Menge vererbt). Die frühere Annahme `CAP_SYS_ADMIN` entfällt.
//!
//! # Die Landlock-Asymmetrie (Entscheidung Nr. 4)
//! `harw-sentinel` **degradiert** bei fehlender Landlock-Unterstützung
//! (`SensorDegraded`-Ereignis, `harw-sentinel/src/sandbox.rs`) und läuft
//! ohne die zusätzliche Schranke weiter. Die drei privilegierten Binaries
//! (`harw-probe-fs`, `harw-probe-bpf`, `harw-warden`) brechen dagegen
//! **hart** ab, sobald `landlock::RulesetStatus::FullyEnforced` nicht
//! erreicht wird. Die Units erzwingen diese Vorbedingung nicht noch einmal;
//! das Binary selbst ist der harte Torwächter.
//!
//! # Wohin die Units gehören
//! Ausschließlich Systemunits (`/etc/systemd/system/` bzw.
//! `$prefix/lib/systemd/system/`): eine Nutzerinstanz kann keine
//! Capabilities vergeben und keinen für mehrere Systemnutzer gemeinsamen
//! `/run/harw`-Pfad bereitstellen. [`crate::service_systemd`] kennt nur
//! Nutzerunits; die Systeminstallation übernimmt `dod/scripts/install.sh`
//! (DoD) bzw. der Betreiber mit `harw install --print-systemd` (Infra).
//!
//! # Warum diese Units nicht über [`crate::service::ServiceSpec`] gerendert
//! werden
//! [`crate::service_systemd::render_systemd_unit`] kennt weder
//! Capabilities noch Socket-Aktivierung. Die Rechtematrix ist fest und darf
//! von keiner Laufzeit-Eingabe verändert werden; deshalb sind die Units
//! statische, geprüfte Dateien unter `deploy/systemd/`.
//!
//! # Exportierte Typen
//! [`UnitClass`], [`UNIT_CLASSES`], [`parse_unit`], [`ParsedUnit`],
//! [`capability_set`].
//!
//! # Concurrency
//! Reine, zustandslose Funktionen und `'static`-Daten; `Send + Sync`.
//!
//! # Fehler
//! Keine — [`parse_unit`] ist tolerant (fehlende Abschnitte/Schlüssel liefern
//! `None`/leere Mengen statt eines `Err`), weil es ausschließlich
//! von Menschen geschriebene, im Repository liegende Unit-Texte prüft.

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
/// Die Fähigkeitsmengen von `harw-probe-bpf` und `harw-warden` sind die
/// H10-Entscheidungen (siehe Moduldoku, Abschnitt „Die vier Klassen").
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
        expected_capabilities: &["CAP_BPF", "CAP_PERFMON"],
    },
    UnitClass {
        binary: "harw-warden",
        service_file: "harw-warden.service",
        expected_capabilities: &["CAP_DAC_OVERRIDE", "CAP_NET_ADMIN"],
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
        sections[index]
            .1
            .push((key.trim().to_owned(), value.trim().to_owned()));
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
    use super::{UNIT_CLASSES, capability_set, parse_unit};
    use crate::deployment::systemd_unit;
    use crate::test_support::{TestError, TestResult};

    /// Der eingebettete Produktionstext einer Unit aus `deploy/systemd/`
    /// (dieselbe Quelle wie `harw install --print-systemd`).
    fn service_text(service_file: &str) -> TestResult<&'static str> {
        systemd_unit(service_file)
            .map(|asset| asset.contents)
            .ok_or_else(|| {
                TestError::Unexpected(format!("kein eingebetteter Unit-Text für {service_file}"))
            })
    }

    #[test]
    fn test_every_service_unit_is_syntactically_well_formed() -> TestResult {
        for class in UNIT_CLASSES {
            let parsed = parse_unit(service_text(class.service_file)?);
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
        Ok(())
    }

    #[test]
    fn test_warden_socket_unit_is_syntactically_well_formed() -> TestResult {
        let parsed = parse_unit(service_text("harw-warden.socket")?);
        assert!(parsed.has_section("Unit"));
        assert!(parsed.has_section("Socket"));
        Ok(())
    }

    #[test]
    fn test_every_unit_grants_exactly_the_capabilities_of_its_class() -> TestResult {
        for class in UNIT_CLASSES {
            let parsed = parse_unit(service_text(class.service_file)?);
            let expected: std::collections::BTreeSet<String> = class
                .expected_capabilities
                .iter()
                .map(|s| (*s).to_owned())
                .collect();

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
        Ok(())
    }

    #[test]
    fn test_no_unit_grants_a_capability_outside_its_class() -> TestResult {
        // Eigenständig von der Gleichheitsprüfung oben: prüft explizit die
        // Teilmengenbeziehung, damit ein künftiger Tippfehler (eine
        // zusätzliche Fähigkeit neben der erwarteten) hier unabhängig
        // auffällt, selbst wenn jemand die Gleichheitsprüfung oben
        // versehentlich lockert.
        for class in UNIT_CLASSES {
            let parsed = parse_unit(service_text(class.service_file)?);
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
        Ok(())
    }

    #[test]
    fn test_warden_restricts_address_families_without_inet() -> TestResult {
        let parsed = parse_unit(service_text("harw-warden.service")?);
        let value = parsed
            .last_value("Service", "RestrictAddressFamilies")
            .ok_or(TestError::Missing(
                "harw-warden.service muss RestrictAddressFamilies= setzen",
            ))?;
        let families = capability_set(value);
        assert!(
            !families.contains("AF_INET") && !families.contains("AF_INET6"),
            "harw-warden.service darf kein AF_INET/AF_INET6 zulassen (Gate: kein Netz im Warden)"
        );
        assert!(
            families.iter().all(|f| f == "AF_UNIX" || f == "AF_NETLINK"),
            "harw-warden.service: nur AF_UNIX (Aktivierungssocket) und AF_NETLINK \
             (nft-Kindprozess) sind begründet, gefunden: {families:?}"
        );
        assert!(
            families.contains("AF_UNIX"),
            "harw-warden.service muss mindestens AF_UNIX zulassen (sein einziger Transport)"
        );
        Ok(())
    }

    #[test]
    fn test_warden_socket_unit_produces_exactly_one_descriptor() -> TestResult {
        let parsed = parse_unit(service_text("harw-warden.socket")?);
        let listen_directives = parsed.count_keys_with_prefix("Socket", "Listen");
        assert_eq!(
            listen_directives, 1,
            "harw-warden::systemd::acquire_listen_socket() verlangt hart genau einen \
             LISTEN_FDS-Eintrag — mehr als eine Listen*=-Zeile wäre ein Startfehler des Binaries"
        );
        Ok(())
    }

    #[test]
    fn test_warden_socket_unit_uses_sequential_packet() -> TestResult {
        let parsed = parse_unit(service_text("harw-warden.socket")?);
        assert!(
            parsed
                .last_value("Socket", "ListenSequentialPacket")
                .is_some(),
            "harw-warden::ipc erwartet einen SOCK_SEQPACKET-Socket, keinen Stream-/Datagram-Socket"
        );
        Ok(())
    }

    #[test]
    fn test_probes_order_after_the_sentinel_without_a_hard_requires() -> TestResult {
        for service_file in ["harw-probe-fs.service", "harw-probe-bpf.service"] {
            let parsed = parse_unit(service_text(service_file)?);
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
        Ok(())
    }

    #[test]
    fn test_warden_service_requires_its_own_socket() -> TestResult {
        let parsed = parse_unit(service_text("harw-warden.service")?);
        assert!(
            parsed
                .values("Unit", "Requires")
                .iter()
                .any(|v| v.contains("harw-warden.socket")),
            "harw-warden.service ist ohne seinen Socket bedeutungslos (acquire_listen_socket() \
             bricht sonst hart ab) und muss ihn deshalb über Requires= verlangen"
        );
        Ok(())
    }

    #[test]
    fn test_warden_service_does_not_protect_control_groups() -> TestResult {
        // Die im Auftrag ausdrücklich benannte Falle: ProtectControlGroups=yes
        // würde /sys/fs/cgroup durch eine leere, private Instanz ersetzen und
        // damit genau den Pfad wegnehmen, den harw-warden laut
        // --cgroup-root braucht.
        let parsed = parse_unit(service_text("harw-warden.service")?);
        let value = parsed.last_value("Service", "ProtectControlGroups");
        assert_ne!(
            value,
            Some("yes"),
            "harw-warden.service darf ProtectControlGroups=yes nicht setzen — das nimmt dem \
             Binary seinen einzigen Schreibbereich (--cgroup-root) weg"
        );
        Ok(())
    }

    #[test]
    fn test_unit_classes_reference_distinct_binaries() {
        let binaries: std::collections::BTreeSet<&str> =
            UNIT_CLASSES.iter().map(|class| class.binary).collect();
        assert_eq!(
            binaries.len(),
            UNIT_CLASSES.len(),
            "jede Klasse muss ein eigenes Binary nennen"
        );
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
