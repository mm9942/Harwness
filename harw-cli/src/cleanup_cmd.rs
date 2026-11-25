//! `harw cleanup`: ephemere Daten (Logs, Caches, Spools) nach den
//! Aufbewahrungsregeln aufräumen.
//!
//! Der Befehl blockiert nicht: er reiht einen Hintergrundauftrag
//! `retention_sweep` im Job-System ein (`harw_ops::retention_job`) und gibt
//! sofort die Auftrags-ID aus. Status, Abbruch und Ergebnis laufen über
//! `harw jobs`. Ohne `--apply` ist es ein Probelauf, der nur meldet;
//! sicherheitsrelevante Klassen (DoD-Spool, Sentinel-Export, DoD-Verdicts
//! `dod_verdicts`, DoD-Labels `dod_labels`, Freeze-Records,
//! Session-Transkripte und deren Korrupt-Backups) sind Opt-in: sie werden
//! auch mit `--apply` nur gelöscht, wenn `[retention.<klasse>] enabled =
//! true` ausdrücklich gesetzt ist.
//!
//! Session-Transkripte (`session_transcripts`) brauchen zusätzlich eine
//! autoritative Anhänge-Quelle, die erst bei der Ausführung je Sitzung sagt,
//! ob sie gerade angehängt ist: die Sitzungssperre `<id>.jsonl.lock` gilt nur
//! für einen einzelnen Schreibvorgang (`TranscriptStore::append`) und schützt
//! eine angehängte, ruhende Sitzung nicht. Eine solche Quelle gibt es noch
//! nicht. Der Job meldet die Klasse deshalb im Probelauf
//! wie mit `--apply` als übersprungen (`skipped: "no authoritative attachment
//! source"` im Job-Ergebnis von `harw jobs show`) und löscht kein Transkript,
//! auch mit Opt-in; ohne Opt-in nennt `--apply` zuerst das fehlende Opt-in.
//! Die übrigen Klassen, auch die Korrupt-Backups, laufen unverändert. Wie
//! sich das später freischalten lässt, beschreibt `harw_ops::retention_job`.

use crate::cli::GlobalArgs;
use crate::jobs_cmd::working_dir;
use crate::op_bridge::{OpTarget, with_op_context};
use crate::output::Printer;
use harw_ops::retention_job::{RetentionSweepSpec, enqueue_retention_sweep};

/// Führt `harw cleanup` aus.
///
/// # Errors
/// Ein Fehlertext, wenn Home oder Konfiguration nicht aufgelöst werden
/// können, kein Job-Store verfügbar ist, der Auftrag abgelehnt wird oder die
/// Ausgabe fehlschlägt.
pub fn run(
    g: &GlobalArgs,
    apply: bool,
    classes: Vec<String>,
    deadline_secs: Option<u64>,
) -> Result<(), String> {
    let cwd = working_dir(g)?;
    let output = with_op_context(
        OpTarget {
            home: g.home.clone(),
            cwd: &cwd,
        },
        |ctx, home, config| {
            let home = std::path::absolute(home)
                .map_err(|error| format!("Home nicht absolut auflösbar: {error}"))?;
            let project = std::path::absolute(&cwd).ok();
            let mut spec = RetentionSweepSpec::new(home, project, config.harness.retention.clone());
            spec.apply = apply;
            spec.classes = classes;
            if let Some(secs) = deadline_secs {
                spec.deadline_secs = secs;
            }
            enqueue_retention_sweep(ctx, &spec).map_err(|error| format!("harw cleanup: {error}"))
        },
    )?;
    Printer::new(g.output()).op_output(&output)
}
