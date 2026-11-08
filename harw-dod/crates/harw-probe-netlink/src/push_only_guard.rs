//! Prüft die Push-Only-Zusage am eigenen Quelltext, nicht am Verhalten.
//!
//! # Verantwortungsbereich
//! Dies ist die Datei, auf die `main.rs`, `sink.rs` und die übrige
//! Moduldokumentation verweisen: ein späterer Leser bestätigt die
//! Push-Only-Zusage dieser Crate, indem er genau diese kurze Datei liest.
//! Dieselbe Bauweise wie `harw-probe-bpf/src/push_only_guard.rs` — jede
//! privilegierte Sonde trägt ihre eigene, im eigenen Quelltext geprüfte
//! Kopie.
//!
//! # Was „push-only" bei dieser Sonde heißt
//! Diese Sonde **liest** den Audit-Strom des Kernels — das ist ihr Zweck.
//! Dieser Lesevorgang liegt aber nicht in dieser Crate, sondern in
//! `harw_dod_netlink::socket::NetlinkAuditSource` (Mitgliedschaft in der
//! nur lesenden Multicast-Gruppe `AUDIT_NLGRP_READLOG`; kein Kommandokanal,
//! keine Audit-Regeln, kein Audit-Daemon). Gegenüber dem **Sentinel** gilt
//! dieselbe Zusage wie bei den anderen Sonden: nur `connect()` und `send()`,
//! nie ein Empfang. Deshalb darf keine Implementierungsdatei dieser Crate
//! die Unix-Socket-Empfangsfunktion oder das Einlesen eines Puffers bis zum
//! Streamende nennen.
//!
//! # Wie die Prüfung funktioniert
//! [`tests::test_no_socket_receive_path_exists_in_source`] bettet den
//! Quelltext jeder anderen Implementierungsdatei über `include_str!` ein und
//! sucht die beiden Zeichenketten. [`tests::test_every_implementation_file_is_scanned`]
//! hält die Liste vollständig: es listet `src/` zur Testzeit auf und
//! scheitert, sobald eine `*.rs`-Datei außer dieser Datei und
//! `test_support.rs` fehlt.
//!
//! **Diese Datei selbst ist bewusst nicht Teil der durchsuchten Liste:** die
//! gesuchten Zeichenketten stehen hier als Literale. Aus demselben Grund
//! vermeidet jede *andere* Datei dieser Crate sie auch in Prosa.
//!
//! # Exportierte Typen
//! Keine — ausschließlich die Tests in [`tests`].
//!
//! # Nebenläufigkeit
//! Nicht anwendbar; reiner Testcode.
//!
//! # Fehler
//! Nicht anwendbar.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use crate::test_support::{TestResult, ctx};

    /// Jede Implementierungsdatei dieser Crate außer dieser Testdatei selbst.
    const SOURCE_FILES: &[(&str, &str)] = &[
        ("main.rs", include_str!("main.rs")),
        ("cli.rs", include_str!("cli.rs")),
        ("error.rs", include_str!("error.rs")),
        ("landlock.rs", include_str!("landlock.rs")),
        ("collect.rs", include_str!("collect.rs")),
        ("sink.rs", include_str!("sink.rs")),
    ];

    /// Dateien in `src/`, die bewusst nicht in [`SOURCE_FILES`] stehen.
    const EXEMPT_FILES: &[&str] = &["push_only_guard.rs", "test_support.rs"];

    /// Die beiden Bezeichner, die zusammen einen Empfangspfad ausmachen
    /// würden: die Unix-Socket-Empfangsfunktion (`rustix::net::…`) und die
    /// Funktion, die einen Puffer bis zum Streamende einliest
    /// (`std::io::Read::…`).
    const FORBIDDEN_SUBSTRINGS: &[&str] = &["recv", "read_to_end"];

    #[test]
    fn test_no_socket_receive_path_exists_in_source() {
        for &(name, contents) in SOURCE_FILES {
            for &forbidden in FORBIDDEN_SUBSTRINGS {
                assert!(
                    !contents.contains(forbidden),
                    "gefunden {forbidden:?} in {name}: ein Empfangspfad ist für diese Sonde verboten"
                );
            }
        }
    }

    #[test]
    fn test_source_files_list_is_not_empty() {
        assert!(!SOURCE_FILES.is_empty());
    }

    #[test]
    fn test_every_implementation_file_is_scanned() -> TestResult {
        let src = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        let mut on_disk = BTreeSet::new();
        for entry in std::fs::read_dir(src).map_err(ctx("src-Verzeichnis auflisten"))? {
            let entry = entry.map_err(ctx("src-Eintrag lesen"))?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "rs") {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !EXEMPT_FILES.contains(&name.as_str()) {
                    on_disk.insert(name);
                }
            }
        }
        let scanned: BTreeSet<String> = SOURCE_FILES
            .iter()
            .map(|&(name, _)| name.to_owned())
            .collect();
        assert_eq!(
            on_disk, scanned,
            "SOURCE_FILES muss jede Implementierungsdatei in src/ enthalten"
        );
        Ok(())
    }
}
