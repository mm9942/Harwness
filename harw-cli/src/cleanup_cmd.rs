//! `harw cleanup`: ephemere Daten (Logs, Caches, Spools) nach den
//! Aufbewahrungsregeln aufräumen.
//!
//! Der Befehl blockiert nicht: er reiht einen Hintergrundauftrag
//! `retention_sweep` im Job-System ein (`harw_ops::retention_job`) und gibt
//! sofort die Auftrags-ID aus. Status, Abbruch und Ergebnis laufen über
//! `harw jobs`. Ohne `--apply` ist es ein Probelauf, der nur meldet;
//! sicherheitsrelevante Klassen (DoD-Spool, Sentinel-Export, Freeze-Records,
//! Session-Transkripte) werden auch mit `--apply` nur gelöscht, wenn
//! `[retention.<klasse>] enabled = true` ausdrücklich gesetzt ist.

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
