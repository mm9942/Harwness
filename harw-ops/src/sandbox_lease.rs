//! `/sandbox-lease` — Modell-getriebene Aufhebung der Sandbox für `shell.exec`.
//!
//! Neue Operation `sandbox-lease` mit einem
//! `Arc<harw_tool_shell::HostPermitHandles>`-ServiceMap-Eintrag.
//!
//! # Verantwortungsbereich
//! Diese Operation ist der **einzige** Weg, über den ein Modell selbst eine
//! Host-Freigabe für `shell.exec` anfordern kann — der Bestätigungsdialog,
//! den sie auslöst, ist die Freigabe selbst, deshalb trägt das `model_tool`
//! bewusst `approval = "none"`. Sie mutiert **nie** direkt einen
//! [`harw_sandbox::ProcessPermitLedger`]-Permit; sie setzt ausschließlich den
//! **prozessweiten** Zustand in [`harw_sandbox::HostPermitSessionRegistry`]
//! (`mark_global_approval`/`mark_global_single_use`/`revoke_global_approval`,
//! Nutzerwunsch „volle Sandbox-Deaktivierung“, 2026-09-21), den
//! [`harw_tool_shell::exec::ShellExecutor::run_command`] bereits ohne
//! erneute Rückfrage abfragt (Plan Teil B1). Eine über dieses Werkzeug
//! erteilte Freigabe gilt damit für **jede** Session-ID desselben
//! `harw`-Prozesses — die Root-Session ebenso wie jede Kind-Session (z. B.
//! `uia-shell-worker`/`host-process-worker`), nicht nur für die Session, die
//! `request` aufgerufen hat. Ein Widerruf räumt zusätzlich bereits
//! ausgestellte Ledger-Permits der aufrufenden Sitzung ab
//! ([`harw_sandbox::ProcessPermitLedger::revoke_session`]).
//!
//! # Lebensdauer einer Freigabe (Nutzerentscheidung 2026-09-24)
//! Eine erteilte Host-Arbeitsphase hat **keinen** Zeitablauf. Sie bleibt
//! aktiv, bis der **Nutzer** sie beendet — per Strg+H in der TUI
//! (`ChatApp::end_host_mode`) oder durch das getippte `/sandbox-lease
//! revoke`. Das Modell kann sie nicht beenden: `revoke` wird nur angenommen,
//! wenn der [`OpContext`] den Marker [`HostLeaseUserControl`] trägt, den die
//! Montage ausschließlich in die Slash-`ServiceMap` legt
//! (`harw-runtime/src/services.rs`). Auf der Model-Tool-Fläche (und Web/Job)
//! fehlt er, `revoke` endet dort mit [`OpError::NotAvailable`]. Der Marker
//! ist kein Argumentfeld und damit über Tool-Argumente nicht fälschbar.
//!
//! # Unterkommandos (beide Flächen, gemeinsamer Rumpf)
//! - `request` (Vorgabe auf der Model-Tool-Fläche, siehe
//!   [`SandboxLeaseArgs::from_raw_args`] für den abweichenden Command-Vorgabe-
//!   Wert) — verlangt `reason` (nicht-leer). Ist bereits eine prozessweite
//!   Freigabe aktiv, wird sofort bestätigt, **ohne**
//!   erneut zu fragen. Sonst geht ein [`harw_tool_shell::HostPermitPrompt`]
//!   an die anzeigende Oberfläche (Vorauswahl
//!   [`harw_tool_shell::HostPermitVariant::SessionLease`]); die Operation
//!   wartet höchstens [`harw_tool_shell::HOST_PERMIT_PROMPT_TIMEOUT`] lang auf
//!   eine Antwort. Jeder Ausgang außer einer ausdrücklichen Zustimmung —
//!   Ablehnung, Zeitablauf, fallengelassene Antwort, kein Fragekanal
//!   angehängt — ist ein Fehler (fail-closed, dieselbe Sicherheitsregel wie
//!   [`harw_tool_shell::exec`]).
//! - `status` (Vorgabe auf der Command-Fläche für ein bares
//!   `/sandbox-lease`) — meldet „aktiv (bis Strg+H oder /sandbox-lease
//!   revoke)“, „Einmalfreigabe offen“ oder „aus“, rein lesend;
//!   berücksichtigt sowohl die prozessweite als auch eine etwaige
//!   sitzungseigene Freigabe der aufrufenden Sitzung.
//! - `revoke` (nur Command-Fläche, nur mit [`HostLeaseUserControl`]) —
//!   entfernt die prozessweite Sitzungs- und Einmalfreigabe
//!   ([`harw_sandbox::HostPermitSessionRegistry::revoke_global_approval`])
//!   sowie eine etwaige sitzungseigene Freigabe der aufrufenden Sitzung
//!   ([`harw_sandbox::HostPermitSessionRegistry::revoke_session_approval`])
//!   **und** widerruft bereits ausgestellte Ledger-Permits der aufrufenden
//!   Sitzung; idempotent (ein zweiter Aufruf ist ein No-Op, kein Fehler).
//!
//! # Warum die Vorgabe je Fläche unterschiedlich ist
//! [`SandboxLeaseArgs::from_raw_args`] löst den Command-Vorgabewert
//! (`"status"`, sicher und rein lesend für ein bares `/sandbox-lease`) bereits
//! beim Parsen der Command-Fläche auf `Some("status")` auf. Der Operationsrumpf
//! fällt nur dann auf `"request"` zurück, wenn `action` `None` bleibt — das
//! passiert ausschließlich über die JSON-Flächen (Model-Tool/Agent-Tool), wenn
//! kein `action`-Feld mitgeschickt wurde. Ein bares Modell-Tool-Aufruf ohne
//! `action` ergibt damit `request` (der einzige praktisch sinnvolle Zweck
//! dieses Werkzeugs für ein Modell), ein bares `/sandbox-lease` dagegen
//! `status` (kein Seiteneffekt ohne ausdrückliche Absicht).
//!
//! # Fehler
//! - [`OpError::NotAvailable`]: kein `Arc<`[`harw_tool_shell::HostPermitHandles`]`>`
//!   im [`OpContext`] registriert — „Host-Freigaben sind in dieser
//!   Oberfläche nicht verfügbar“; oder `revoke` ohne
//!   [`HostLeaseUserControl`] (Modell-Aufruf) — nur der Nutzer beendet eine
//!   Host-Arbeitsphase.
//! - [`OpError::InvalidArguments`]: `reason` fehlt oder ist leer bei
//!   `action = "request"`, oder eine unbekannte Aktion.
//! - [`OpError::Execution`]: die Anfrage wurde vom Nutzer abgelehnt, es kam
//!   keine Antwort (Zeitablauf, fallengelassener Kanal), oder kein
//!   Fragekanal ist angehängt.

