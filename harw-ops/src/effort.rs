//! `/effort` — setzt die providerneutrale Reasoning-Stärke für nachfolgende Turns.
//!
//! # Verantwortungsbereich
//! Liest den aktuellen [`ReasoningEffort`]-Level aus dem Session-Zustand
//! (via [`SharedSessionController`]) oder mutiert ihn auf einen neuen Wert.
//! Die Übersetzung in Provider-spezifische Wire-Parameter (Anthropic `budget_tokens`,
//! OpenAI `reasoning.effort`) geschieht downstream in `harw-provider`.
//!
//! # Sicherheitsregel — kein ModelTool
//! Diese Operation ist `permission = "operator"` und `visibility = "tui_only"`:
//! erreichbar ausschließlich über die vom Menschen bediente TUI.
//! Das laufende Sprachmodell darf seine eigene Reasoning-Stärke **nicht selbst
//! wechseln** — ein solcher Mechanismus wäre anfällig für Prompt-Injection-Angriffe.
//! Es darf **kein** `ModelTool`-Variant erstellt werden, der diesen Code-Pfad
//! über eine automatische Tool-Call-Kette aufruft.
//!
//! # Schlüsseltypen
//! - [`EffortArgs`] — geparste Sub-Kommando-Argumente
//!
//! # Nebenläufigkeit
//! Der Handler ist `async`, führt jedoch keinen konkurrenten I/O aus.
//! Er ist `Send + Sync`-kompatibel. Der [`SessionController`]-Zugriff erfolgt
//! über `Arc<dyn SessionController>`, das Interior Mutability erzwingt.
//!
//! # Fehlertypen
//! - [`harw_operations::OpError::Execution`]: `SessionController` nicht in der
//!   `ServiceMap` registriert oder der interne Mutations-Kanal ist geschlossen.
//! - [`harw_operations::OpError::InvalidArguments`]: Unbekannter Effort-Level-String.
//!
//! # Beispiel
//! ```no_run
//! // Wird vom harw-ops-Dispatch-Layer aufgerufen — nicht direkt nutzbar.
//! ```

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput, SharedSessionController};
use harw_types::ReasoningEffort;

// ── EffortArgs ────────────────────────────────────────────────────────────────

