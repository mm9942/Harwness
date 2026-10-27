//! Zulassung einzelner Werkzeugaufrufe unmittelbar vor ihrer Ausführung.
//!
//! # Warum es diese Naht gibt
//! [`crate::turn_loop::TurnLimits::max_tool_calls`] und
//! [`crate::turn_loop::TurnControl::with_tool_call_budget`] zählen im Speicher
//! eines einzelnen Turns. Ein dauerhafter Auftrag braucht mehr: Der Aufruf
//! muss unter der aktuell gültigen Lease gebucht *und festgeschrieben* sein,
//! bevor das Werkzeug läuft. Sonst sieht ein Reclaim den Verbrauch nicht, und
//! ein Worker, dessen Lease verloren ging, führt weiter Werkzeuge aus. Store
//! und Lease kennt nur der Aufrufer des Turn-Loops; `harw-core` stellt
//! deshalb nur den Haken bereit, kein Zählwerk.
//!
//! # Vertrag
//! Der Turn-Loop fragt den Meter (registriert über
//! [`crate::turn_loop::TurnControl::with_tool_call_meter`]) für **jeden**
//! Aufruf, dessen Guardrail `Allow` ergab, und zwar *bevor* irgendetwas davon
//! ausgeführt wird: normale Werkzeuge, Handoffs (`transfer_to_*`,
//! `agents.delegate`) und lokal beantwortete Aufrufe (`agents.catalog`).
//! Verweigerte (`Deny`) und auf eine Rückfrage wartende (`AskUser`) Aufrufe
//! werden nicht gebucht; ein genehmigter Aufruf wird beim Fortsetzen gebucht.
//!
//! - `Ok(())`: der Aufruf ist gebucht und darf laufen.
//! - `Err(reason)`: der Aufruf wird **nicht** ausgeführt. Er bekommt ein
//!   Fehlerergebnis mit dem Präfix
//!   [`crate::turn_loop::TOOL_CALL_REFUSED_PREFIX`], jeder noch offene Aufruf
//!   derselben Antwort ebenso, und der Turn endet mit
//!   `TurnOutcome::Cancelled { reason: CancelReason::Budget }`
//!   ([`crate::turn_loop::TurnControl::stop_detail`] nennt den Grund) — genau
//!   wie bei einem Treffer des Werkzeug-Budgets. In der parallelen
//!   Ausführung laufen die davor zugelassenen Aufrufe der Antwort zu Ende;
//!   der erste abgewiesene und alle folgenden werden nicht gestartet.
//!
//! # Concurrency
//! Der Turn-Loop ruft den Meter nacheinander und wartet jeweils auf das
//! Ergebnis; ein Meter darf trotzdem von mehreren Turns geteilt werden
//! (`Send + Sync`). Er sollte kurz bleiben: Während der Meter arbeitet, läuft
//! der Turn nicht weiter.

use std::future::Future;
use std::pin::Pin;

/// Boxed Future von [`ToolCallMeter::admit_tool_call`]. `Err` trägt den
/// Grund der Abweisung (kurz, ohne Geheimnisse: er geht an das Modell und in
/// den Stop-Grund des Turns).
pub type ToolCallAdmissionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// Bucht und lässt jeden Werkzeugaufruf eines Turns zu, bevor er ausgeführt
/// wird.
///
/// # Beschreibung
/// Siehe die Moduldokumentation für den vollständigen Vertrag. Der Meter
/// bekommt bewusst nur den Werkzeugnamen, nie die Argumente: Zulassung und
/// Budget hängen nicht vom Inhalt eines Aufrufs ab, und Argumente stammen aus
/// nicht vertrauenswürdiger Modellausgabe.
pub trait ToolCallMeter: Send + Sync {
    /// Bucht einen Aufruf von `tool` und entscheidet, ob er laufen darf.
    ///
    /// # Arguments
    /// - `tool`: Name des Werkzeugs, wie ihn das Modell angefragt hat.
    ///
    /// # Returns
    /// `Ok(())`, wenn der Aufruf gebucht ist und laufen darf; `Err(reason)`,
    /// wenn er nicht ausgeführt werden darf (Budget erschöpft, Lease
    /// verloren, Buchung nicht festschreibbar).
    fn admit_tool_call<'a>(&'a self, tool: &'a str) -> ToolCallAdmissionFuture<'a>;
}