use std::sync::Arc;

use harw_macros::operation;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};
use harw_tool_shell::{
    HOST_PERMIT_PROMPT_TIMEOUT, HostLeaseUserControl, HostPermitHandles, HostPermitPrompt,
    HostPermitVariant, SANDBOX_LEASE_WORKER_DEFINITION,
};

/// Meldung für den Fall, dass keine `Arc<HostPermitHandles>` registriert ist.
const NO_HOST_PERMIT_HANDLES: &str = "Host-Freigaben sind in dieser Oberfläche nicht verfügbar";

/// Meldung für jeden Ausgang einer offenen Anfrage, der keine ausdrückliche
/// Zustimmung ist: Ablehnung, Zeitablauf, fallengelassene Antwort oder kein
/// Fragekanal angehängt.
const SANDBOX_LEASE_DENIED_MSG: &str =
    "Sandbox-Lease-Anfrage vom Nutzer abgelehnt oder keine Antwort erhalten";

/// Meldung für einen `revoke`-Versuch ohne [`HostLeaseUserControl`] — also
/// jeden Aufruf, der nicht aus einer vom Nutzer getippten Slash-Eingabe
/// stammt (Model-Tool, Web, Job).
const REVOKE_USER_ONLY_MSG: &str = "Nur der Nutzer kann den Host-Modus beenden \
     (Strg+H oder das getippte /sandbox-lease revoke); das Modell kann eine \
     Sandbox-Lease nicht widerrufen.";

/// Statusanzeige einer aktiven Host-Arbeitsphase (kein Zeitablauf).
const ACTIVE_UNTIL_USER_ENDS: &str = "aktiv (bis Strg+H oder /sandbox-lease revoke)";

/// Eingabe-Argumente der `sandbox-lease`-Operation.
///
/// # Felder
/// - `action` (`Option<String>`): `"request"` | `"status"` | `"revoke"`
///   (`revoke` nur über das vom Nutzer getippte `/sandbox-lease revoke`, siehe
///   Moduldoku).
///   Auf der Command-Fläche löst [`FromRawArgs::from_raw_args`] ein fehlendes
///   Token bereits auf `Some("status")` auf; auf den JSON-Flächen bleibt ein
///   fehlendes Feld `None` und der Operationsrumpf fällt dort auf
///   `"request"` zurück (siehe Moduldoku).
/// - `reason` (`Option<String>`): Pflicht (nicht-leer) bei `action =
///   "request"`; auf der Command-Fläche immer `None` (der Vertrag reserviert
///   `request` bewusst für das Model-Tool, siehe Moduldoku).
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct SandboxLeaseArgs {
    /// `"request"` (Vorgabe für die JSON-Flächen) | `"status"` (Vorgabe für
    /// ein bares `/sandbox-lease`). Beenden kann eine Freigabe nur der Nutzer.
    #[serde(default)]
    pub action: Option<String>,
    /// Freitext-Begründung; Pflicht (nicht-leer) bei `action = "request"`.
    #[serde(default)]
    pub reason: Option<String>,
}

