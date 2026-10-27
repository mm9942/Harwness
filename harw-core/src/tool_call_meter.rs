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
//! - `Ok(permit)`: der Aufruf ist gebucht und darf laufen, solange der
//!   [`ToolCallPermit`] lebt (siehe „Lease-Zaun").
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
//! # Lease-Zaun
//! Eine erfolgreiche Buchung beweist nur, dass die Lease *während* der
//! Buchung gültig war. Ohne weiteren Schutz könnte sie gleich danach — noch
//! bevor das Werkzeug startet — ablaufen und zurückgefordert werden; ein
//! veralteter Worker schriebe oder führte dann aus, nachdem der neue Inhaber
//! begonnen hat. Der [`ToolCallPermit`] ist die Dispatcher-Seite des Zauns:
//! Der Meter legt in ihn einen Guard seiner Wahl (etwa eine Sperre oder einen
//! In-flight-Zähler der Job-Lease, den ein Reclaim respektiert), und der
//! Turn-Loop hält den Permit, bis das Ergebnis genau dieses Aufrufs
//! feststeht — Werkzeug ausgeführt, Kind gespawnt und übergeben, lokale
//! Antwort erzeugt. Solange der Permit lebt, hält der Guard die Lease; ein
//! Reclaim läuft deshalb nie zwischen Buchung und Seiteneffekt, sondern wird
//! über den Dispatch serialisiert. Was der Guard ist und wie ein Reclaim auf
//! ihn wartet, entscheidet allein der Meter; [`ToolCallPermit::unguarded`]
//! hält nichts.
//!
//! - Sequenziell (auch beim Fortsetzen nach einer Rückfrage) fällt der
//!   Permit, sobald das Ergebnis des Aufrufs feststeht, und in jedem Fall
//!   bevor der nächste Aufruf zugelassen wird oder der Turn mit einer
//!   weiteren Modellrunde weiterläuft.
//! - Parallel hält jeder gestartete Job seinen eigenen Permit und lässt ihn
//!   nach seiner Ausführung fallen. Die Permits einer Antwort leben also
//!   gleichzeitig, und der Meter wird befragt, während frühere Permits noch
//!   leben: Ein Guard muss mehrere gleichzeitige Inhaber vertragen (etwa die
//!   Lese-Seite einer Sperre oder ein Zähler). Wartet eine Zulassung auf das
//!   Ende eines früheren Guards, serialisiert das die Antwort.
//! - Bricht ein Cancel einen laufenden Aufruf ab, fällt sein Permit mit ihm.
//! - Ein abgewiesener Aufruf bekommt keinen Permit.
//!
//! # Concurrency
//! Der Turn-Loop ruft den Meter nacheinander und wartet jeweils auf das
//! Ergebnis; ein Meter darf trotzdem von mehreren Turns geteilt werden
//! (`Send + Sync`). Er sollte kurz bleiben: Während der Meter arbeitet, läuft
//! der Turn nicht weiter. Der [`ToolCallPermit`] ist `Send`, nicht `Sync`:
//! der Turn-Loop besitzt ihn und reicht ihn nur als Wert weiter (in den Job
//! eines parallelen Aufrufs).

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// Zulassung eines Werkzeugaufrufs: solange der Permit lebt, darf der
/// Aufruf ausgeführt werden. Der Meter kann darin einen Guard halten
/// (z. B. die Job-Lease-Sperre), den der Dispatcher erst nach der
/// Ausführung des Werkzeugs fallen lässt.
///
/// # Beschreibung
/// Siehe „Lease-Zaun" in der Moduldokumentation. Der Guard wird nie
/// gelesen, nur gehalten: Er wirkt allein dadurch, dass er lebt, und
/// über sein `Drop`, sobald der Dispatcher den Permit fallen lässt.
///
/// # Concurrency
/// `Send`, nicht `Sync` (der Guard ist ein beliebiger `Send`-Wert).
pub struct ToolCallPermit {
    // Gehalten, nie gelesen: fällt mit dem Permit.
    guard: Option<Box<dyn Any + Send>>,
}

impl ToolCallPermit {
    /// Ein Permit ohne Guard: der Aufruf darf laufen, nichts wird gehalten.
    ///
    /// # Returns
    /// Einen Permit, dessen Fallenlassen keine Wirkung hat.
    #[must_use]
    pub fn unguarded() -> Self {
        Self { guard: None }
    }

    /// Ein Permit, der `guard` hält, bis der Dispatcher ihn nach der
    /// Ausführung des Aufrufs fallen lässt.
    ///
    /// # Arguments
    /// - `guard`: beliebiger Wert des Meters (etwa eine Lease-Sperre); er
    ///   wird nie gelesen und erst mit dem Permit fallen gelassen.
    ///
    /// # Returns
    /// Einen Permit, der `guard` hält.
    #[must_use]
    pub fn guarded(guard: Box<dyn Any + Send>) -> Self {
        Self { guard: Some(guard) }
    }
}

impl fmt::Debug for ToolCallPermit {
    // Zeigt nur, ob ein Guard gehalten wird, nie den Guard selbst.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolCallPermit")
            .field("guarded", &self.guard.is_some())
            .finish()
    }
}

/// Boxed Future von [`ToolCallMeter::admit_tool_call`]. `Ok` trägt den
/// [`ToolCallPermit`] des Aufrufs; `Err` trägt den Grund der Abweisung
/// (kurz, ohne Geheimnisse: er geht an das Modell und in den Stop-Grund des
/// Turns).
pub type ToolCallAdmissionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ToolCallPermit, String>> + Send + 'a>>;

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
    /// `Ok(permit)`, wenn der Aufruf gebucht ist und laufen darf, solange
    /// `permit` lebt (der Turn-Loop lässt ihn erst fallen, wenn das Ergebnis
    /// des Aufrufs feststeht); `Err(reason)`, wenn er nicht ausgeführt werden
    /// darf (Budget erschöpft, Lease verloren, Buchung nicht festschreibbar).
    fn admit_tool_call<'a>(&'a self, tool: &'a str) -> ToolCallAdmissionFuture<'a>;
}
