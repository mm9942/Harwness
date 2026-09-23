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
//! # `/uia-effort` — struktureller Zwilling, Config-only
//! Dieses Modul enthält zusätzlich `/uia-effort`: dieselbe
//! `show|clear|minimal|low|medium|high|xhigh|max`-Grammatik wie `/effort`,
//! aber **keine** Live-`SessionController`-Mutation. Stattdessen persistiert
//! sie `reasoning.uia` (`harw_config::HarnessConfig::reasoning.uia`) in der
//! Profil-`config.toml` — analog zu `/uia-worker-model`
//! (`crate::model::uia_worker_model`), das `uia_worker_model` genauso
//! Config-only verankert. Der gesetzte Wert wirkt erst ab der nächsten
//! Sitzung (gelesen von `harw-runtime::guard_wiring::parse_effort_field`),
//! nicht für laufende Turns.
//!
//! # Schlüsseltypen
//! - [`EffortArgs`] — geparste Sub-Kommando-Argumente (von `/effort` **und**
//!   `/uia-effort` geteilt)
//!
//! # Nebenläufigkeit
//! Beide Handler sind `async`, führen jedoch keinen konkurrenten I/O aus.
//! Sie sind `Send + Sync`-kompatibel. Der `/effort`-[`SessionController`]-Zugriff
//! erfolgt über `Arc<dyn SessionController>`, das Interior Mutability
//! erzwingt; `/uia-effort` liest stattdessen synchron eine aufgelöste
//! [`harw_config::ResolvedConfig`] und schreibt bestes Bemühen über
//! [`harw_config::ConfigWriter`] — kein geteilter Laufzeitzustand.
//!
//! # Fehlertypen
//! - [`harw_operations::OpError::Execution`] — `/effort`: `SessionController`
//!   nicht in der `ServiceMap` registriert oder der interne Mutations-Kanal
//!   ist geschlossen. `/uia-effort`: Config-Discovery fehlgeschlagen.
//! - [`harw_operations::OpError::InvalidArguments`] — Unbekannter
//!   Effort-Level-String (beide Operationen).
//!
//! # Beispiel
//! ```no_run
//! // Wird vom harw-ops-Dispatch-Layer aufgerufen — nicht direkt nutzbar.
//! ```

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput, SharedSessionController};
use harw_types::ReasoningEffort;

// `crate::config_util::selection_persistence(ctx)` already returns
// `Arc<dyn SelectionPersistence>`, so the trait is named in that return
// type and its methods resolve without a separate `use` of the trait here.

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
/// - [`OpError::Execution`] — `SessionController` nicht in `ServiceMap` oder
///   Mutations-Kanal geschlossen (`SessionControlError::Disconnected`).
/// - [`OpError::InvalidArguments`] — Unbekannter Effort-Level-String.
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
                     Erwartet: minimal | low | medium | high | xhigh | max | clear | show."
                ))
            })?;
            controller
                .set_reasoning_effort(Some(effort))
                .map_err(|e| OpError::Execution(e.to_string()))?;
            Ok(OpOutput::from(format!("Reasoning-Effort gesetzt: {other}")))
        }
    }
}

// ── `/uia-effort` — Config-only Zwilling ────────────────────────────────────

