//! Prüft die Push-Only-Zusage am eigenen Quelltext, nicht am Verhalten.
//!
//! # Verantwortungsbereich
//! Dies ist die Datei, auf die `main.rs`, `sink.rs` und die übrige
//! Moduldokumentation verweisen, wenn sie sagen „siehe
//! `src/push_only_guard.rs` für die genauen Bezeichner": ein späterer
//! Leser bestätigt die Push-Only-Zusage dieser Crate, indem er genau diese
//! kurze Datei liest, statt den gesamten Quelltext zu durchsuchen.
//!
//! # Wie die Prüfung funktioniert
//! [`tests::test_no_socket_receive_path_exists_in_source`] bettet den
//! Quelltext jeder anderen Implementierungsdatei dieser Crate über
//! `include_str!` ein und durchsucht ihn nach zwei Zeichenketten: der
//! Unix-Socket-Empfangsfunktion aus `rustix::net` und der Funktion, die
//! einen Puffer bis zum Streamende einliest. Kommt eine von beiden in
//! einer Implementierungsdatei vor, schlägt der Test fehl — unabhängig
//! davon, ob der Aufruf jemals ausgeführt würde.
//!
//! **Diese Datei selbst ist bewusst nicht Teil der durchsuchten Liste:**
//! Die beiden gesuchten Zeichenketten müssen hier als Zeichenkettenliterale
//! auftauchen, damit der Test überhaupt danach suchen kann — würde diese
//! Datei sich selbst einbetten, fände sie ihre eigenen Suchbegriffe und
//! schlüge unabhängig vom tatsächlichen Quelltext der übrigen Crate fehl.
//! Aus demselben Grund vermeidet jede *andere* Datei dieser Crate die
//! beiden Zeichenketten auch in Prosa (Moduldoku, Kommentare) — sie
//! beschreiben die Eigenschaft, ohne die verbotenen Bezeichner wörtlich zu
//! nennen, und verweisen stattdessen hierher.
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
    /// Jede Implementierungsdatei dieser Crate außer dieser Testdatei
    /// selbst (siehe Moduldoku für die Begründung der Auslassung).
    const SOURCE_FILES: &[(&str, &str)] = &[
        ("main.rs", include_str!("main.rs")),
        ("cli.rs", include_str!("cli.rs")),
        ("error.rs", include_str!("error.rs")),
        ("landlock.rs", include_str!("landlock.rs")),
        ("source.rs", include_str!("source.rs")),
        ("sink.rs", include_str!("sink.rs")),
        ("collect.rs", include_str!("collect.rs")),
    ];

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
}
