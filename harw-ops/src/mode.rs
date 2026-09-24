//! `/mode` — Anzeige, Wechsel und Standard des Interaktionsmodus.
//!
//! # Verantwortungsbereich
//! Implementiert die `mode`-Operation (AP W4-05). Sie exponiert **nur** einen
//! Command `/mode` mit `tui_only`-Sichtbarkeit:
//!
//! | Eingabe | Wirkung |
//! |---|---|
//! | `/mode` / `/mode show` | Aktueller Modus, konfigurierter Standard, alle Modi mit Kurzbeschreibung |
//! | `/mode <modus>` | Fordert den Wechsel der laufenden Sitzung an (`chat|plan|explore|work|shell`) |
//! | `/mode default <modus>` | Persistiert `[mode] default` in der Profil-`config.toml` (neue Sitzungen) |
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
//! # Wie der Wechsel wirkt
//! [`OpContext`] ist unveränderlich; zustandsändernde Ops mutieren über den
//! [`SessionController`](harw_operations::session_control::SessionController)
//! aus der `ServiceMap`. Der Controller bietet dafür
//! [`request_mode`](harw_operations::session_control::SessionController::request_mode)
//! und trägt den aktuellen Modus im Snapshot
//! ([`SessionControlSnapshot::interaction_mode`](harw_operations::session_control::SessionControlSnapshot)).
//! Der Controller *merkt den Wunsch vor*; angewandt wird er von der Oberfläche
//! an der nächsten Turn-Grenze — ein laufender Turn darf seine Tool-Menge und
//! Sandbox-Obergrenze nicht unter sich wechseln. Deshalb meldet die Antwort
//! `requested` und nicht `applied`. Oberflächen ohne Modusführung lehnen fail-closed
//! mit `ModeUnsupported` ab; das wird als [`OpError::NotAvailable`] gemeldet.
//!
//! `/mode default <modus>` berührt die laufende Sitzung nicht, sondern schreibt
//! bestes Bemühen `[mode] default` über
//! [`crate::config_util::persist_default_interaction_mode`].
//!
//! # Schlüsseltypen
//! - [`ModeArgs`] — positionale Argumente `action target`.
//! - `ModeOperation` — vom `#[operation]`-Makro erzeugter Op-Struct.
//!
//! # Datenvertrag (`OpOutput::data`)
//! `show` → `{"action":"show","mode","known","default","available_modes":[{"name","summary"}]}`;
//! Wechsel → `{"action":"switch","mode","requested","available_modes","note"}`;
//! Standard → `{"action":"default","default","persisted","note"}`.
//!
//! # Nebenläufigkeit
//! `ModeOperation` ist ein zustandsloser Unit-Struct → `Send + Sync`.
//!
//! # Fehler
//! - [`OpError::InvalidArguments`]: unbekanntes Sub-Kommando oder unbekannter
//!   Modus (die Meldung listet die gültigen auf).
//! - [`OpError::NotAvailable`]: kein Controller registriert bzw. die Oberfläche
//!   führt keine Interaktionsmodi (nur `show` und Wechsel).
//! - [`OpError::Execution`]: Mutations-Kanal geschlossen.
//!
//! # Beispiel
//! ```rust
//! use harw_ops::mode::ModeArgs;
//! use harw_operations::FromRawArgs;
//!
//! // "/mode" ohne Argument zeigt den Modus an.
//! let show = ModeArgs::from_raw_args(&[]).ok();
//! assert!(show.is_some_and(|args| args.requested_mode().is_none()));
//! // "/mode explore" ist die Wechselabsicht.
//! let switch = ModeArgs::from_raw_args(&["explore".to_owned()]).ok();
//! assert!(switch.is_some_and(|args| args.requested_mode().is_some()));
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

/// Wann ein geänderter Standardmodus wirksam wird.
pub(crate) const DEFAULT_APPLIES_NEXT_SESSION: &str =
    "Der Standardmodus gilt ab der nächsten Sitzung; die laufende Sitzung bleibt unverändert.";

// ── Argumente ────────────────────────────────────────────────────────────────

