//! Prüft die Push-Only-Zusage am eigenen Quelltext, nicht am Verhalten.
//!
//! # Verantwortungsbereich
//! Dies ist die Datei, auf die `main.rs`, `sink.rs` und die übrige
//! Moduldokumentation verweisen, wenn sie sagen „siehe
//! `src/push_only_guard.rs` für die genauen Bezeichner": ein späterer Leser
//! bestätigt die Push-Only-Zusage dieser Crate, indem er genau diese kurze
//! Datei liest, statt den gesamten Quelltext zu durchsuchen. Dieselbe
//! Bauweise wie `harw-probe-fs/src/push_only_guard.rs` — dasselbe
//! Konsumentenpaar (privilegierte Sonde → unprivilegierter Sentinel),
//! dieselbe strukturelle Zusage.
//!
//! # Wie die Prüfung funktioniert
//! [`tests::test_no_socket_receive_path_exists_in_source`] bettet den
//! Quelltext jeder anderen Implementierungsdatei dieser Crate über
//! `include_str!` ein und durchsucht ihn nach zwei Zeichenketten: der
//! Unix-Socket-Empfangsfunktion aus `rustix::net` und der Funktion, die
//! einen Puffer bis zum Streamende einliest. Kommt eine von beiden in einer
//! Implementierungsdatei vor, schlägt der Test fehl — unabhängig davon, ob
//! der Aufruf jemals ausgeführt würde.
//!
//! [`tests::test_every_implementation_file_is_scanned`] hält die Liste
//! vollständig: es listet `src/` zur Testzeit auf und schlägt fehl, sobald
//! eine `*.rs`-Datei außer dieser Datei und `test_support.rs` (reiner
//! Testhelfer, nur unter `cfg(test)` gebaut) fehlt. Eine neue
//! Implementierungsdatei — zuletzt `profile.rs` und `scope_rescan.rs`, die
//! beide im privilegierten Laufpfad liegen (Konfiguration laden, cgroup-Walk,
//! Map-Updates) — kann so nicht unbemerkt aus der Prüfung fallen.
//!
//! **Diese Datei selbst ist bewusst nicht Teil der durchsuchten Liste:** die
//! beiden gesuchten Zeichenketten müssen hier als Zeichenkettenliterale
//! auftauchen, damit der Test überhaupt danach suchen kann — würde diese
//! Datei sich selbst einbetten, fände sie ihre eigenen Suchbegriffe und
//! schlüge unabhängig vom tatsächlichen Quelltext der übrigen Crate fehl.
//! Aus demselben Grund vermeidet jede *andere* Datei dieser Crate die beiden
//! Zeichenketten auch in Prosa (Moduldoku, Kommentare) — sie beschreiben die
//! Eigenschaft, ohne die verbotenen Bezeichner wörtlich zu nennen, und
//! verweisen stattdessen hierher.
//!
//! # Exportierte Typen
//! Keine — ausschließlich der Test in [`tests`].
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

    /// Jede Implementierungsdatei dieser Crate außer dieser Testdatei selbst
    /// (siehe Moduldoku für die Begründung der Auslassung).
    const SOURCE_FILES: &[(&str, &str)] = &[
        ("main.rs", include_str!("main.rs")),
        ("cli.rs", include_str!("cli.rs")),
        ("error.rs", include_str!("error.rs")),
        ("landlock.rs", include_str!("landlock.rs")),
        ("real_loader.rs", include_str!("real_loader.rs")),
        ("sensors.rs", include_str!("sensors.rs")),
        ("collect.rs", include_str!("collect.rs")),
        ("sink.rs", include_str!("sink.rs")),
        ("profile.rs", include_str!("profile.rs")),
        ("scope_rescan.rs", include_str!("scope_rescan.rs")),
    ];

    /// Files of `src/` that are deliberately not in [`SOURCE_FILES`]: this
    /// guard itself (it must spell the forbidden identifiers) and the
    /// `cfg(test)`-only test error helper.
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
        // Ein leerer Dateisatz würde den obigen Test trivial bestehen lassen,
        // ohne irgendetwas zu prüfen — diese Absicherung hält die Liste
        // ehrlich, falls sie künftig versehentlich geleert wird.
        assert!(!SOURCE_FILES.is_empty());
    }

    /// Every `*.rs` file in `src/` except [`EXEMPT_FILES`] is scanned, and the
    /// list names no file that does not exist, so a new implementation file
    /// cannot silently escape [`test_no_socket_receive_path_exists_in_source`].
    #[test]
    fn test_every_implementation_file_is_scanned() -> TestResult {
        let src = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        let mut on_disk = BTreeSet::new();
        for entry in std::fs::read_dir(src).map_err(ctx("list the crate's src directory"))? {
            let entry = entry.map_err(ctx("read a src directory entry"))?;
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
        // Beide Richtungen: keine Datei fehlt, und kein Listeneintrag ist veraltet.
        assert_eq!(
            on_disk, scanned,
            "SOURCE_FILES muss jede Implementierungsdatei in src/ enthalten"
        );
        Ok(())
    }
}