impl FromRawArgs for SandboxLeaseArgs {
    /// Erstes Token ist die Aktion (`status`/`revoke`); ein bares
    /// `/sandbox-lease` löst explizit auf `Some("status")` auf, nicht auf
    /// `None` — siehe Moduldoku „Warum die Vorgabe je Fläche unterschiedlich
    /// ist“. `reason` bleibt auf der Command-Fläche immer `None`.
    ///
    /// # Fehler
    /// Nie — eine unbekannte Aktion wird erst im Op-Rumpf abgewiesen, damit
    /// die Fehlermeldung den vollständigen Aktionssatz nennen kann.
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        let action = tokens
            .first()
            .cloned()
            .unwrap_or_else(|| "status".to_owned());
        Ok(Self {
            action: Some(action),
            reason: None,
        })
    }
}

/// Fordert eine Sandbox-Aufhebung an, zeigt oder widerruft eine bestehende
/// Freigabe.
///
/// # Beschreibung
/// Siehe Moduldoku für den vollständigen Ablauf der drei Unterkommandos.
///
/// # Model-Tool-Hinweis
/// Wann nutzen: eine Aufgabe braucht `cargo`/`rustc` oder eine andere
/// Nutzer-Toolchain (z. B. unter `~/.cargo`, `~/.rustup`), Netzzugriff, oder
/// Dateien außerhalb des Workspace — der normale `shell.exec`-Aufruf läuft
/// sonst in einer hermetischen Sandbox ohne all das. Nicht versuchen,
/// fehlende Toolchains per `apt`/Download in der Sandbox nachzuinstallieren
/// — stattdessen dieses Werkzeug rufen.
///
/// Was passiert: mit `action = "request"` und einem konkreten `reason` zeigt
/// dieses Werkzeug dem Nutzer einen Bestätigungsdialog (die Zustimmung dort
/// *ist* die Freigabe, kein weiterer Schritt nötig). Der Nutzer wählt im
/// Dialog zwischen einer Host-Arbeitsphase, die aktiv bleibt, bis er sie
/// selbst beendet (Strg+H oder `/sandbox-lease revoke`), und einer
/// einmaligen Freigabe für genau den nächsten `shell.exec`-Aufruf. Nach
/// Zustimmung laufen die betroffenen `shell.exec`-Aufrufe dieser harw-Sitzung
/// **inklusive aller Kind-Agenten** (z. B. `uia-shell-worker`,
/// `host-process-worker`) auf dem Host mit der Umgebung des Nutzers (PATH,
/// Toolchains, Netzzugriff) statt in der isolierten Sandbox; das Ergebnis
/// trägt `"executed_on": "host"`. Immer zuerst den Grund nennen (`reason`),
/// bevor dieses Werkzeug aufgerufen wird — der Dialog zeigt ihn dem Nutzer.
/// Eine bereits aktive Freigabe wird ohne erneuten Dialog sofort bestätigt.
///
/// `action = "status"` meldet nur, ob eine Freigabe aktiv ist (rein lesend,
/// kein Dialog). Beenden kann eine Freigabe nur der Nutzer (Strg+H oder das
/// getippte `/sandbox-lease revoke`); ein `revoke` aus dem Model-Tool wird
/// abgewiesen.
#[operation(
    name = "sandbox-lease",
    summary = "Fordert eine Sandbox-Aufhebung für shell.exec dieser harw-Sitzung inkl. aller Kind-Agenten an — nutzen, wenn eine Aufgabe cargo/rustc, Nutzer-Toolchains, Netzzugriff oder Dateien außerhalb des Workspace braucht; Toolchains nie in der Sandbox nachinstallieren. action=\"request\" (mit Grund) öffnet den Bestätigungsdialog: der Nutzer erteilt oder verweigert die Freigabe und wählt Host-Arbeitsphase (aktiv, bis er sie beendet) oder einmalige Freigabe; danach laufen die shell.exec-Aufrufe auf dem Host. action=\"status\" zeigt die Freigabe (rein lesend). Beenden kann sie nur der Nutzer (Strg+H oder getipptes /sandbox-lease revoke), nicht das Modell.",
    domain = "execution",
    permission = "operator",
    command(path = "/sandbox-lease", visibility = "tui_only", busy = "immediate"),
    model_tool(approval = "none")
)]
async fn sandbox_lease(ctx: &OpContext, args: SandboxLeaseArgs) -> Result<OpOutput, OpError> {
    let action = args.action.as_deref().unwrap_or("request");
    match action {
        "request" => handle_request(ctx, args.reason.as_deref()).await,
        "status" => handle_status(ctx),
        "revoke" => handle_revoke(ctx),
        other => Err(OpError::InvalidArguments(format!(
            "Unbekannte /sandbox-lease Aktion: '{other}'. Erlaubt: request, status \
             (revoke nur für den Nutzer über das getippte /sandbox-lease revoke)."
        ))),
    }
}

