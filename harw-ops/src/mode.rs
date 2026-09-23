//! `/mode` — Anzeige und Wechselabsicht des Interaktionsmodus.
//!
//! # Verantwortungsbereich
//! Implementiert die `mode`-Operation gemäß AP W4-05. Sie exponiert **nur**
//! einen Command `/mode` mit `tui_only`-Sichtbarkeit.
//!
//! # Warum kein `model_tool`
//! Aus demselben Grund wie bei [`crate::agent`]: das Modell darf sich nicht
//! selbst manipulieren. Der Interaktionsmodus bestimmt, welche Werkzeuge das
//! Modell überhaupt sehen darf ([`InteractionMode::tool_profile`],
//! [`InteractionMode::permission_ceiling`]). Ein Modell, das seinen eigenen
//! Modus umschalten könnte, könnte damit sein eigenes Werkzeug-Ceiling anheben —
//! die Beschränkung wäre eine Bitte statt einer Grenze. Der Moduswechsel bleibt
//! deshalb eine Handlung des Menschen an der TUI.
//!
//! # Warum die Operation den Wechsel nicht vollzieht
//! [`OpContext`] ist unveränderlich; zustandsändernde Ops mutieren über den
//! [`SessionController`](harw_operations::session_control::SessionController)
//! aus der `ServiceMap` (so machen es `/effort`, `/model switch`,
//! `/provider switch`). Dieser Trait kennt **heute keinen Interaktionsmodus**:
//! er hat `set_reasoning_effort`, `set_active_model`, `set_active_provider` und
//! `snapshot`, und [`SessionControlSnapshot`](harw_operations::session_control::SessionControlSnapshot)
//! trägt kein Modus-Feld.
//!
//! Solange das so ist, gibt `/mode` die **Absicht** strukturiert zurück
//! (`{"mode": "explore", "applied": false, …}`) und benennt genau, was fehlt.
//! Ein stilles `Ok` ohne Wirkung wäre die schlechtere Antwort: der Aufrufer
//! glaubte dann, der Modus sei gewechselt.
//!
//! Fehlende Signaturen (siehe `CONTROLLER_LACKS_MODE` in dieser Datei):
//! ```rust,ignore
//! fn set_interaction_mode(
//!     &self,
//!     mode: harw_core::InteractionMode,
//! ) -> Result<(), SessionControlError>;
//! // und in SessionControlSnapshot:
//! pub interaction_mode: Option<harw_core::InteractionMode>,
//! ```
//!
//! # Schlüsseltypen
//! - [`ModeArgs`] — Subcommand-Enum (`show`, `chat`, `plan`, `explore`, `work`,
//!   `shell`).
//! - `ModeOperation` — vom `#[operation]`-Makro erzeugter Op-Struct.
//!
//! # Nebenläufigkeit
//! `ModeOperation` ist ein zustandsloser Unit-Struct → `Send + Sync`.
//!
//! # Fehler
//! - [`OpError::InvalidArguments`]: unbekanntes Subcommand (die Meldung listet
//!   die gültigen auf — vom `FromRawArgs`-Derive erzeugt).
//! - [`OpError::Execution`]: der Bericht ist nicht serialisierbar.
//!
//! # Beispiel
//! ```rust
//! use harw_ops::mode::ModeArgs;
//! use harw_operations::FromRawArgs;
//!
//! // "/mode" ohne Argument zeigt den Modus an.
//! assert!(matches!(
//!     ModeArgs::from_raw_args(&[]),
//!     Ok(ModeArgs::Show)
//! ));
//! // "/mode explore" ist die Wechselabsicht.
//! assert!(matches!(
//!     ModeArgs::from_raw_args(&["explore".to_owned()]),
//!     Ok(ModeArgs::Explore)
//! ));
//! ```

use harw_core::InteractionMode;
use harw_macros::operation;
use harw_operations::session_control::SessionControlError;
use harw_operations::{OpContext, OpError, OpOutput, SharedSessionController};
use serde_json::{Value, json};

// ── Konstanten ───────────────────────────────────────────────────────────────