/// Verarbeitet ein erfolgreich geparstes `/uia-effort <level>`: validiert
/// `target` gegen [`ReasoningEffort`] und persistiert es über `persist`.
///
/// # Beschreibung
/// Im Gegensatz zu `/effort`s `other`-Zweig mutiert dieser Pfad **keinen**
/// Live-`SessionController`-Zustand — `reasoning.uia` wird ausschließlich
/// über [`crate::config_util::persist_uia_reasoning_effort`] in der
/// Profil-`config.toml` verankert und wirkt erst beim nächsten
/// Sitzungsstart, analog zu `crate::model::handle_uia_worker_model_switch`
/// (dortiger `uia_worker_model`-Pfad; dort wie hier privat, deshalb bewusst
/// als Klartext statt als Intra-Doc-Link referenziert).
///
/// `persist` mirrors that function's injected-closure design: the production
/// caller ([`uia_effort`]) always passes
/// [`crate::config_util::persist_uia_reasoning_effort`], so runtime behavior
/// is unchanged from a hardcoded call — the injection exists purely so tests
/// can supply a no-op closure and never touch the real, `HARW_HOME`-resolving
/// persistence path (this crate declares `#![forbid(unsafe_code)]`, so a
/// testing-only `HARW_HOME` env-isolation helper, which would need `unsafe
/// fn std::env::set_var`/`remove_var`, is not available here).
///
/// # Argumente
/// - `target` (`&str`): das zu setzende Effort-Level, roh aus der TUI-Eingabe.
/// - `persist` (`impl FnOnce(Option<&str>) -> Option<String>`): wird nach
///   erfolgreicher Validierung einmal mit `Some(kanonisches_level_str)`
///   aufgerufen. `None` bei Erfolg, `Some(note)` mit einer Fehlernotiz.
///
/// # Rückgabe
/// [`OpOutput`] mit Bestätigungstext („Reasoning-Effort für die UIA gesetzt:
/// {level} (gespeichert für die nächste Sitzung)"), inklusive Persistenz-Notiz
/// bei Fehlschlag.
///
/// # Fehler
/// - [`OpError::InvalidArguments`] — Unbekannter Effort-Level-String.
///
/// # Panics
/// Nie.
fn handle_uia_effort_switch(
    target: &str,
    persist: impl FnOnce(Option<&str>) -> Option<String>,
) -> Result<OpOutput, OpError> {
    let effort: ReasoningEffort = target.parse().map_err(|_| {
        OpError::InvalidArguments(format!(
            "Unbekanntes Level: '{target}'. \
             Erwartet: minimal | low | medium | high | xhigh | max | clear | show."
        ))
    })?;
    let level = effort.to_string();

    let mut text = format!("Reasoning-Effort für die UIA gesetzt: {level}");
    match persist(Some(level.as_str())) {
        Some(note) => {
            text.push('\n');
            text.push_str(&note);
        }
        None => text.push_str(" (gespeichert für die nächste Sitzung)"),
    }
    Ok(OpOutput::from(text))
}

/// Verarbeitet `/uia-effort clear|none|auto`: entfernt den `reasoning.uia`-Pin
/// über `persist`, ohne jeglichen Live-Zustand zu berühren.
///
/// # Argumente
/// - `persist` (`impl FnOnce(Option<&str>) -> Option<String>`): wird einmal
///   mit `None` aufgerufen (explizite Pin-Entfernung, wie bei
///   [`crate::config_util::persist_uia_worker_model`]).
///
/// # Rückgabe
/// [`OpOutput`] mit Bestätigungstext, inklusive Persistenz-Notiz bei Fehlschlag.
///
/// # Panics
/// Nie.
fn handle_uia_effort_clear(persist: impl FnOnce(Option<&str>) -> Option<String>) -> OpOutput {
    let mut text = "Reasoning-Effort für die UIA zurückgesetzt".to_string();
    match persist(None) {
        Some(note) => {
            text.push('\n');
            text.push_str(&note);
        }
        None => text.push_str(" (gespeichert für die nächste Sitzung)"),
    }
    OpOutput::from(text)
}