/// Löst die `Arc<HostPermitHandles>` aus dem [`OpContext`] auf.
///
/// # Fehler
/// [`OpError::NotAvailable`], wenn keine registriert ist.
fn resolve_handles(ctx: &OpContext) -> Result<&Arc<HostPermitHandles>, OpError> {
    ctx.service::<Arc<HostPermitHandles>>()
        .ok_or_else(|| OpError::NotAvailable(NO_HOST_PERMIT_HANDLES.to_owned()))
}

/// Implementiert `action = "status"` — rein lesend.
///
/// # Beschreibung
/// [`harw_sandbox::HostPermitSessionRegistry::is_session_approved`]
/// und [`harw_sandbox::HostPermitSessionRegistry::has_single_use`]
/// berücksichtigen sowohl die prozessweite Freigabe (die `request` unten
/// setzt) als auch eine etwaige sitzungseigene Freigabe der aufrufenden
/// Sitzung — ein direkter Blick auf `has_global_approval`/
/// `has_global_single_use` ist hier nicht nötig. Eine Restlaufzeit gibt es
/// nicht mehr (kein Zeitablauf).
fn handle_status(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let handles = resolve_handles(ctx)?;
    let session_id = ctx.session_id().as_str();
    let text = if handles.registry.is_session_approved(session_id) {
        format!("Sandbox-Lease: {ACTIVE_UNTIL_USER_ENDS}.")
    } else if handles.registry.has_single_use(session_id) {
        "Sandbox-Lease: Einmalfreigabe offen.".to_owned()
    } else {
        "Sandbox-Lease: aus.".to_owned()
    };
    Ok(OpOutput::from(text))
}

/// Implementiert `action = "revoke"` — idempotent, **nur für den Nutzer**.
///
/// # Beschreibung
/// Verlangt [`HostLeaseUserControl`] im [`OpContext`] (nur die Slash-Fläche
/// trägt ihn, siehe Moduldoku); fehlt er, wird nichts verändert und
/// [`OpError::NotAvailable`] mit [`REVOKE_USER_ONLY_MSG`] geliefert. Sonst
/// entfernt sie die prozessweite Sitzungs- und Einmalfreigabe
/// ([`harw_sandbox::HostPermitSessionRegistry::revoke_global_approval`]) —
/// das beendet die Host-Ausführung für die Root-Session **und** jede
/// Kind-Session, die dieselbe Registry teilt — sowie eine etwaige
/// sitzungseigene Freigabe der aufrufenden Sitzung
/// ([`harw_sandbox::HostPermitSessionRegistry::revoke_session_approval`]),
/// und widerruft, bestes Bemühen, bereits ausgestellte Ledger-Permits dieser
/// Sitzung ([`harw_sandbox::ProcessPermitLedger::revoke_session`]). Ein
/// fehlgeschlagener Ledger-Widerruf (vergifteter Mutex) bricht den Aufruf
/// nicht ab — die Registry-Freigabe ist bereits entfernt, das ist die
/// sicherheitsrelevante Wirkung.
fn handle_revoke(ctx: &OpContext) -> Result<OpOutput, OpError> {
    if ctx.service::<HostLeaseUserControl>().is_none() {
        return Err(OpError::NotAvailable(REVOKE_USER_ONLY_MSG.to_owned()));
    }
    let handles = resolve_handles(ctx)?;
    let session_id = ctx.session_id().as_str();
    handles.registry.revoke_global_approval();
    handles.registry.revoke_session_approval(session_id);
    let _ = handles.ledger.revoke_session(session_id);
    Ok(OpOutput::from(
        "Sandbox-Lease widerrufen (für diese harw-Sitzung inkl. aller Kind-Agenten).".to_owned(),
    ))
}