/// Meldung für den Fall, dass gar kein [`SharedSessionController`] registriert ist.
///
/// Das ist kein Fehler der Operation, sondern eine unvollständige
/// Zusammenstellung der Laufzeit: ohne Controller gibt es keinen Adressaten für
/// den Wunsch. Die Antwort sagt das, statt einen Wechsel zu behaupten.
pub(crate) const NO_CONTROLLER: &str = "In dieser Laufzeit ist kein SessionController registriert — der Modus kann \
     weder gelesen noch gewechselt werden. Die Oberfläche muss einen \
     `Arc<dyn SessionController>` in die ServiceMap legen.";

/// Wann ein angeforderter Wechsel wirksam wird.
///
/// Der Controller *merkt den Wunsch vor*; angewandt wird er von der Oberfläche
/// an der Turn-Grenze (`TuiSessionController::apply_to_session`). Ein laufender
/// Turn darf seine Tool-Menge und Sandbox-Obergrenze nicht unter sich wechseln.
pub(crate) const APPLIES_AT_TURN_BOUNDARY: &str =
    "Der Wechsel ist vorgemerkt und wird an der nächsten Turn-Grenze wirksam.";

/// Alle wählbaren Modi in der Reihenfolge der Subcommands.
const AVAILABLE_MODES: &[InteractionMode] = &[
    InteractionMode::Chat,
    InteractionMode::Plan,
    InteractionMode::Explore,
    InteractionMode::Work,
    InteractionMode::Shell,
];

// ── Argumente ────────────────────────────────────────────────────────────────

/// Subcommands der `mode`-Operation.
///
/// # Beschreibung
/// `/mode` ohne Argument ist [`Self::Show`] (`#[raw(default_subcommand)]`); jede
/// andere Variante ist die Absicht, in den gleichnamigen
/// [`InteractionMode`] zu wechseln. Ein unbekanntes Token liefert
/// [`OpError::InvalidArguments`] mit der Liste aller Subcommands — erzeugt vom
/// `FromRawArgs`-Derive.
///
/// # Spec-Referenz
/// AP W4-05 — `/mode`.
#[derive(Debug, Default, PartialEq, Eq, serde::Deserialize, harw_macros::FromRawArgs)]
#[serde(rename_all = "snake_case")]
#[raw(subcommand)]
pub enum ModeArgs {
    /// Zeigt den aktuellen Modus an (Vorgabe ohne Argument).
    #[default]
    #[raw(default_subcommand)]
    Show,
    /// Wechselabsicht: Gespräch, keine namensbasierte Werkzeug-Einschränkung.
    Chat,
    /// Wechselabsicht: Planung — Plan-/Goal-Werkzeuge und Recherche.
    Plan,
    /// Wechselabsicht: Exploration — ausschließlich lesende Werkzeuge.
    Explore,
    /// Wechselabsicht: Ausführung — voller Werkzeugsatz.
    Work,
    /// Wechselabsicht: Host-Arbeit — voller Werkzeugsatz, das Modell soll
    /// Host-Befehle bevorzugt an den `uia-shell-worker` delegieren; eine
    /// Host-Freigabe erteilt weiterhin nur der Mensch.
    Shell,
}

impl ModeArgs {
    /// Gibt den gewünschten Zielmodus zurück, falls es einer ist.
    ///
    /// # Rückgabe
    /// `None` für [`Self::Show`] (eine Anzeige ist kein Wechsel), sonst der
    /// zugehörige [`InteractionMode`].
    ///
    /// # Nebenläufigkeit
    /// Reine Funktion auf einem Werttyp.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_ops::mode::ModeArgs;
    ///
    /// assert!(ModeArgs::Show.requested_mode().is_none());
    /// assert!(ModeArgs::Explore.requested_mode().is_some());
    /// ```
    #[must_use]
    pub fn requested_mode(&self) -> Option<InteractionMode> {
        match self {
            Self::Show => None,
            Self::Chat => Some(InteractionMode::Chat),
            Self::Plan => Some(InteractionMode::Plan),
            Self::Explore => Some(InteractionMode::Explore),
            Self::Work => Some(InteractionMode::Work),
            Self::Shell => Some(InteractionMode::Shell),
        }
    }
}

// ── Operation ────────────────────────────────────────────────────────────────