/// Argumente der `mode`-Operation.
///
/// # Beschreibung
/// Positional: `action` ist `show` (Standard), ein Modusname oder `default`;
/// `target` ist bei `default` der neue Standardmodus. Die Validierung erfolgt
/// im Handler über [`InteractionMode::parse`], damit die Fehlermeldung alle
/// gültigen Eingaben nennen kann.
///
/// # Spec-Referenz
/// AP W4-05 — `/mode`; TUI-Vertrag — `/mode default <modus>`.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct ModeArgs {
    /// `show` (Standard), `chat|plan|explore|work|shell` oder `default`.
    #[serde(default)]
    #[raw(first)]
    pub action: Option<String>,
    /// Zielmodus für `default`.
    #[serde(default)]
    #[raw(nth = 1)]
    pub target: Option<String>,
}

/// Interne, validierte Form von [`ModeArgs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeCommand {
    Show,
    Switch(InteractionMode),
    SetDefault(InteractionMode),
}

/// Liste aller gültigen Modusnamen für Fehlermeldungen.
fn mode_names() -> String {
    InteractionMode::names().collect::<Vec<_>>().join(", ")
}

impl ModeArgs {
    /// Gibt den gewünschten Zielmodus der laufenden Sitzung zurück, falls es einer ist.
    ///
    /// # Rückgabe
    /// `Some(modus)`, wenn `action` ein Modusname ist; `None` für `show`,
    /// `default …` und unbekannte Eingaben (eine Anzeige bzw. ein
    /// Standardwechsel ist kein Sitzungswechsel).
    ///
    /// # Nebenläufigkeit
    /// Reine Funktion auf einem Werttyp.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_ops::mode::ModeArgs;
    ///
    /// assert!(ModeArgs::default().requested_mode().is_none());
    /// let explore = ModeArgs { action: Some("explore".to_owned()), target: None };
    /// assert!(explore.requested_mode().is_some());
    /// ```
    #[must_use]
    pub fn requested_mode(&self) -> Option<InteractionMode> {
        match self.command() {
            Ok(ModeCommand::Switch(mode)) => Some(mode),
            _ => None,
        }
    }

    /// Validiert die Argumente.
    fn command(&self) -> Result<ModeCommand, OpError> {
        let action = self
            .action
            .as_deref()
            .map(str::trim)
            .filter(|action| !action.is_empty())
            .unwrap_or("show");
        match action.to_ascii_lowercase().as_str() {
            "show" => Ok(ModeCommand::Show),
            "default" => {
                let target = self
                    .target
                    .as_deref()
                    .map(str::trim)
                    .filter(|target| !target.is_empty())
                    .ok_or_else(|| {
                        OpError::InvalidArguments(format!(
                            "Modus fehlt. Aufruf: /mode default <modus>. Gültig: {}.",
                            mode_names()
                        ))
                    })?;
                InteractionMode::parse(target)
                    .map(ModeCommand::SetDefault)
                    .ok_or_else(|| {
                        OpError::InvalidArguments(format!(
                            "Unbekannter Modus: '{target}'. Gültig: {}.",
                            mode_names()
                        ))
                    })
            }
            _ => InteractionMode::parse(action)
                .map(ModeCommand::Switch)
                .ok_or_else(|| {
                    OpError::InvalidArguments(format!(
                        "Unbekanntes /mode-Sub-Kommando: '{action}'. Gültig: show, {}, \
                         default <modus>.",
                        mode_names()
                    ))
                }),
        }
    }
}

// ── Hilfsfunktionen ──────────────────────────────────────────────────────────

/// Alle Modi als `[{"name","summary"}]` für den Datenvertrag.
fn available_modes_json() -> Value {
    Value::Array(
        InteractionMode::ALL
            .iter()
            .map(|mode| json!({ "name": mode.as_str(), "summary": mode.summary_de() }))
            .collect(),
    )
}

/// Baut Text und `data` für `show`.
fn render_show(current: Option<&str>, configured_default: Option<&str>) -> OpOutput {
    let mut lines = vec![
        format!("Modus: {}", current.unwrap_or("(unbekannt)")),
        format!(
            "Standard für neue Sitzungen: {}",
            configured_default.unwrap_or("(unbekannt)")
        ),
        String::new(),
        "Verfügbare Modi:".to_owned(),
    ];
    for mode in InteractionMode::ALL {
        let marker = if current == Some(mode.as_str()) {
            "*"
        } else {
            " "
        };
        lines.push(format!(
            "{marker} {:<8} {}",
            mode.as_str(),
            mode.summary_de()
        ));
    }
    lines.push(String::new());
    lines.push("Wechseln: /mode <modus> · Standard setzen: /mode default <modus>".to_owned());

    OpOutput {
        text: lines.join("\n"),
        data: Some(json!({
            "action": "show",
            "mode": current,
            "known": current.is_some(),
            "default": configured_default,
            "available_modes": available_modes_json(),
        })),
    }
}