/// Implementiert `action = "request"`.
///
/// # Beschreibung
/// Siehe Moduldoku, Abschnitt „Unterkommandos“, für den vollständigen Ablauf.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: `reason` fehlt oder ist leer.
/// - [`OpError::NotAvailable`]: keine `Arc<HostPermitHandles>` registriert.
/// - [`OpError::Execution`]: kein Fragekanal angehängt, der Kanal ist
///   geschlossen, die Antwort wurde abgelehnt, fallengelassen, oder die
///   Wartezeit lief ab.
async fn handle_request(ctx: &OpContext, reason: Option<&str>) -> Result<OpOutput, OpError> {
    let reason = reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .ok_or_else(|| {
            OpError::InvalidArguments(
                "/sandbox-lease request: 'reason' ist Pflicht und darf nicht leer sein.".to_owned(),
            )
        })?;

    let handles = resolve_handles(ctx)?;
    let session_id = ctx.session_id().as_str().to_owned();

    // Bereits aktive prozessweite Freigabe: sofortiger Erfolg, kein
    // erneuter Prompt (siehe Moduldoku). Geprüft wird ausdrücklich die
    // globale Freigabe, nicht `is_session_approved` — dieser Pfad setzt
    // unten selbst nur noch die globale Freigabe, eine rein sitzungseigene
    // Zustimmung entsteht über `/sandbox-lease` nicht mehr.
    if handles.registry.has_global_approval() {
        return Ok(OpOutput::from(format!(
            "Sandbox-Lease bereits {ACTIVE_UNTIL_USER_ENDS}; shell.exec läuft bereits auf dem \
             Host (für diese harw-Sitzung inkl. aller Kind-Agenten)."
        )));
    }

    let Some(sender) = handles.prompts.as_ref() else {
        return Err(OpError::Execution(SANDBOX_LEASE_DENIED_MSG.to_owned()));
    };

    let workspace = ctx.sandbox().workspace().canonical_root().to_path_buf();
    // Letzte Verwendung von `session_id`: die prozessweite Freigabe unten
    // (`mark_global_approval`/`mark_global_single_use`) braucht sie nicht
    // mehr — nur der Prompt selbst trägt sie noch, zur Anzeige in der
    // lokalen Bestätigungs-UI.
    let (prompt, answer) = HostPermitPrompt::new(
        session_id,
        SANDBOX_LEASE_WORKER_DEFINITION.to_owned(),
        reason.to_owned(),
        workspace,
        HostPermitVariant::SessionLease,
    );

    if sender.send(prompt).is_err() {
        return Err(OpError::Execution(SANDBOX_LEASE_DENIED_MSG.to_owned()));
    }

    let decision = match tokio::time::timeout(HOST_PERMIT_PROMPT_TIMEOUT, answer).await {
        Ok(Ok(decision)) => decision,
        Ok(Err(_)) | Err(_) => None,
    };

    let Some(variant) = decision else {
        return Err(OpError::Execution(SANDBOX_LEASE_DENIED_MSG.to_owned()));
    };

    match variant {
        HostPermitVariant::SessionLease => {
            // Prozessweit statt sitzungseigen (Nutzerwunsch „volle
            // Sandbox-Deaktivierung“, 2026-09-21): gilt damit auch für jede
            // Kind-Session (z. B. `uia-shell-worker`, `host-process-worker`),
            // nicht nur für `session_id` selbst. Kein Zeitablauf: die Phase
            // endet nur durch den Nutzer (Strg+H oder `/sandbox-lease revoke`).
            handles.registry.mark_global_approval();
            Ok(OpOutput::from(format!(
                "Sandbox-Lease erteilt, {ACTIVE_UNTIL_USER_ENDS}; shell.exec läuft jetzt auf \
                 dem Host (für diese harw-Sitzung inkl. aller Kind-Agenten). Beenden kann sie \
                 nur der Nutzer."
            )))
        }
        HostPermitVariant::SingleExecution => {
            // Prozessweit statt sitzungseigen — derselbe Grund wie oben: der
            // nächste `shell.exec`-Aufruf einer beliebigen Session dieses
            // Prozesses verbraucht die Freigabe, nicht nur einer von
            // `session_id`.
            handles.registry.mark_global_single_use();
            Ok(OpOutput::from(
                "Einmalige Host-Ausführung erteilt: der nächste shell.exec-Aufruf dieser \
                 harw-Sitzung inkl. aller Kind-Agenten läuft auf dem Host."
                    .to_owned(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{REVOKE_USER_ONLY_MSG, SandboxLeaseArgs, sandbox_lease};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::{HostPermitSessionRegistry, ProcessPermitLedger};
    use harw_tool_shell::{
        HostLeaseUserControl, HostPermitHandles, HostPermitVariant, host_permit_prompt_channel,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Baut einen `OpContext` mit frischem temporärem Workspace, optional mit
    /// registrierten `Arc<HostPermitHandles>` — **ohne** den Nutzer-Marker
    /// [`HostLeaseUserControl`], also wie auf der Model-Tool-Fläche.
    fn test_context(handles: Option<Arc<HostPermitHandles>>) -> TestResult<(OpContext, PathBuf)> {
        build_context(handles, false)
    }

    /// Wie [`test_context`], aber wie auf der Slash-Fläche: mit
    /// [`HostLeaseUserControl`] (vom Nutzer getipptes `/sandbox-lease`).
    fn user_context(handles: Arc<HostPermitHandles>) -> TestResult<(OpContext, PathBuf)> {
        build_context(Some(handles), true)
    }

    fn build_context(
        handles: Option<Arc<HostPermitHandles>>,
        user_typed: bool,
    ) -> TestResult<(OpContext, PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-sandbox-lease-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("workspace")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("build workspace registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        let mut services = ServiceMap::new();
        if let Some(handles) = handles {
            services.insert(handles);
        }
        if user_typed {
            services.insert(HostLeaseUserControl);
        }
        let ctx = OpContext::new(
            SessionId::new(),
            TurnId::new(),
            SandboxSpec::from_resolved(
                binding,
                PermissionSet::from_policy([Permission::ReadWorkspace]),
            ),
            services,
        );
        Ok((ctx, root))
    }

    /// Frische Handles mit leerem Ledger/Registry, optional mit angehängtem
    /// Fragekanal-Sender.
    fn fresh_handles(
        prompts: Option<harw_tool_shell::HostPermitPromptSender>,
    ) -> Arc<HostPermitHandles> {
        Arc::new(HostPermitHandles {
            ledger: Arc::new(ProcessPermitLedger::default()),
            registry: Arc::new(HostPermitSessionRegistry::default()),
            prompts,
        })
    }

    fn args(action: &str, reason: Option<&str>) -> SandboxLeaseArgs {
        SandboxLeaseArgs {
            action: Some(action.to_owned()),
            reason: reason.map(str::to_owned),
        }
    }

    // ── Argument-Parsing ───────────────────────────────────────────────────

    #[test]
    fn test_sandbox_lease_args_from_raw_args_bare_defaults_to_status() -> TestResult {
        let parsed = SandboxLeaseArgs::from_raw_args(&toks(&[])).map_err(ctx("parse"))?;
        assert_eq!(parsed.action.as_deref(), Some("status"));
        assert_eq!(parsed.reason, None);
        Ok(())
    }

    #[test]
    fn test_sandbox_lease_args_from_raw_args_recognizes_revoke() -> TestResult {
        let parsed = SandboxLeaseArgs::from_raw_args(&toks(&["revoke"])).map_err(ctx("parse"))?;
        assert_eq!(parsed.action.as_deref(), Some("revoke"));
        assert_eq!(parsed.reason, None);
        Ok(())
    }

    #[test]
    fn test_sandbox_lease_args_from_raw_args_recognizes_status() -> TestResult {
        let parsed = SandboxLeaseArgs::from_raw_args(&toks(&["status"])).map_err(ctx("parse"))?;
        assert_eq!(parsed.action.as_deref(), Some("status"));
        Ok(())
    }

    // ── status ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn sandbox_lease_status_without_handles_is_not_available() -> TestResult {
        let (ctx, root) = test_context(None)?;
        let result = sandbox_lease(&ctx, args("status", None)).await;
        std::fs::remove_dir_all(&root).ok();
        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("Host-Freigaben"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn sandbox_lease_status_is_off_by_default() -> TestResult {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)))?;
        let output = sandbox_lease(&ctx, args("status", None))
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        std::fs::remove_dir_all(&root).ok();
        assert!(output.text.contains("aus"), "{}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn sandbox_lease_status_reports_active_session_lease() -> TestResult {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)))?;
        handles
            .registry
            .mark_session_approved(ctx.session_id().as_str());

        let output = sandbox_lease(&ctx, args("status", None))
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        std::fs::remove_dir_all(&root).ok();
        assert!(
            output
                .text
                .contains("aktiv (bis Strg+H oder /sandbox-lease revoke)"),
            "{}",
            output.text
        );
        assert!(
            !output.text.contains("min"),
            "no remaining time any more: {}",
            output.text
        );
        Ok(())
    }

    #[tokio::test]
    async fn sandbox_lease_status_reports_single_use() -> TestResult {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)))?;
        handles
            .registry
            .mark_single_use(ctx.session_id().as_str().to_owned());

        let output = sandbox_lease(&ctx, args("status", None))
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        std::fs::remove_dir_all(&root).ok();
        assert!(output.text.contains("Einmalfreigabe"), "{}", output.text);
        Ok(())
    }

    /// `status` muss auch eine prozessweite Freigabe melden, die eine ganz
    /// andere Session-ID gesetzt hat (z. B. die Root-Session über
    /// `/sandbox-lease request`) — nicht nur eine sitzungseigene.
    #[tokio::test]
    async fn sandbox_lease_status_reports_active_global_lease_for_a_different_session() -> TestResult
    {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)))?;
        handles.registry.mark_global_approval();

        let output = sandbox_lease(&ctx, args("status", None))
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        std::fs::remove_dir_all(&root).ok();
        assert!(
            output.text.contains("aktiv"),
            "a global approval set by a different session must still show as active: {}",
            output.text
        );
        Ok(())
    }

    // ── revoke ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn sandbox_lease_revoke_clears_an_active_lease_and_is_idempotent() -> TestResult {
        let handles = fresh_handles(None);
        let (ctx, root) = user_context(Arc::clone(&handles))?;
        handles
            .registry
            .mark_session_approved(ctx.session_id().as_str());

        let first = sandbox_lease(&ctx, args("revoke", None))
            .await
            .map_err(crate::test_support::ctx("revoke must not fail"))?;
        assert!(first.text.contains("widerrufen"), "{}", first.text);
        assert!(
            !handles
                .registry
                .is_session_approved(ctx.session_id().as_str())
        );

        // Zweiter Aufruf ist ein No-Op statt eines Fehlers.
        let second = sandbox_lease(&ctx, args("revoke", None)).await;
        std::fs::remove_dir_all(&root).ok();
        assert!(second.is_ok());
        Ok(())
    }

    /// Beweist die eigentliche Behebung dieses Auftrags: `revoke` muss auch
    /// die prozessweite Freigabe entfernen, nicht nur die sitzungseigene —
    /// sonst bliebe ein über `/sandbox-lease request` erteiltes globales
    /// Lease für jede Kind-Session aktiv, selbst nach einem Widerruf durch
    /// die Root-Session.
    #[tokio::test]
    async fn sandbox_lease_revoke_clears_the_global_lease_for_every_session() -> TestResult {
        let handles = fresh_handles(None);
        let (ctx, root) = user_context(Arc::clone(&handles))?;
        handles.registry.mark_global_approval();
        assert!(handles.registry.is_session_approved("some-child-session"));

        let output = sandbox_lease(&ctx, args("revoke", None))
            .await
            .map_err(crate::test_support::ctx("revoke must not fail"))?;
        std::fs::remove_dir_all(&root).ok();

        assert!(output.text.contains("widerrufen"), "{}", output.text);
        assert!(
            !handles.registry.is_session_approved("some-child-session"),
            "revoke must clear the global approval for every session id, not just the caller's"
        );
        assert!(!handles.registry.has_global_approval());
        Ok(())
    }

    /// Nutzerentscheidung 2026-09-24: das Modell (Model-Tool-Fläche, kein
    /// [`HostLeaseUserControl`] im Kontext) darf eine Host-Arbeitsphase nicht
    /// beenden — der Aufruf wird abgewiesen und ändert nichts.
    #[tokio::test]
    async fn sandbox_lease_revoke_from_the_model_is_rejected_and_keeps_the_lease() -> TestResult {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)))?;
        handles.registry.mark_global_approval();
        handles
            .registry
            .mark_session_approved(ctx.session_id().as_str());

        let result = sandbox_lease(&ctx, args("revoke", None)).await;
        std::fs::remove_dir_all(&root).ok();
        match result {
            Err(OpError::NotAvailable(message)) => {
                assert_eq!(message, REVOKE_USER_ONLY_MSG);
                assert!(message.contains("Strg+H"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable for a model revoke, got {other:?}"
                )));
            }
        }
        assert!(
            handles.registry.has_global_approval(),
            "a rejected model revoke must leave the global lease active"
        );
        assert!(
            handles
                .registry
                .is_session_approved(ctx.session_id().as_str())
        );
        Ok(())
    }

    /// Das Model-Tool-Summary bietet `revoke` nicht mehr an und nennt keine
    /// Ablaufzeit.
    #[test]
    fn sandbox_lease_summary_offers_no_model_revoke_and_no_expiry() {
        use harw_operations::Operation;
        let summary = super::SandboxLeaseOperation.meta().summary;
        assert!(!summary.contains("action=\"revoke\""), "{summary}");
        assert!(!summary.contains("mehrstündig"), "{summary}");
        assert!(summary.contains("nur der Nutzer"), "{summary}");
    }

    // ── request ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn sandbox_lease_request_requires_a_non_empty_reason() -> TestResult {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)))?;
        let result = sandbox_lease(&ctx, args("request", Some("   "))).await;
        std::fs::remove_dir_all(&root).ok();
        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn sandbox_lease_request_without_handles_is_not_available() -> TestResult {
        let (ctx, root) = test_context(None)?;
        let result = sandbox_lease(&ctx, args("request", Some("brauche cargo"))).await;
        std::fs::remove_dir_all(&root).ok();
        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("Host-Freigaben"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn sandbox_lease_request_reuses_an_already_active_global_lease_without_prompting()
    -> TestResult {
        // Kein Sender angehängt -- ein Prompt-Versuch würde fehlschlagen, also
        // beweist ein Erfolg hier, dass kein Prompt gesendet wurde. Die
        // "bereits aktiv"-Prüfung in `handle_request` fragt ausdrücklich die
        // globale Freigabe ab (siehe Moduldoku) — eine rein sitzungseigene
        // Freigabe (`mark_session_approved`) reicht dafür seit dieser
        // Behebung nicht mehr aus, weil `request` selbst keine mehr setzt.
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)))?;
        handles.registry.mark_global_approval();

        let output = sandbox_lease(&ctx, args("request", Some("brauche cargo")))
            .await
            .map_err(crate::test_support::ctx(
                "an already active global lease must succeed without a prompt",
            ))?;
        std::fs::remove_dir_all(&root).ok();
        assert!(output.text.contains("bereits aktiv"), "{}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn sandbox_lease_request_without_a_prompt_channel_is_denied() -> TestResult {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)))?;
        let result = sandbox_lease(&ctx, args("request", Some("brauche cargo"))).await;
        std::fs::remove_dir_all(&root).ok();
        assert!(matches!(result, Err(OpError::Execution(_))));
        Ok(())
    }

    /// Beweist die eigentliche Behebung dieses Auftrags: eine über
    /// `/sandbox-lease request` erteilte `SessionLease`-Freigabe muss auch
    /// eine völlig andere Session-ID sehen (Kind-Agent), nicht nur die
    /// anfragende Session selbst.
    #[tokio::test]
    async fn sandbox_lease_request_session_lease_answer_marks_global_approval_visible_to_any_session()
    -> TestResult {
        let (sender, mut receiver) = host_permit_prompt_channel();
        let handles = fresh_handles(Some(sender));
        let (ctx, root) = test_context(Some(Arc::clone(&handles)))?;

        let responder = tokio::spawn(async move {
            let prompt = receiver
                .recv()
                .await
                .ok_or(TestError::Missing("prompt must arrive"))?;
            if !prompt.approve(HostPermitVariant::SessionLease) {
                return Err(TestError::Unexpected(
                    "prompt.approve returned false".to_owned(),
                ));
            }
            Ok(())
        });

        let output = sandbox_lease(&ctx, args("request", Some("brauche cargo")))
            .await
            .map_err(crate::test_support::ctx(
                "an approved session lease must succeed",
            ))?;
        responder
            .await
            .map_err(crate::test_support::ctx("responder task must not panic"))??;
        std::fs::remove_dir_all(&root).ok();

        assert!(output.text.contains("erteilt"), "{}", output.text);
        assert!(
            handles
                .registry
                .is_session_approved(ctx.session_id().as_str())
        );
        assert!(
            handles
                .registry
                .is_session_approved("a-completely-different-child-session"),
            "a sandbox-lease grant must cover every session id of this harw process, \
             not only the one that requested it"
        );
        assert!(handles.registry.has_global_approval());
        Ok(())
    }

    /// Wie oben, für `SingleExecution`: die Einmalfreigabe muss den nächsten
    /// `shell.exec`-Aufruf **jeder** Session dieses Prozesses abdecken.
    #[tokio::test]
    async fn sandbox_lease_request_single_execution_answer_marks_global_single_use_visible_to_any_session()
    -> TestResult {
        let (sender, mut receiver) = host_permit_prompt_channel();
        let handles = fresh_handles(Some(sender));
        let (ctx, root) = test_context(Some(Arc::clone(&handles)))?;

        let responder = tokio::spawn(async move {
            let prompt = receiver
                .recv()
                .await
                .ok_or(TestError::Missing("prompt must arrive"))?;
            if !prompt.approve(HostPermitVariant::SingleExecution) {
                return Err(TestError::Unexpected(
                    "prompt.approve returned false".to_owned(),
                ));
            }
            Ok(())
        });

        let output = sandbox_lease(&ctx, args("request", Some("brauche cargo")))
            .await
            .map_err(crate::test_support::ctx(
                "an approved single execution must succeed",
            ))?;
        responder
            .await
            .map_err(crate::test_support::ctx("responder task must not panic"))??;
        std::fs::remove_dir_all(&root).ok();

        assert!(output.text.contains("Einmalige"), "{}", output.text);
        assert!(handles.registry.has_single_use(ctx.session_id().as_str()));
        assert!(
            !handles
                .registry
                .is_session_approved(ctx.session_id().as_str())
        );
        assert!(
            handles.registry.has_global_single_use(),
            "the single-execution grant must be visible as a global single-use approval"
        );
        // Verbraucht durch eine ganz andere Session-ID, wie es ein
        // Kind-Agent tun würde -- beweist, dass die Freigabe prozessweit und
        // nicht sitzungseigen ist.
        assert!(
            handles
                .registry
                .take_single_use("a-completely-different-child-session")
        );
        assert!(!handles.registry.has_global_single_use());
        Ok(())
    }

    #[tokio::test]
    async fn sandbox_lease_request_denied_answer_is_an_error() -> TestResult {
        let (sender, mut receiver) = host_permit_prompt_channel();
        let handles = fresh_handles(Some(sender));
        let (ctx, root) = test_context(Some(Arc::clone(&handles)))?;

        let responder = tokio::spawn(async move {
            let prompt = receiver
                .recv()
                .await
                .ok_or(TestError::Missing("prompt must arrive"))?;
            if !prompt.deny() {
                return Err(TestError::Unexpected(
                    "prompt.deny returned false".to_owned(),
                ));
            }
            Ok(())
        });

        let result = sandbox_lease(&ctx, args("request", Some("brauche cargo"))).await;
        responder
            .await
            .map_err(crate::test_support::ctx("responder task must not panic"))??;
        std::fs::remove_dir_all(&root).ok();

        assert!(matches!(result, Err(OpError::Execution(_))));
        Ok(())
    }

    // ── unbekannte Aktion ──────────────────────────────────────────────────

    #[tokio::test]
    async fn sandbox_lease_unknown_action_is_rejected() -> TestResult {
        let (ctx, root) = test_context(None)?;
        let result = sandbox_lease(&ctx, args("frobnicate", None)).await;
        std::fs::remove_dir_all(&root).ok();
        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("frobnicate"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected InvalidArguments, got {other:?}"
                )));
            }
        }
        Ok(())
    }
}