/// Zeigt den Interaktionsmodus an oder meldet die Wechselabsicht.
///
/// # Beschreibung
/// `show` liest den aktuellen Modus aus
/// [`SessionController::snapshot`](harw_operations::session_control::SessionController::snapshot).
/// Hat die Oberfläche nie einen Modus angefordert, meldet die Antwort
/// `known = false` statt einen Vorgabewert zu erfinden — ein erfundenes `"chat"`
/// wäre schlimmer als ein ehrliches „unbekannt", weil der Nutzer daraus auf
/// Werkzeug- und Sandbox-Grenzen schließt.
///
/// Jedes andere Subcommand ruft
/// [`SessionController::request_mode`](harw_operations::session_control::SessionController::request_mode).
/// Der Controller merkt den Wunsch vor; wirksam wird er an der nächsten
/// Turn-Grenze — deshalb heißt das Feld `requested` und nicht `applied`.
///
/// **Command only**: Das Modell darf diese Operation nicht selbst aufrufen; es
/// würde sonst sein eigenes Werkzeug-Ceiling verschieben. Diese Grenze wird von
/// der Flächen-Deklaration gezogen (kein `model_tool`, kein `agent_tool`), nicht
/// erst im Rumpf.
///
/// # Argumente
/// - `ctx` (`&OpContext`): liefert die `ServiceMap` mit dem
///   [`SharedSessionController`].
/// - `args` ([`ModeArgs`]): das Subcommand.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit einem JSON-Bericht.
///
/// # Fehler
/// - [`OpError::NotAvailable`]: kein Controller registriert, oder die Oberfläche
///   führt keine Interaktionsmodi
///   ([`SessionControlError::ModeUnsupported`](harw_operations::session_control::SessionControlError::ModeUnsupported)).
/// - [`OpError::Execution`]: Mutations-Kanal geschlossen, oder der Bericht ist
///   nicht serialisierbar.
///
/// # Nebenläufigkeit
/// Zustandslos; der Controller nimmt intern kurz einen Lock.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run().
/// ```
#[operation(
    name = "mode",
    summary = "Zeigt den Interaktionsmodus oder meldet die Wechselabsicht (chat/plan/explore/work/shell).",
    domain = "session",
    permission = "operator",
    command(path = "/mode", visibility = "tui_only")
)]
async fn mode(ctx: &OpContext, args: ModeArgs) -> Result<OpOutput, OpError> {
    let available: Vec<&'static str> = AVAILABLE_MODES.iter().map(|mode| mode.as_str()).collect();

    // Ohne Controller gibt es keinen Adressaten — für beide Subcommands ein
    // Fehler und keine beschönigende Antwort: `show` würde sonst „unbekannt"
    // melden, obwohl der wahre Befund „nicht anschließbar" ist.
    let Some(controller) = ctx.service::<SharedSessionController>() else {
        return Err(OpError::NotAvailable(NO_CONTROLLER.to_owned()));
    };

    let report = match args.requested_mode() {
        None => {
            let current = controller.snapshot().interaction_mode;
            json!({
                "action": "show",
                "mode": current.clone().map_or(Value::Null, Value::String),
                "known": current.is_some(),
                "available_modes": available,
            })
        }
        Some(mode) => {
            controller
                .request_mode(mode.as_str())
                .map_err(map_control_error)?;
            json!({
                "action": "switch",
                "mode": mode.as_str(),
                "requested": true,
                "available_modes": available,
                "note": APPLIES_AT_TURN_BOUNDARY,
            })
        }
    };

    let text = serde_json::to_string_pretty(&report)
        .map_err(|error| OpError::Execution(format!("Bericht nicht serialisierbar: {error}")))?;
    Ok(OpOutput::from(text))
}

