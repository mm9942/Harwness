//! Werkzeugkasten für die CI-Gates und die Oberflächen-Erzeugung.
//!
//! # Verantwortungsbereich
//! Diese Binärcrate besitzt ausschließlich die *Verteilung* der Unterbefehle.
//! Jeder Unterbefehl lebt in einer eigenen Datei, damit zwei Arbeitsknoten
//! sie unabhängig füllen können, ohne sich eine Datei zu teilen:
//!
//! - [`gates`] — die Prüfungen aus Knoten AW0-10 (verbotene Kanten,
//!   Privilegienbudget je Binary, Erzeugung der Schreibbereichstabelle).
//! - [`webui`] — Typerzeugung und statischer Export aus Knoten UI-01.
//!
//! # Warum keine Argument-Bibliothek
//! Ein Aufgabenläufer mit zwei Unterbefehlen braucht keine. Die Abhängigkeit
//! wäre in jedem CI-Lauf zu bauen und würde nichts vereinfachen.
//!
//! # Stand
//! Gerüst aus Knoten AW0-00 (Workspace-Fundament).
//!
//! # Examples
//! ```text
//! cargo xtask gates
//! cargo xtask webui types
//! ```

mod gates;
mod webui;

use std::process::ExitCode;

/// Verteilt auf den benannten Unterbefehl.
///
/// # Returns
/// `ExitCode::SUCCESS`, wenn der Unterbefehl erfolgreich war; sonst
/// `ExitCode::FAILURE`. Ein unbekannter oder fehlender Unterbefehl ist ein
/// Fehler, keine stille Hilfeausgabe — im CI soll ein Tippfehler im
/// Aufgabennamen rot werden, nicht grün.
fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let rest: Vec<String> = args.collect();

    let result = match command.as_str() {
        "gates" => gates::run(&rest),
        "webui" => webui::run(&rest),
        other => {
            eprintln!("xtask: unbekannter Unterbefehl '{other}'\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask {command}: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Aufrufhinweis, an genau einer Stelle formuliert.
const USAGE: &str = "\
Aufruf: cargo xtask <befehl> [argumente]

  gates    Struktur- und Berechtigungsprüfungen über den Abhängigkeitsgraphen
  webui    Typerzeugung und statischer Export der Control Plane";

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