/// Verarbeitet die `/uia-effort`-Operation.
///
/// # Beschreibung
/// Wiederverwendet [`EffortArgs`] und dieselbe
/// `show|clear|minimal|low|medium|high|xhigh|max`-Grammatik wie `/effort`,
/// dispatcht aber Config-only statt über den Live-`SessionController`:
///
/// | Sub-Kommando | Verhalten |
/// |---|---|
/// | `show` (Standard) | Liest `reasoning.uia` aus der aufgelösten Config (kein Live-Snapshot) |
/// | `clear` / `none` / `auto` | Entfernt den `reasoning.uia`-Pin in der Profil-`config.toml` |
/// | `minimal` / `low` / `medium` / `high` / `xhigh` / `max` | Persistiert `reasoning.uia` |
/// | *(sonstiges)* | Gibt [`OpError::InvalidArguments`] zurück |
///
/// # Sicherheitsregel — kein ModelTool
/// Diese Funktion darf niemals als LLM-aufrufbares Tool exponiert werden.
/// Das aktive Modell **darf seine eigene (oder die UIA-)Reasoning-Stärke
/// nicht wechseln** — das würde Prompt-Injection-Angriffe ermöglichen. Die
/// Absicherung erfolgt durch `permission = "operator"` und
/// `visibility = "tui_only"`.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext; nur für `show` genutzt, um die
///   aufgelöste Config zu lesen (via
///   [`crate::provider::resolved_config`] — context-scoped-first, dieselbe
///   Autorität wie `/uia-worker-model`).
/// - `args` (`EffortArgs`): geparstes Sub-Kommando; fehlendes `level` fällt
///   auf `"show"` zurück.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit menschenlesbarem Text.
///
/// # Fehler
/// - [`OpError::Execution`] — Config-Discovery fehlgeschlagen (nur `show`).
/// - [`OpError::InvalidArguments`] — Unbekannter Effort-Level-String.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Rein synchron abgesehen vom `async fn`-Signaturzwang der Operation-Macro;
/// kein geteilter Laufzeitzustand, kein Datei-Lock.
///
/// # Beispiel
/// ```no_run
/// // Wird vom harw-ops-Dispatch-Layer aufgerufen — nicht direkt nutzbar.
/// ```
#[operation(
    name = "uia-effort",
    summary = "Zeigt/setzt den für die UIA gepinnten Reasoning-Effort (reasoning.uia); wirkt erst ab der nächsten Sitzung.",
    domain = "catalog_config",
    permission = "operator",
    category = "model",
    command(path = "/uia-effort", visibility = "tui_only")
)]
async fn uia_effort(ctx: &OpContext, args: EffortArgs) -> Result<OpOutput, OpError> {
    let sub = args.level.as_deref().unwrap_or("show");

    match sub {
        "show" => {
            let config = crate::provider::resolved_config(ctx)?;
            let text = match config.harness.reasoning.uia.as_deref() {
                Some(level) => format!("Reasoning-Effort (UIA): {level}"),
                None => "Reasoning-Effort (UIA): (nicht gesetzt — Rollen-Default)".to_string(),
            };
            Ok(OpOutput::from(text))
        }
        "clear" | "none" | "auto" => {
            let persistence = crate::config_util::selection_persistence(ctx);
            Ok(handle_uia_effort_clear(move |effort| {
                persistence.persist_uia_reasoning_effort(effort)
            }))
        }
        other => {
            let persistence = crate::config_util::selection_persistence(ctx);
            handle_uia_effort_switch(other, move |effort| {
                persistence.persist_uia_reasoning_effort(effort)
            })
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::EffortArgs;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_operations::{FromRawArgs, OpInput};

    /// Prüft, dass `from_raw_args` das erste Token als `level` übernimmt.
    #[test]
    fn test_effort_args_from_raw_args_maps_level() -> TestResult {
        let args = EffortArgs::from_raw_args(&toks(&["high"]))
            .map_err(ctx("EffortArgs::from_raw_args"))?;
        assert_eq!(args.level.as_deref(), Some("high"));
        Ok(())
    }

    /// Prüft, dass leere Token-Liste `level = None` liefert.
    #[test]
    fn test_effort_args_from_empty_tokens_produces_none() -> TestResult {
        let args =
            EffortArgs::from_raw_args(&toks(&[])).map_err(ctx("EffortArgs::from_raw_args"))?;
        assert!(
            args.level.is_none(),
            "Leere Token-Liste muss level=None ergeben"
        );
        Ok(())
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
    fn test_effort_args_serde_deserialize_from_json_null_uses_default() -> TestResult {
        let json = r#"{"level": null}"#;
        let args: EffortArgs = serde_json::from_str(json)
            .map_err(ctx("Deserialisierung aus JSON null muss klappen"))?;
        assert!(args.level.is_none(), "level=null in JSON muss None ergeben");
        Ok(())
    }

    // ── OpInvocation coverage ─────────────────────────────────────────────────

    /// Prüft, dass `OpInput::command` ein Command-Invocation mit rohen Args erzeugt.
    #[test]
    fn test_op_input_command_is_command_with_raw_args() {
        let input = OpInput::command("/effort", vec!["medium".to_owned()]);
        assert!(input.invocation.is_command());
        assert_eq!(input.invocation.raw_args(), &["medium".to_owned()]);
    }

    // ── `/uia-effort` — Config-only Zwilling ────────────────────────────────

    use super::{handle_uia_effort_clear, handle_uia_effort_switch};
    use harw_operations::OpError;

    /// Prüft, dass ein gültiges Level validiert, kanonisiert und über
    /// `persist` mit `Some(level)` verankert wird — ohne jeglichen
    /// Live-`SessionController`-Aufruf (anders als `/effort`).
    #[test]
    fn handle_uia_effort_switch_persists_and_confirms() -> TestResult {
        let mut persisted: Option<String> = None;
        let result = handle_uia_effort_switch("high", |level| {
            persisted = level.map(str::to_owned);
            None
        });

        let output = result.map_err(ctx("ein gültiges Level darf nicht fehlschlagen"))?;
        assert_eq!(persisted.as_deref(), Some("high"));
        assert!(
            output
                .text
                .contains("Reasoning-Effort für die UIA gesetzt: high"),
            "unerwarteter Text: {}",
            output.text
        );
        assert!(
            output.text.contains("gespeichert für die nächste Sitzung"),
            "Bestätigungstext muss auf die nächste Sitzung verweisen: {}",
            output.text
        );
        Ok(())
    }

    /// Prüft, dass ein Persistenzfehler als angehängte Notiz zurückkommt,
    /// statt den Aufrufer fehlschlagen zu lassen (bestes Bemühen, analog
    /// `persist_uia_worker_model`).
    #[test]
    fn handle_uia_effort_switch_appends_persist_failure_note() -> TestResult {
        let result = handle_uia_effort_switch("medium", |_| {
            Some(
                "Hinweis: konnte UIA-Reasoning-Effort nicht dauerhaft speichern (boom).".to_owned(),
            )
        });

        let output = result.map_err(ctx(
            "Persistenzfehler darf den bereits validierten Wechsel nicht scheitern lassen",
        ))?;
        assert!(
            output
                .text
                .contains("konnte UIA-Reasoning-Effort nicht dauerhaft speichern"),
            "Fehlernotiz muss im Bestätigungstext auftauchen: {}",
            output.text
        );
        Ok(())
    }

    /// Prüft, dass ein unbekanntes Level abgelehnt wird und die Fehlermeldung
    /// alle sechs Effort-Stufen nennt — `persist` darf dabei nicht aufgerufen
    /// werden.
    #[test]
    fn uia_effort_rejects_unknown_level() -> TestResult {
        let persist_called = std::cell::Cell::new(false);
        let result = handle_uia_effort_switch("extreme", |_| {
            persist_called.set(true);
            None
        });
        assert!(
            !persist_called.get(),
            "persist darf bei ungültigem Level nicht aufgerufen werden"
        );

        match result {
            Err(OpError::InvalidArguments(msg)) => {
                assert!(msg.contains("extreme"), "Fehlertext ohne Eingabe: {msg}");
                for level in ["minimal", "low", "medium", "high", "xhigh", "max"] {
                    assert!(
                        msg.contains(level),
                        "Fehlertext muss '{level}' auflisten: {msg}"
                    );
                }
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "InvalidArguments erwartet, erhalten: {other:?}"
            ))),
        }
    }

    /// Prüft, dass `clear` den Pin explizit mit `None` entfernt (kein
    /// "unverändert lassen") und den Reset im Text bestätigt.
    #[test]
    fn handle_uia_effort_clear_removes_pin_and_confirms() {
        let mut persist_called_with_none = false;
        let output = handle_uia_effort_clear(|level| {
            persist_called_with_none = level.is_none();
            None
        });

        assert!(
            persist_called_with_none,
            "clear muss persist mit None aufrufen"
        );
        assert!(
            output
                .text
                .contains("Reasoning-Effort für die UIA zurückgesetzt"),
            "unerwarteter Text: {}",
            output.text
        );
    }

    // ── `/uia-effort show` — liest Config, nie den Live-Controller ──────────

    /// Baut einen minimalen `OpContext` mit optionalem `SharedSessionController`
    /// und context-gescopter `Arc<ResolvedConfig>` — Zwilling von
    /// `crate::model::tests::make_test_ctx`, hier lokal gehalten, weil nur
    /// `/uia-effort show` ihn braucht.
    fn make_test_ctx(
        ctrl: Option<harw_operations::SharedSessionController>,
        config: std::sync::Arc<harw_config::ResolvedConfig>,
    ) -> TestResult<(harw_operations::OpContext, std::path::PathBuf)> {
        use harw_authority::{
            Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
        };
        use harw_operations::context::ServiceMap;
        use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "harw-uia-effort-test-{}-{}",
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(tmp.join("ws")).map_err(ctx("Test-Workspace anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("WorkspaceRegistry::build"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("resolve binding"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let mut services = ServiceMap::new();
        if let Some(c) = ctrl {
            services.insert(c);
        }
        services.insert(config);
        let test_ctx =
            harw_operations::OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        Ok((test_ctx, tmp))
    }

    /// Prüft, dass `/uia-effort show` den Wert aus `config.harness.reasoning.uia`
    /// meldet und dabei einen abweichenden Live-Controller-Wert **ignoriert**
    /// — im Gegensatz zu `/effort show`, das bewusst den Live-Snapshot liest.
    #[tokio::test]
    async fn uia_effort_show_reports_config_value_without_a_live_override() -> TestResult {
        use harw_operations::{NullSessionController, SessionController, SharedSessionController};
        use harw_types::ReasoningEffort;
        use std::sync::Arc;

        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_reasoning_effort(Some(ReasoningEffort::Medium))
            .map_err(ctx("set_reasoning_effort muss gelingen"))?;
        let shared: SharedSessionController = ctrl as SharedSessionController;

        let mut config = harw_config::ResolvedConfig::default();
        config.harness.reasoning.uia = Some("high".to_owned());

        let (op_ctx, _tmp) = make_test_ctx(Some(shared), Arc::new(config))?;

        let args = EffortArgs {
            level: Some("show".to_owned()),
        };
        let result = super::uia_effort(&op_ctx, args)
            .await
            .map_err(ctx("show darf nicht fehlschlagen"))?;

        assert!(
            result.text.contains("high"),
            "show-Ausgabe muss den Config-Wert melden: {}",
            result.text
        );
        assert!(
            !result.text.contains("medium"),
            "show-Ausgabe darf den Live-Controller-Wert nicht durchsickern lassen: {}",
            result.text
        );
        Ok(())
    }

    /// Prüft, dass `/uia-effort show` ohne gesetzten Config-Wert den
    /// Rollen-Default-Hinweis meldet, statt (fälschlich) einen Live-Snapshot
    /// zu lesen.
    #[tokio::test]
    async fn uia_effort_show_reports_role_default_hint_when_unset() -> TestResult {
        let config = harw_config::ResolvedConfig::default();
        let (op_ctx, _tmp) = make_test_ctx(None, std::sync::Arc::new(config))?;

        let args = EffortArgs {
            level: Some("show".to_owned()),
        };
        let result = super::uia_effort(&op_ctx, args)
            .await
            .map_err(ctx("show darf nicht fehlschlagen"))?;

        assert!(
            result.text.contains("nicht gesetzt"),
            "show-Ausgabe muss den 'nicht gesetzt'-Hinweis melden: {}",
            result.text
        );
        Ok(())
    }
}