/// Bildet einen [`SessionControlError`] auf die passende [`OpError`]-Variante ab.
///
/// Die Unterscheidung ist inhaltlich, nicht kosmetisch: `ModeUnsupported` heißt
/// „diese Oberfläche kann das nicht" (eine Verfügbarkeitsaussage), `Disconnected`
/// heißt „der Kanal ist weg" (ein Ausführungsfehler). Beide auf `Execution` zu
/// werfen, würde dem Aufrufer die Unterscheidung nehmen, ob ein Wiederholen
/// sinnvoll ist.
fn map_control_error(error: SessionControlError) -> OpError {
    match error {
        SessionControlError::ModeUnsupported(mode) => OpError::NotAvailable(format!(
            "Diese Oberfläche führt keinen Interaktionsmodus (angefordert: {mode})."
        )),
        other => OpError::Execution(format!("Moduswechsel fehlgeschlagen: {other}")),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{ModeArgs, ModeOperation};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_core::InteractionMode;
    use harw_operations::context::ServiceMap;
    use harw_operations::{FromRawArgs, OpContext, OpError, Operation, Surface};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Baut einen minimalen [`OpContext`] mit leerer [`ServiceMap`].
    fn test_context() -> TestResult<(OpContext, std::path::PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("harw-mode-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).map_err(ctx("Test-Workspace anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Workspace-Registry bauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace-Binding auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        ))
    }

    /// Führt `/mode` aus und liefert den Bericht als JSON.
    /// Führt `/mode` mit registriertem [`NullSessionController`] aus.
    ///
    /// Der Null-Controller zeichnet den Wunsch auf und gibt ihn im Snapshot
    /// zurück — genau das, was die Operation von einer echten Oberfläche
    /// erwartet, ohne eine TUI zu brauchen.
    async fn run(args: ModeArgs) -> TestResult<serde_json::Value> {
        let (op_ctx, root) = test_context_with_controller()?;
        let result = super::mode(&op_ctx, args).await;
        std::fs::remove_dir_all(root).ok();

        let output = result.map_err(|error| {
            TestError::Unexpected(format!("/mode darf hier nicht fehlschlagen: {error}"))
        })?;
        serde_json::from_str(&output.text).map_err(ctx("/mode-Ausgabe ist kein JSON"))
    }

    /// Baut einen [`OpContext`] mit einem [`NullSessionController`] in der
    /// [`ServiceMap`].
    fn test_context_with_controller() -> TestResult<(OpContext, std::path::PathBuf)> {
        let (op_ctx, root) = test_context()?;
        let controller: harw_operations::SharedSessionController =
            std::sync::Arc::new(harw_operations::session_control::NullSessionController::new());
        let mut services = ServiceMap::new();
        services.insert(controller);
        Ok((
            OpContext::new(
                op_ctx.session_id().clone(),
                op_ctx.turn_id().clone(),
                op_ctx.sandbox().clone(),
                services,
            ),
            root,
        ))
    }

    #[test]
    fn test_mode_args_default_is_show() {
        assert_eq!(ModeArgs::default(), ModeArgs::Show);
    }

    #[test]
    fn test_mode_args_from_raw_args_without_tokens_is_show() -> TestResult {
        let args = ModeArgs::from_raw_args(&toks(&[])).map_err(ctx("ModeArgs::from_raw_args"))?;
        assert_eq!(args, ModeArgs::Show);
        Ok(())
    }

    #[test]
    fn test_mode_args_from_raw_args_parses_every_subcommand() -> TestResult {
        for (token, expected) in [
            ("show", ModeArgs::Show),
            ("chat", ModeArgs::Chat),
            ("plan", ModeArgs::Plan),
            ("explore", ModeArgs::Explore),
            ("work", ModeArgs::Work),
            ("shell", ModeArgs::Shell),
        ] {
            let args = ModeArgs::from_raw_args(&toks(&[token])).map_err(|error| {
                TestError::Unexpected(format!("Subcommand '{token}' schlug fehl: {error}"))
            })?;
            assert_eq!(args, expected, "Subcommand '{token}'");
        }
        Ok(())
    }

    #[test]
    fn test_mode_args_from_raw_args_rejects_unknown_subcommand() -> TestResult {
        match ModeArgs::from_raw_args(&toks(&["turbo"])) {
            Err(OpError::InvalidArguments(message)) => {
                assert!(
                    message.contains("explore"),
                    "die Meldung muss die gültigen Subcommands nennen: {message}"
                );
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "erwartet InvalidArguments, war: {other:?}"
            ))),
        }
    }

    #[test]
    fn test_requested_mode_maps_each_variant() {
        assert!(ModeArgs::Show.requested_mode().is_none());
        assert_eq!(ModeArgs::Chat.requested_mode(), Some(InteractionMode::Chat));
        assert_eq!(ModeArgs::Plan.requested_mode(), Some(InteractionMode::Plan));
        assert_eq!(
            ModeArgs::Explore.requested_mode(),
            Some(InteractionMode::Explore)
        );
        assert_eq!(ModeArgs::Work.requested_mode(), Some(InteractionMode::Work));
        assert_eq!(
            ModeArgs::Shell.requested_mode(),
            Some(InteractionMode::Shell)
        );
    }

    #[test]
    fn test_mode_declares_no_model_tool_surface() {
        let meta = ModeOperation.meta();
        assert_eq!(meta.name, "mode");
        assert!(
            !meta
                .surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. })),
            "das Modell darf seinen eigenen Modus nicht wechseln"
        );
        assert!(
            !meta
                .surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::AgentTool { .. })),
            "/mode ist keine Kind-Fläche"
        );
        assert!(meta.surfaces.iter().any(|surface| matches!(
            surface,
            Surface::Command {
                path,
                visibility: harw_operations::CommandVisibility::TuiOnly
            } if *path == "/mode"
        )));
    }

    #[tokio::test]
    async fn test_mode_show_before_any_switch_reports_unknown_mode() -> TestResult {
        // Ein Controller ist da, hat aber nie einen Modus gesehen. Die Antwort
        // muss das zugeben statt `"chat"` zu erfinden — der Nutzer schließt aus
        // dem Modus auf Werkzeug- und Sandbox-Grenzen.
        let report = run(ModeArgs::Show).await?;
        assert_eq!(report["action"], serde_json::json!("show"));
        assert_eq!(report["mode"], serde_json::Value::Null);
        assert_eq!(report["known"], serde_json::json!(false));
        Ok(())
    }

    #[tokio::test]
    async fn test_mode_show_without_a_controller_is_an_error_not_an_empty_report() -> TestResult {
        // Ohne Controller gibt es keinen Adressaten. Ein beschönigendes
        // „unbekannt" wäre die falsche Auskunft: der wahre Befund ist
        // „nicht anschließbar".
        let (op_ctx, root) = test_context()?;
        let result = super::mode(&op_ctx, ModeArgs::Show).await;
        std::fs::remove_dir_all(root).ok();
        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "ohne Controller muss /mode fehlschlagen, war: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_mode_switch_requests_the_mode_and_says_when_it_applies() -> TestResult {
        let report = run(ModeArgs::Explore).await?;
        assert_eq!(report["action"], serde_json::json!("switch"));
        assert_eq!(report["mode"], serde_json::json!("explore"));
        assert_eq!(
            report["requested"],
            serde_json::json!(true),
            "der Wunsch muss beim Controller angekommen sein"
        );
        // `requested`, nicht `applied`: der Controller merkt vor, angewandt
        // wird an der Turn-Grenze.
        assert!(
            report["note"]
                .as_str()
                .is_some_and(|note| note.contains("Turn-Grenze")),
            "die Antwort muss sagen, wann der Wechsel wirkt: {report}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_mode_switch_is_visible_in_the_following_show() -> TestResult {
        // Der eigentliche Beweis, dass die Kette geschlossen ist: erst
        // wechseln, dann anzeigen — der Controller hat den Modus behalten.
        let (op_ctx, root) = test_context_with_controller()?;
        let switched = super::mode(&op_ctx, ModeArgs::Explore).await;
        assert!(switched.is_ok(), "Wechsel schlug fehl: {switched:?}");

        let shown = super::mode(&op_ctx, ModeArgs::Show).await;
        std::fs::remove_dir_all(root).ok();

        let output = shown.map_err(|error| {
            TestError::Unexpected(format!("/mode show darf nicht fehlschlagen: {error}"))
        })?;
        let report: serde_json::Value =
            serde_json::from_str(&output.text).map_err(ctx("/mode-Ausgabe ist kein JSON"))?;
        assert_eq!(report["mode"], serde_json::json!("explore"));
        assert_eq!(report["known"], serde_json::json!(true));
        Ok(())
    }

    #[tokio::test]
    async fn test_mode_lists_all_available_modes() -> TestResult {
        let report = run(ModeArgs::Show).await?;
        assert_eq!(
            report["available_modes"],
            serde_json::json!(["chat", "plan", "explore", "work", "shell"])
        );
        Ok(())
    }
}
