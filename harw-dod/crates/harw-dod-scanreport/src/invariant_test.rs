//! Testbeleg für die Kommandozeilen-Invariante dieser Crate.
//!
//! Nur unter `#[cfg(test)]` eingebunden (siehe `lib.rs`). Durchsucht den
//! eigenen Quelltext dieser Crate nach den vier in der Aufgabenstellung
//! genannten Mustern und schlägt bei einem Treffer fehl — die einzige
//! Prüfung, die auch einen später versehentlich hinzugefügten Aufruf fängt,
//! selbst wenn er tief in einer neuen Funktion verborgen ist. Siehe
//! `crate`-Moduldoku, Abschnitt „Die Kommandozeilen-Invariante", für die
//! Begründung, warum diese Crate überhaupt keine Kommandozeile bilden darf.
//!
//! # Warum dieser Test sich nicht selbst als Treffer meldet
//! Die verbotenen Wörter stehen in dieser Datei nirgends als zusammenhängende
//! Zeichenkette — [`banned_patterns`] baut sie aus Fragmenten zur Laufzeit
//! zusammen (`concat`). Ein naiver Quelltext-Scan dieser Datei findet damit
//! keinen Treffer in ihrem eigenen Prüfcode. Kommentarzeilen (beginnend mit
//! `//`, `///` oder `//!`) werden zusätzlich grundsätzlich übersprungen, weil
//! die Modul-Dokumentation dieser Crate die verbotenen Wörter ausdrücklich
//! *nennen* muss, um die Regel zu erklären (verbindliche Vorgabe: „Absatz im
//! `//!`-Block"). Dieselbe Ausnahme deckt auch die Kommentare in dieser
//! Datei selbst ab, die die vier Muster beim Namen nennen.
//!
//! Zusammengenommen genügt bereits die Kommentar-Ausnahme, damit die
//! Moduldokumentation frei bleibt; die Fragmentierung von
//! [`banned_patterns`] ist eine zusätzliche, unabhängige Absicherung
//! speziell für den ausführbaren Testcode dieser Datei, der die Muster als
//! Zeichenketten referenzieren muss, um sie zu suchen.

use std::path::{Path, PathBuf};

/// Sammelt rekursiv alle `.rs`-Dateien unter `dir`.
///
/// Nur für diesen Test: durchsucht den eigenen Quelltext dieser Crate, nicht
/// deren Laufzeit-Codepfade. Verwendet bewusst `std::fs` direkt — die
/// Invariante „kein direkter `std::fs`-Zugriff" (siehe `harw-dod-readfs`s
/// Moduldoku) gilt für die *Bibliothekscodepfade* dieser Crate (die Module,
/// die Berichte lesen), nicht für ihre Testinfrastruktur, die den Quelltext
/// selbst inspiziert.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Baut die vier verbotenen Muster zur Laufzeit aus Fragmenten zusammen.
///
/// Siehe die Moduldokumentation oben: keines dieser Wörter steht als
/// zusammenhängende Zeichenkette im Quelltext dieser Datei.
fn banned_patterns() -> Vec<String> {
    vec![
        ["Com", "mand"].concat(),
        ["ex", "ec("].concat(),
        ["sp", "awn("].concat(),
        ["sh ", "-c"].concat(),
    ]
}

/// Belegt die Kommandozeilen-Invariante dieser Crate: kein Codepfad bildet
/// eine Kommandozeile.
///
/// # Description
/// Durchsucht jede `.rs`-Datei unter `src/` dieser Crate, Zeile für Zeile,
/// nach den vier verbotenen Mustern aus [`banned_patterns`]. Kommentarzeilen
/// werden übersprungen (siehe Moduldoku). Ein Treffer in einer Codezeile
/// lässt den Test mit einer Meldung fehlschlagen, die die Fundstelle nennt.
///
/// # Warum eine leere Dateiliste selbst ein Fehler ist
/// Fände dieser Test keine `.rs`-Dateien, würde er trivial und
/// bedeutungslos bestehen — das wäre schlimmer als ein echtes Scheitern,
/// weil es unbemerkt bliebe. Die erste Prüfung stellt sicher, dass der
/// Verzeichnispfad zur Laufzeit tatsächlich aufgelöst wurde.
#[test]
fn test_source_contains_no_command_execution() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&src_dir, &mut files);
    assert!(
        !files.is_empty(),
        "Quelltext-Scan darf keine leere Dateiliste ergeben (Pfad falsch aufgelöst?)"
    );

    let patterns = banned_patterns();
    for file in &files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in content.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for pattern in &patterns {
                assert!(
                    !line.contains(pattern.as_str()),
                    "verbotenes Muster '{pattern}' gefunden in {file:?}: {line}"
                );
            }
        }
    }
}