/// Persistiert den Standardmodus über `persist` und baut die Antwort.
///
/// # Beschreibung
/// `persist` ist in Produktion
/// [`crate::config_util::persist_default_interaction_mode`]; die Injektion
/// erlaubt Tests ohne `HARW_HOME`-Zugriff (dieses Crate verbietet `unsafe`,
/// also auch Env-Isolation über `set_var`).
fn handle_set_default(
    mode: InteractionMode,
    persist: impl FnOnce(&str) -> Option<String>,
) -> OpOutput {
    let note = persist(mode.as_str());
    let mut text = format!("Standardmodus gesetzt: {}", mode.as_str());
    match note.as_deref() {
        Some(note) => {
            text.push('\n');
            text.push_str(note);
        }
        None => {
            text.push_str(" — gespeichert. ");
            text.push_str(DEFAULT_APPLIES_NEXT_SESSION);
        }
    }
    OpOutput {
        text,
        data: Some(json!({
            "action": "default",
            "default": mode.as_str(),
            "persisted": note.is_none(),
            "note": note,
        })),
    }
}

// ── Operation ────────────────────────────────────────────────────────────────

/// Zeigt den Interaktionsmodus, fordert einen Wechsel an oder setzt den Standard.
///
/// # Beschreibung
/// `show` liest den aktuellen Modus aus
/// [`SessionController::snapshot`](harw_operations::session_control::SessionController::snapshot).
/// Hat die Oberfläche nie einen Modus angefordert, meldet die Antwort
/// `known = false` statt einen Vorgabewert zu erfinden — ein erfundenes `"chat"`
/// wäre schlimmer als ein ehrliches „unbekannt", weil der Nutzer daraus auf
/// Werkzeug- und Sandbox-Grenzen schließt. `default` stammt aus der
/// aufgelösten Config (`[mode] default`).
///
/// `/mode <modus>` ruft
/// [`SessionController::request_mode`](harw_operations::session_control::SessionController::request_mode);
/// wirksam wird der Wechsel an der nächsten Turn-Grenze.
///
/// `/mode default <modus>` persistiert `[mode] default` (bestes Bemühen) und
/// braucht keinen Controller.
///
/// **Command only**: Das Modell darf diese Operation nicht selbst aufrufen; es
/// würde sonst sein eigenes Werkzeug-Ceiling verschieben. Diese Grenze wird von
/// der Flächen-Deklaration gezogen (kein `model_tool`, kein `agent_tool`).
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: unbekanntes Sub-Kommando/unbekannter Modus.
/// - [`OpError::NotAvailable`]: kein Controller registriert, oder die Oberfläche
///   führt keine Interaktionsmodi.
/// - [`OpError::Execution`]: Mutations-Kanal geschlossen.
///
/// # Nebenläufigkeit
/// Zustandslos; der Controller nimmt intern kurz einen Lock.
#[operation(
    name = "mode",
    summary = "Zeigt/wechselt den Interaktionsmodus (chat/plan/explore/work/shell) oder setzt den Standard (default <modus>).",
    domain = "session",
    permission = "operator",
    command(path = "/mode", visibility = "tui_only")
)]
async fn mode(ctx: &OpContext, args: ModeArgs) -> Result<OpOutput, OpError> {
    let command = args.command()?;

    if let ModeCommand::SetDefault(mode) = command {
        return Ok(handle_set_default(
            mode,
            crate::config_util::persist_default_interaction_mode,
        ));
    }

    // Ohne Controller gibt es keinen Adressaten — für Anzeige und Wechsel ein
    // Fehler und keine beschönigende Antwort: `show` würde sonst „unbekannt"
    // melden, obwohl der wahre Befund „nicht anschließbar" ist.
    let Some(controller) = ctx.service::<SharedSessionController>() else {
        return Err(OpError::NotAvailable(NO_CONTROLLER.to_owned()));
    };

    match command {
        ModeCommand::Switch(mode) => {
            controller
                .request_mode(mode.as_str())
                .map_err(map_control_error)?;
            Ok(OpOutput {
                text: format!(
                    "Modus angefordert: {}. {APPLIES_AT_TURN_BOUNDARY}",
                    mode.as_str()
                ),
                data: Some(json!({
                    "action": "switch",
                    "mode": mode.as_str(),
                    "requested": true,
                    "available_modes": available_modes_json(),
                    "note": APPLIES_AT_TURN_BOUNDARY,
                })),
            })
        }
        ModeCommand::Show | ModeCommand::SetDefault(_) => {
            let current = controller.snapshot().interaction_mode;
            let configured_default = crate::provider::resolved_config(ctx)
                .ok()
                .map(|config| config.harness.mode.default.clone());
            Ok(render_show(
                current.as_deref(),
                configured_default.as_deref(),
            ))
        }
    }
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
    use super::{ModeArgs, ModeCommand, ModeOperation, handle_set_default};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_core::InteractionMode;
    use harw_operations::context::ServiceMap;
    use harw_operations::{FromRawArgs, OpContext, OpError, Operation, Surface};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Baut einen minimalen [`OpContext`]; optional mit
    /// [`harw_operations::session_control::NullSessionController`]. Eine
    /// Standard-[`harw_config::ResolvedConfig`] wird immer injiziert, damit
    /// `show` nie die echte `HARW_HOME`-Config liest.
    fn test_context(with_controller: bool) -> TestResult<(OpContext, std::path::PathBuf)> {
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
        let mut services = ServiceMap::new();
        services.insert(Arc::new(harw_config::ResolvedConfig::default()));
        if with_controller {
            let controller: harw_operations::SharedSessionController =
                Arc::new(harw_operations::session_control::NullSessionController::new());
            services.insert(controller);
        }
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            root,
        ))
    }

    fn args(tokens: &[&str]) -> TestResult<ModeArgs> {
        ModeArgs::from_raw_args(&toks(tokens)).map_err(ctx("ModeArgs::from_raw_args"))
    }

    /// Führt `/mode` mit registriertem Null-Controller aus und liefert `data`.
    async fn run(tokens: &[&str]) -> TestResult<serde_json::Value> {
        let (op_ctx, root) = test_context(true)?;
        let result = super::mode(&op_ctx, args(tokens)?).await;
        std::fs::remove_dir_all(root).ok();

        let output = result.map_err(|error| {
            TestError::Unexpected(format!("/mode darf hier nicht fehlschlagen: {error}"))
        })?;
        output.data.ok_or(TestError::Missing("OpOutput.data"))
    }

    #[test]
    fn test_mode_args_default_is_show() -> TestResult {
        assert_eq!(
            ModeArgs::default().command().map_err(ctx("command"))?,
            ModeCommand::Show
        );
        Ok(())
    }

    #[test]
    fn test_mode_args_from_raw_args_without_tokens_is_show() -> TestResult {
        let parsed = args(&[])?;
        assert_eq!(parsed, ModeArgs::default());
        assert_eq!(parsed.command().map_err(ctx("command"))?, ModeCommand::Show);
        Ok(())
    }

    #[test]
    fn test_mode_args_parses_every_subcommand() -> TestResult {
        for (tokens, expected) in [
            (vec!["show"], ModeCommand::Show),
            (vec!["chat"], ModeCommand::Switch(InteractionMode::Chat)),
            (vec!["plan"], ModeCommand::Switch(InteractionMode::Plan)),
            (
                vec!["explore"],
                ModeCommand::Switch(InteractionMode::Explore),
            ),
            (vec!["work"], ModeCommand::Switch(InteractionMode::Work)),
            (vec!["shell"], ModeCommand::Switch(InteractionMode::Shell)),
            (
                vec!["default", "explore"],
                ModeCommand::SetDefault(InteractionMode::Explore),
            ),
            (
                vec!["default", "Work"],
                ModeCommand::SetDefault(InteractionMode::Work),
            ),
        ] {
            let command = args(&tokens)?.command().map_err(|error| {
                TestError::Unexpected(format!("{tokens:?} schlug fehl: {error}"))
            })?;
            assert_eq!(command, expected, "{tokens:?}");
        }
        Ok(())
    }

    #[test]
    fn test_mode_args_rejects_unknown_subcommand() -> TestResult {
        match args(&["turbo"])?.command() {
            Err(OpError::InvalidArguments(message)) => {
                assert!(
                    message.contains("explore") && message.contains("default"),
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
    fn test_mode_default_rejects_missing_and_unknown_mode() -> TestResult {
        for (tokens, needle) in [
            (vec!["default"], "Modus fehlt"),
            (vec!["default", "turbo"], "Unbekannter Modus"),
        ] {
            match args(&tokens)?.command() {
                Err(OpError::InvalidArguments(message)) => assert!(
                    message.contains(needle) && message.contains("shell"),
                    "{tokens:?}: {message}"
                ),
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{tokens:?}: erwartet InvalidArguments, war: {other:?}"
                    )));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn test_requested_mode_maps_each_variant() -> TestResult {
        assert!(args(&["show"])?.requested_mode().is_none());
        assert!(args(&["default", "work"])?.requested_mode().is_none());
        assert!(args(&["turbo"])?.requested_mode().is_none());
        for mode in InteractionMode::ALL {
            assert_eq!(args(&[mode.as_str()])?.requested_mode(), Some(mode));
        }
        Ok(())
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
        let report = run(&["show"]).await?;
        assert_eq!(report["action"], serde_json::json!("show"));
        assert_eq!(report["mode"], serde_json::Value::Null);
        assert_eq!(report["known"], serde_json::json!(false));
        Ok(())
    }

    #[tokio::test]
    async fn test_mode_show_reports_configured_default() -> TestResult {
        // Die injizierte Standard-Config trägt `[mode] default = "chat"`.
        let report = run(&[]).await?;
        assert_eq!(report["default"], serde_json::json!("chat"));
        Ok(())
    }

    #[tokio::test]
    async fn test_mode_show_without_a_controller_is_an_error_not_an_empty_report() -> TestResult {
        // Ohne Controller gibt es keinen Adressaten. Ein beschönigendes
        // „unbekannt" wäre die falsche Auskunft: der wahre Befund ist
        // „nicht anschließbar".
        let (op_ctx, root) = test_context(false)?;
        let result = super::mode(&op_ctx, ModeArgs::default()).await;
        std::fs::remove_dir_all(root).ok();
        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "ohne Controller muss /mode fehlschlagen, war: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_mode_switch_requests_the_mode_and_says_when_it_applies() -> TestResult {
        let report = run(&["explore"]).await?;
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
        let (op_ctx, root) = test_context(true)?;
        let switched = super::mode(&op_ctx, args(&["explore"])?).await;
        assert!(switched.is_ok(), "Wechsel schlug fehl: {switched:?}");

        let shown = super::mode(&op_ctx, args(&["show"])?).await;
        std::fs::remove_dir_all(root).ok();

        let output = shown.map_err(|error| {
            TestError::Unexpected(format!("/mode show darf nicht fehlschlagen: {error}"))
        })?;
        let report = output.data.ok_or(TestError::Missing("OpOutput.data"))?;
        assert_eq!(report["mode"], serde_json::json!("explore"));
        assert_eq!(report["known"], serde_json::json!(true));
        assert!(output.text.contains("* explore"), "{}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn test_mode_lists_all_available_modes_with_summaries() -> TestResult {
        let report = run(&["show"]).await?;
        let modes = report["available_modes"]
            .as_array()
            .ok_or(TestError::Missing("available_modes"))?;
        let names: Vec<&str> = modes
            .iter()
            .filter_map(|mode| mode["name"].as_str())
            .collect();
        assert_eq!(names, ["chat", "plan", "explore", "work", "shell"]);
        for (entry, mode) in modes.iter().zip(InteractionMode::ALL) {
            assert_eq!(entry["summary"], serde_json::json!(mode.summary_de()));
        }
        Ok(())
    }

    #[test]
    fn test_handle_set_default_persists_canonical_name() {
        let mut persisted = None;
        let output = handle_set_default(InteractionMode::Explore, |mode| {
            persisted = Some(mode.to_owned());
            None
        });
        assert_eq!(persisted.as_deref(), Some("explore"));
        assert!(
            output.text.contains("Standardmodus gesetzt: explore"),
            "{}",
            output.text
        );
        assert!(output.text.contains("nächsten Sitzung"), "{}", output.text);
        let data = output.data.unwrap_or_default();
        assert_eq!(data["default"], serde_json::json!("explore"));
        assert_eq!(data["persisted"], serde_json::json!(true));
    }

    #[test]
    fn test_handle_set_default_reports_persist_failure_note() {
        let output = handle_set_default(InteractionMode::Chat, |_| {
            Some("Hinweis: konnte den Standardmodus nicht dauerhaft speichern (boom).".to_owned())
        });
        assert!(output.text.contains("(boom)"), "{}", output.text);
        let data = output.data.unwrap_or_default();
        assert_eq!(data["persisted"], serde_json::json!(false));
    }
}
