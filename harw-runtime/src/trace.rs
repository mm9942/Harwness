//! Der eine Wurzel-Trace eines Laufs.
//!
//! # Verantwortungsbereich
//! Ein Trace ist hereditär wie die Kontext-Decke: ein Kind erbt die
//! `trace_id` seines Elternteils. Der Weg beginnt an einer Sitzung ohne
//! Elternteil — und dort stand der Erzeuger bisher viermal fast, aber nicht
//! ganz gleich im Workspace: `harw-cli/src/main.rs`
//! (`new_local_root_trace`), `harw-cli/src/chat.rs`
//! (`new_one_shot_root_trace`), `harw-tui/src/app.rs` (`new_tui_root_trace`)
//! und `harw-cli/src/job_worker.rs` (`plan_node_spawn_trace`). Drei davon
//! sind Zeile für Zeile identisch, die vierte unterscheidet sich im
//! Fehlerverhalten. Alle vier Altstellen — `new_local_root_trace`
//! eingeschlossen — sind seit W2d-2 entfernt: ihre Aufrufer laufen jetzt über
//! die RuntimeAssembly-Montage (siehe [`crate::RuntimeAssembly`]), die
//! [`new_root_trace`] hier einmal je Lauf zieht.
//!
//! # Der Trace-Bruch (Befund G-044)
//! Der Wurzel-Trace entstand jeweils **innerhalb** des `SpawnContext`, also
//! im Spawner-Pfad. Die Sitzung selbst kannte ihn nicht: was der Root-Agent
//! tat, bevor (oder ohne dass) er ein Kind startete, hing an keiner
//! `trace_id`, und ein Einstieg ohne Spawner
//! ([`crate::spec::SpawnerPolicy::None`]) hatte überhaupt keinen. Deshalb
//! steht [`new_root_trace`] hier, in der Montage: sie zieht den Trace
//! **einmal je Lauf**, und Sitzung wie Spawn-Kontext bekommen denselben Wert.
//!
//! # Woher der Zufall kommt
//! Alle vier Altstellen benutzten `uuid::Uuid::new_v4()`. `harw-runtime`
//! führt keine eigene `uuid`-Abhängigkeit; der einzige über die Crate-Grenze
//! erreichbare Ziehungspunkt derselben Quelle ist das `newtype_id!`-Makro in
//! `harw-types` (`harw-types/src/ids.rs:36-40`: `Self(Uuid::new_v4()
//! .to_string())`). [`new_root_trace`] zieht dort und benutzt die Hexziffern
//! der UUID. Eine zweite Zufallsquelle daneben (etwa ein Hash über die Uhr)
//! wäre eine zweite Auslegung derselben Frage — genau das, was dieses Modul
//! beseitigt.
//!
//! # Fehler
//! Keine: die gezogenen Zeichenketten sind per Konstruktion gültiges
//! Kleinbuchstaben-Hex der geforderten Länge.

use harw_observe::TraceContext;
use harw_types::SessionId;

use crate::spec::EntryKind;

/// Zieht 32 Kleinbuchstaben-Hexzeichen.
///
/// # Beschreibung
/// Eine UUID v4 in Standarddarstellung besteht aus genau 32 Hexziffern in
/// Kleinschreibung plus vier Bindestrichen; das Filtern der Bindestriche
/// liefert daher immer genau 32 gültige Zeichen. Die Ziehung erfolgt über
/// [`SessionId::new`] (siehe Moduldoku: derselbe `Uuid::new_v4`-Aufruf, den
/// die Altstellen benutzten); der `SessionId`-Wert selbst wird verworfen.
fn draw_hex32() -> String {
    SessionId::new()
        .as_str()
        .chars()
        .filter(char::is_ascii_hexdigit)
        .collect()
}

/// Erzeugt den Wurzel-Trace eines Laufs.
///
/// # Beschreibung
/// Diese Stelle ist eine Wurzel: ein Lauf hat kein Elternteil, dessen Trace
/// er erben könnte. `trace_id` sind die 32 Hexziffern einer frisch gezogenen
/// UUID v4, `span_id` die ersten 16 Hexziffern einer **zweiten,
/// unabhängigen** Ziehung — dasselbe Muster wie `harw-core`s `new_span_id`
/// und die vier Altstellen. `parent_span_id` bleibt leer; ein Wurzel-Span
/// hat keinen Elternteil.
///
/// Der zurückgegebene Wert ist der Trace des **ganzen** Laufs: die
/// Wurzelsitzung und der Spawn-Kontext ihrer Kinder teilen ihn, statt dass
/// der Spawner sich einen eigenen zieht (Befund G-044, siehe Moduldoku).
///
/// # Argumente
/// - `entry` ([`EntryKind`]): der Einstieg. Beeinflusst den Trace nicht — ein
///   Trace ist eine Identität, keine Rechtsentscheidung — und erscheint nur
///   im Diagnoseprotokoll, damit eine `trace_id` ihrem Einstieg zugeordnet
///   werden kann.
///
/// # Rückgabe
/// Ein [`TraceContext`] ohne Elternspanne.
///
/// # Panics
/// Nie: [`TraceContext::new`] prüft Länge und Zeichensatz, und beide
/// Zeichenketten erfüllen sie per Konstruktion (siehe `draw_hex32`). Die
/// Prüfung läuft trotzdem, statt die `pub`-Felder direkt zu setzen — der
/// validierende Konstruktor ist die eine Stelle, die das Format kennt.
#[must_use]
pub fn new_root_trace(entry: EntryKind) -> TraceContext {
    let trace_id = draw_hex32();
    let span_id: String = draw_hex32().chars().take(16).collect();
    let trace = TraceContext::new(trace_id, span_id)
        .expect("a UUID v4's hex digits are valid lowercase trace and span identifiers");
    tracing::debug!(
        entry = ?entry,
        trace_id = %trace.trace_id,
        span_id = %trace.span_id,
        "runtime.trace.root_created"
    );
    trace
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_ENTRIES: [EntryKind; 11] = [
        EntryKind::Tui,
        EntryKind::OneShot,
        EntryKind::LocalEcho,
        EntryKind::Analyze,
        EntryKind::Doctor,
        EntryKind::Web,
        EntryKind::McpServe,
        EntryKind::JobPrompt,
        EntryKind::JobPlanNode,
        EntryKind::GatewayTelegram,
        EntryKind::GatewayDream,
    ];

    fn is_lowercase_hex(value: &str, len: usize) -> bool {
        value.len() == len
            && value
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    }

    #[test]
    fn draw_yields_exactly_thirty_two_lowercase_hex_digits() {
        for _ in 0..16 {
            assert!(is_lowercase_hex(&draw_hex32(), 32));
        }
    }

    #[test]
    fn every_entry_gets_a_well_formed_root_span() {
        for entry in ALL_ENTRIES {
            let trace = new_root_trace(entry);
            assert!(is_lowercase_hex(&trace.trace_id, 32), "{entry:?}");
            assert!(is_lowercase_hex(&trace.span_id, 16), "{entry:?}");
            assert!(trace.parent_span_id.is_none(), "{entry:?}");
        }
    }

    #[test]
    fn two_roots_never_share_a_trace_id() {
        let first = new_root_trace(EntryKind::Tui);
        let second = new_root_trace(EntryKind::Tui);
        assert_ne!(first.trace_id, second.trace_id);
        assert_ne!(first.span_id, second.span_id);
    }

    #[test]
    fn trace_id_and_span_id_are_independent_draws() {
        let trace = new_root_trace(EntryKind::OneShot);
        assert_ne!(trace.trace_id[..16], trace.span_id);
    }
}