/// Argumente für die `/effort`-Operation.
///
/// # Beschreibung
/// Trägt den optionalen Sub-Kommando-String, den der Operator eingegeben hat.
/// Die Runtime deserialisiert die rohe TUI-Eingabe in dieses Struct, bevor
/// der Handler aufgerufen wird.
///
/// # Felder
/// - `level` (`Option<String>`): Eines von `"show"` (Standard), `"clear"`,
///   `"minimal"`, `"low"`, `"medium"`, `"high"`.  Bei Abwesenheit fällt die
///   Operation auf `"show"` zurück.
///
/// # Design-Doc-Referenz
/// `/effort`-Operationsspezifikation — Args-Abschnitt.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct EffortArgs {
    /// Sub-Kommando: `"show"` (Standard), `"clear"`, `"minimal"`, `"low"`,
    /// `"medium"` oder `"high"`.
    ///
    /// Wird aus der TUI-Eingabe deserialisiert.  `None` wird als `"show"` behandelt.
    #[serde(default)]
    #[raw(first)]
    pub level: Option<String>,
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// Verarbeitet die `/effort`-Operation.
///
/// # Beschreibung
/// Dispatcht anhand von [`EffortArgs::level`] auf das passende Sub-Kommando:
///
/// | Sub-Kommando | Verhalten |
/// |---|---|
/// | `show` (Standard) | Zeigt den aktuell gesetzten Effort-Level oder "nicht gesetzt" |
/// | `clear` / `none` / `auto` | Setzt den Effort auf `None` (Provider-Default) zurück |
/// | `minimal` / `low` / `medium` / `high` | Setzt den Effort auf den angegebenen Level |
/// | *(sonstiges)* | Gibt [`OpError::InvalidArguments`] zurück |
///
/// # Sicherheitsregel — kein ModelTool
/// Diese Funktion darf niemals als LLM-aufrufbares Tool exponiert werden.
/// Das aktive Modell **darf seine eigene Reasoning-Stärke nicht wechseln** —
/// das würde Prompt-Injection-Angriffe ermöglichen.
/// Die Absicherung erfolgt durch `permission = "operator"` und `visibility = "tui_only"`.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Gemeinsamer Ausführungskontext; liefert den
///   [`SharedSessionController`] via `ctx.service::<SharedSessionController>()`.
/// - `args` (`EffortArgs`): Geparstes Sub-Kommando; fehlendes `level` fällt auf `"show"`.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit menschenlesbarem Text.
///
/// # Fehler
/// - [`OpError::Execution`]: `SessionController` nicht in `ServiceMap` oder
///   Mutations-Kanal geschlossen (`SessionControlError::Disconnected`).
/// - [`OpError::InvalidArguments`]: Unbekannter Effort-Level-String.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Greift auf `Arc<dyn SessionController>` zu — der Controller erzwingt intern
/// Interior Mutability. Der Handler selbst hält keine Locks.
///
/// # Beispiel
/// ```no_run
/// // Wird vom harw-ops-Dispatch-Layer aufgerufen — nicht direkt aufrufbar.
/// ```
#[operation(
    name = "effort",
    aliases = ["reasoning"],
    summary = "Setzt die Reasoning-Stärke für kommende Turns (minimal|low|medium|high, clear, show).",
    domain = "catalog_config",
    permission = "operator",
    category = "model",
    command(path = "/effort", visibility = "tui_only"),
)]
async fn effort(ctx: &OpContext, args: EffortArgs) -> Result<OpOutput, OpError> {
    let sub = args.level.as_deref().unwrap_or("show");

    let controller = ctx
        .service::<SharedSessionController>()
        .ok_or_else(|| OpError::Execution("SessionController not available".into()))?;

    match sub {
        "show" => {
            let snap = controller.snapshot();
            let text = match snap.reasoning_effort {
                Some(e) => format!("Reasoning-Effort: {e}"),
                None => "Reasoning-Effort: (nicht gesetzt — Provider-Default)".to_string(),
            };
            Ok(OpOutput::from(text))
        }
        "clear" | "none" | "auto" => {
            controller
                .set_reasoning_effort(None)
                .map_err(|e| OpError::Execution(e.to_string()))?;
            Ok(OpOutput::from(
                "Reasoning-Effort zurückgesetzt (Provider-Default)".to_string(),
            ))
        }
        other => {
            let effort: ReasoningEffort = other.parse().map_err(|_| {
                OpError::InvalidArguments(format!(
                    "Unbekanntes Level: '{other}'. \
                     Erwartet: minimal | low | medium | high | clear | show."
                ))
            })?;
            controller
                .set_reasoning_effort(Some(effort))
                .map_err(|e| OpError::Execution(e.to_string()))?;
            Ok(OpOutput::from(format!(
                "Reasoning-Effort gesetzt: {other}"
            )))
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::EffortArgs;
    use crate::testutil::toks;
    use harw_operations::{FromRawArgs, OpInput};

    /// Prüft, dass `from_raw_args` das erste Token als `level` übernimmt.
    #[test]
    fn test_effort_args_from_raw_args_maps_level() {
        let args = EffortArgs::from_raw_args(&toks(&["high"]));
        match args {
            Ok(a) => assert_eq!(a.level.as_deref(), Some("high")),
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    /// Prüft, dass leere Token-Liste `level = None` liefert.
    #[test]
    fn test_effort_args_from_empty_tokens_produces_none() {
        let args = EffortArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => assert!(
                a.level.is_none(),
                "Leere Token-Liste muss level=None ergeben"
            ),
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    /// Prüft, dass `level = None` im Handler als `"show"` interpretiert wird —
    /// d. h. `args.level.as_deref().unwrap_or("show")` gibt `"show"` zurück.
    #[test]
    fn test_effort_args_show_is_default_when_level_missing() {
        let args = EffortArgs { level: None };
        let sub = args.level.as_deref().unwrap_or("show");
        assert_eq!(sub, "show", "Fehlendes level muss auf 'show' fallen");
    }

    /// Prüft, dass Serde mit einem JSON-`null`-Wert `level = None` ergibt
    /// (via `#[serde(default)]` auf dem Feld).
    #[test]
    fn test_effort_args_serde_deserialize_from_json_null_uses_default() {
        let json = r#"{"level": null}"#;
        let args: EffortArgs =
            serde_json::from_str(json).expect("Deserialisierung aus JSON null muss klappen");
        assert!(args.level.is_none(), "level=null in JSON muss None ergeben");
    }

    // ── OpInvocation coverage ─────────────────────────────────────────────────

    /// Prüft, dass `OpInput::command` ein Command-Invocation mit rohen Args erzeugt.
    #[test]
    fn test_op_input_command_is_command_with_raw_args() {
        let input = OpInput::command("/effort", vec!["medium".to_owned()]);
        assert!(input.invocation.is_command());
        assert_eq!(input.invocation.raw_args(), &["medium".to_owned()]);
    }
}
