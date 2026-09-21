//! `/sandbox-lease` — Modell-getriebene Aufhebung der Sandbox für `shell.exec`.
//!
//! Spec-Quelle: `/home/mia/.claude/plans/recursive-cooking-lobster.md`, Teil
//! B5 ("Neue Operation `sandbox-lease`") und Teil B3
//! (`Arc<harw_tool_shell::HostPermitHandles>`-ServiceMap-Eintrag).
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
//! # Unterkommandos (beide Flächen, gemeinsamer Rumpf)
//! - `request` (Vorgabe auf der Model-Tool-Fläche, siehe
//!   [`SandboxLeaseArgs::from_raw_args`] für den abweichenden Command-Vorgabe-
//!   Wert) — verlangt `reason` (nicht-leer). Ist bereits eine prozessweite
//!   Freigabe aktiv, wird sofort mit der Restlaufzeit geantwortet, **ohne**
//!   erneut zu fragen. Sonst geht ein [`harw_tool_shell::HostPermitPrompt`]
//!   an die anzeigende Oberfläche (Vorauswahl
//!   [`harw_tool_shell::HostPermitVariant::SessionLease`]); die Operation
//!   wartet höchstens [`harw_tool_shell::HOST_PERMIT_PROMPT_TIMEOUT`] lang auf
//!   eine Antwort. Jeder Ausgang außer einer ausdrücklichen Zustimmung —
//!   Ablehnung, Zeitablauf, fallengelassene Antwort, kein Fragekanal
//!   angehängt — ist ein Fehler (fail-closed, dieselbe Sicherheitsregel wie
//!   [`harw_tool_shell::exec`]).
//! - `status` (Vorgabe auf der Command-Fläche für ein bares
//!   `/sandbox-lease`) — meldet „aktiv, noch N min“, „Einmalfreigabe offen“
//!   oder „aus“, rein lesend; berücksichtigt sowohl die prozessweite als auch
//!   eine etwaige sitzungseigene Freigabe der aufrufenden Sitzung.
//! - `revoke` — entfernt die prozessweite Sitzungs- und Einmalfreigabe
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
//!   Oberfläche nicht verfügbar“.
//! - [`OpError::InvalidArguments`]: `reason` fehlt oder ist leer bei
//!   `action = "request"`, oder eine unbekannte Aktion.
//! - [`OpError::Execution`]: die Anfrage wurde vom Nutzer abgelehnt, es kam
//!   keine Antwort (Zeitablauf, fallengelassener Kanal), oder kein
//!   Fragekanal ist angehängt.

use std::sync::Arc;

use harw_macros::operation;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};
use harw_tool_shell::{
    HOST_PERMIT_PROMPT_TIMEOUT, HOST_SESSION_LEASE_TTL, HostPermitHandles, HostPermitPrompt,
    HostPermitVariant, SANDBOX_LEASE_WORKER_DEFINITION,
};

/// Meldung für den Fall, dass keine `Arc<HostPermitHandles>` registriert ist.
const NO_HOST_PERMIT_HANDLES: &str = "Host-Freigaben sind in dieser Oberfläche nicht verfügbar";

/// Meldung für jeden Ausgang einer offenen Anfrage, der keine ausdrückliche
/// Zustimmung ist: Ablehnung, Zeitablauf, fallengelassene Antwort oder kein
/// Fragekanal angehängt.
const SANDBOX_LEASE_DENIED_MSG: &str =
    "Sandbox-Lease-Anfrage vom Nutzer abgelehnt oder keine Antwort erhalten";

/// Eingabe-Argumente der `sandbox-lease`-Operation.
///
/// # Felder
/// - `action` (`Option<String>`): `"request"` | `"status"` | `"revoke"`.
///   Auf der Command-Fläche löst [`FromRawArgs::from_raw_args`] ein fehlendes
///   Token bereits auf `Some("status")` auf; auf den JSON-Flächen bleibt ein
///   fehlendes Feld `None` und der Operationsrumpf fällt dort auf
///   `"request"` zurück (siehe Moduldoku).
/// - `reason` (`Option<String>`): Pflicht (nicht-leer) bei `action =
///   "request"`; auf der Command-Fläche immer `None` (der Vertrag reserviert
///   `request` bewusst für das Model-Tool, siehe Moduldoku).
#[derive(Debug, Default, serde::Deserialize)]
pub struct SandboxLeaseArgs {
    /// `"request"` (Vorgabe für die JSON-Flächen) | `"status"` (Vorgabe für
    /// ein bares `/sandbox-lease`) | `"revoke"`.
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
/// Dialog zwischen einer mehrstündigen Sitzungsfreigabe und einer
/// einmaligen Freigabe für genau den nächsten `shell.exec`-Aufruf. Nach
/// Zustimmung laufen die betroffenen `shell.exec`-Aufrufe dieser harw-Sitzung
/// **inklusive aller Kind-Agenten** (z. B. `uia-shell-worker`,
/// `host-process-worker`) auf dem Host mit der Umgebung des Nutzers (PATH,
/// Toolchains, Netzzugriff) statt in der isolierten Sandbox; das Ergebnis
/// trägt `"executed_on": "host"`. Immer zuerst den Grund nennen (`reason`),
/// bevor dieses Werkzeug aufgerufen wird — der Dialog zeigt ihn dem Nutzer.
/// Eine bereits aktive Freigabe wird ohne erneuten Dialog sofort mit der
/// Restlaufzeit bestätigt.
///
/// `action = "status"` meldet nur, ob/wie lange eine Freigabe aktiv ist
/// (rein lesend, kein Dialog). `action = "revoke"` widerruft eine aktive
/// Freigabe sofort (idempotent).
#[operation(
    name = "sandbox-lease",
    summary = "Fordert eine Sandbox-Aufhebung für shell.exec dieser harw-Sitzung inkl. aller Kind-Agenten an — nutzen, wenn eine Aufgabe cargo/rustc, Nutzer-Toolchains, Netzzugriff oder Dateien außerhalb des Workspace braucht; nicht versuchen, Toolchains in der Sandbox nachzuinstallieren. action=\"request\" (mit Grund) zeigt einen Bestätigungsdialog, in dem der Nutzer die Freigabe erteilt oder ablehnt (Dialog ist die Freigabe) und zwischen einer mehrstündigen Sitzungsfreigabe oder einer einmaligen Freigabe wählt; danach laufen shell.exec-Aufrufe dieser harw-Sitzung inkl. aller Kind-Agenten auf dem Host. action=\"status\" zeigt eine bestehende Freigabe (rein lesend), action=\"revoke\" widerruft sie (idempotent).",
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
            "Unbekannte /sandbox-lease Aktion: '{other}'. Erlaubt: request, status, revoke."
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

/// Rundet die verbleibende Sitzungsfreigabe-Dauer aufwärts auf ganze Minuten.
fn remaining_minutes(remaining: std::time::Duration) -> u64 {
    remaining.as_secs().div_ceil(60)
}

/// Implementiert `action = "status"` — rein lesend.
///
/// # Beschreibung
/// [`harw_sandbox::HostPermitSessionRegistry::session_approval_remaining`]
/// und [`harw_sandbox::HostPermitSessionRegistry::has_single_use`]
/// berücksichtigen sowohl die prozessweite Freigabe (die `request` unten
/// setzt) als auch eine etwaige sitzungseigene Freigabe der aufrufenden
/// Sitzung — ein direkter Blick auf `global_approval_remaining`/
/// `has_global_single_use` ist hier nicht nötig, weil die aufrufende
/// Methode bereits das Maximum aus beiden liefert.
fn handle_status(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let handles = resolve_handles(ctx)?;
    let session_id = ctx.session_id().as_str();
    let text = if let Some(remaining) = handles.registry.session_approval_remaining(session_id) {
        format!(
            "Sandbox-Lease: aktiv, noch {} min.",
            remaining_minutes(remaining)
        )
    } else if handles.registry.has_single_use(session_id) {
        "Sandbox-Lease: Einmalfreigabe offen.".to_owned()
    } else {
        "Sandbox-Lease: aus.".to_owned()
    };
    Ok(OpOutput::from(text))
}

/// Implementiert `action = "revoke"` — idempotent.
///
/// # Beschreibung
/// Entfernt die prozessweite Sitzungs- und Einmalfreigabe
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

    // Bereits aktive prozessweite Freigabe: sofortiger Erfolg mit
    // Restdauer, kein erneuter Prompt (siehe Moduldoku). Geprüft wird
    // ausdrücklich die globale Freigabe, nicht `session_approval_remaining`
    // — dieser Pfad setzt unten selbst nur noch die globale Freigabe, eine
    // rein sitzungseigene Zustimmung entsteht über `/sandbox-lease` nicht
    // mehr.
    if let Some(remaining) = handles.registry.global_approval_remaining() {
        return Ok(OpOutput::from(format!(
            "Sandbox-Lease bereits aktiv, noch {} min; shell.exec läuft bereits auf dem Host \
             (für diese harw-Sitzung inkl. aller Kind-Agenten).",
            remaining_minutes(remaining)
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
            // nicht nur für `session_id` selbst.
            handles.registry.mark_global_approval(HOST_SESSION_LEASE_TTL);
            Ok(OpOutput::from(format!(
                "Sandbox-Lease erteilt für {} Stunden; shell.exec läuft jetzt auf dem Host \
                 (für diese harw-Sitzung inkl. aller Kind-Agenten).",
                HOST_SESSION_LEASE_TTL.as_secs() / 3600
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
    use super::{SandboxLeaseArgs, sandbox_lease};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::{HostPermitSessionRegistry, ProcessPermitLedger};
    use harw_tool_shell::{HostPermitHandles, HostPermitVariant, host_permit_prompt_channel};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// Baut einen `OpContext` mit frischem temporärem Workspace, optional mit
    /// registrierten `Arc<HostPermitHandles>`.
    fn test_context(handles: Option<Arc<HostPermitHandles>>) -> (OpContext, PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join(format!("harw-sandbox-lease-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("workspace")).expect("create test workspace");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .expect("build workspace registry");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .expect("resolve workspace binding");
        let mut services = ServiceMap::new();
        if let Some(handles) = handles {
            services.insert(handles);
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
        (ctx, root)
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
    fn test_sandbox_lease_args_from_raw_args_bare_defaults_to_status() {
        let parsed = SandboxLeaseArgs::from_raw_args(&toks(&[])).expect("parse");
        assert_eq!(parsed.action.as_deref(), Some("status"));
        assert_eq!(parsed.reason, None);
    }

    #[test]
    fn test_sandbox_lease_args_from_raw_args_recognizes_revoke() {
        let parsed = SandboxLeaseArgs::from_raw_args(&toks(&["revoke"])).expect("parse");
        assert_eq!(parsed.action.as_deref(), Some("revoke"));
        assert_eq!(parsed.reason, None);
    }

    #[test]
    fn test_sandbox_lease_args_from_raw_args_recognizes_status() {
        let parsed = SandboxLeaseArgs::from_raw_args(&toks(&["status"])).expect("parse");
        assert_eq!(parsed.action.as_deref(), Some("status"));
    }

    // ── status ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn sandbox_lease_status_without_handles_is_not_available() {
        let (ctx, root) = test_context(None);
        let result = sandbox_lease(&ctx, args("status", None)).await;
        std::fs::remove_dir_all(&root).ok();
        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("Host-Freigaben"), "{message}");
            }
            other => panic!("expected NotAvailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sandbox_lease_status_is_off_by_default() {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)));
        let output = sandbox_lease(&ctx, args("status", None))
            .await
            .expect("status must not fail");
        std::fs::remove_dir_all(&root).ok();
        assert!(output.text.contains("aus"), "{}", output.text);
    }

    #[tokio::test]
    async fn sandbox_lease_status_reports_active_session_lease() {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)));
        handles
            .registry
            .mark_session_approved(ctx.session_id().as_str(), Duration::from_secs(600));

        let output = sandbox_lease(&ctx, args("status", None))
            .await
            .expect("status must not fail");
        std::fs::remove_dir_all(&root).ok();
        assert!(output.text.contains("aktiv"), "{}", output.text);
    }

    #[tokio::test]
    async fn sandbox_lease_status_reports_single_use() {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)));
        handles
            .registry
            .mark_single_use(ctx.session_id().as_str().to_owned());

        let output = sandbox_lease(&ctx, args("status", None))
            .await
            .expect("status must not fail");
        std::fs::remove_dir_all(&root).ok();
        assert!(output.text.contains("Einmalfreigabe"), "{}", output.text);
    }

    /// `status` muss auch eine prozessweite Freigabe melden, die eine ganz
    /// andere Session-ID gesetzt hat (z. B. die Root-Session über
    /// `/sandbox-lease request`) — nicht nur eine sitzungseigene.
    #[tokio::test]
    async fn sandbox_lease_status_reports_active_global_lease_for_a_different_session() {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)));
        handles.registry.mark_global_approval(Duration::from_secs(600));

        let output = sandbox_lease(&ctx, args("status", None))
            .await
            .expect("status must not fail");
        std::fs::remove_dir_all(&root).ok();
        assert!(
            output.text.contains("aktiv"),
            "a global approval set by a different session must still show as active: {}",
            output.text
        );
    }

    // ── revoke ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn sandbox_lease_revoke_clears_an_active_lease_and_is_idempotent() {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)));
        handles
            .registry
            .mark_session_approved(ctx.session_id().as_str(), Duration::from_secs(600));

        let first = sandbox_lease(&ctx, args("revoke", None))
            .await
            .expect("revoke must not fail");
        assert!(first.text.contains("widerrufen"), "{}", first.text);
        assert!(!handles.registry.is_session_approved(ctx.session_id().as_str()));

        // Zweiter Aufruf ist ein No-Op statt eines Fehlers.
        let second = sandbox_lease(&ctx, args("revoke", None)).await;
        std::fs::remove_dir_all(&root).ok();
        assert!(second.is_ok());
    }

    /// Beweist die eigentliche Behebung dieses Auftrags: `revoke` muss auch
    /// die prozessweite Freigabe entfernen, nicht nur die sitzungseigene —
    /// sonst bliebe ein über `/sandbox-lease request` erteiltes globales
    /// Lease für jede Kind-Session aktiv, selbst nach einem Widerruf durch
    /// die Root-Session.
    #[tokio::test]
    async fn sandbox_lease_revoke_clears_the_global_lease_for_every_session() {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)));
        handles.registry.mark_global_approval(Duration::from_secs(600));
        assert!(handles.registry.is_session_approved("some-child-session"));

        let output = sandbox_lease(&ctx, args("revoke", None))
            .await
            .expect("revoke must not fail");
        std::fs::remove_dir_all(&root).ok();

        assert!(output.text.contains("widerrufen"), "{}", output.text);
        assert!(
            !handles.registry.is_session_approved("some-child-session"),
            "revoke must clear the global approval for every session id, not just the caller's"
        );
        assert_eq!(handles.registry.global_approval_remaining(), None);
    }

    // ── request ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn sandbox_lease_request_requires_a_non_empty_reason() {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)));
        let result = sandbox_lease(&ctx, args("request", Some("   "))).await;
        std::fs::remove_dir_all(&root).ok();
        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
    }

    #[tokio::test]
    async fn sandbox_lease_request_without_handles_is_not_available() {
        let (ctx, root) = test_context(None);
        let result = sandbox_lease(&ctx, args("request", Some("brauche cargo"))).await;
        std::fs::remove_dir_all(&root).ok();
        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("Host-Freigaben"), "{message}");
            }
            other => panic!("expected NotAvailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sandbox_lease_request_reuses_an_already_active_global_lease_without_prompting() {
        // Kein Sender angehängt -- ein Prompt-Versuch würde fehlschlagen, also
        // beweist ein Erfolg hier, dass kein Prompt gesendet wurde. Die
        // "bereits aktiv"-Prüfung in `handle_request` fragt ausdrücklich die
        // globale Freigabe ab (siehe Moduldoku) — eine rein sitzungseigene
        // Freigabe (`mark_session_approved`) reicht dafür seit dieser
        // Behebung nicht mehr aus, weil `request` selbst keine mehr setzt.
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)));
        handles.registry.mark_global_approval(Duration::from_secs(600));

        let output = sandbox_lease(&ctx, args("request", Some("brauche cargo")))
            .await
            .expect("an already active global lease must succeed without a prompt");
        std::fs::remove_dir_all(&root).ok();
        assert!(output.text.contains("bereits aktiv"), "{}", output.text);
    }

    #[tokio::test]
    async fn sandbox_lease_request_without_a_prompt_channel_is_denied() {
        let handles = fresh_handles(None);
        let (ctx, root) = test_context(Some(Arc::clone(&handles)));
        let result = sandbox_lease(&ctx, args("request", Some("brauche cargo"))).await;
        std::fs::remove_dir_all(&root).ok();
        assert!(matches!(result, Err(OpError::Execution(_))));
    }

    /// Beweist die eigentliche Behebung dieses Auftrags: eine über
    /// `/sandbox-lease request` erteilte `SessionLease`-Freigabe muss auch
    /// eine völlig andere Session-ID sehen (Kind-Agent), nicht nur die
    /// anfragende Session selbst.
    #[tokio::test]
    async fn sandbox_lease_request_session_lease_answer_marks_global_approval_visible_to_any_session()
     {
        let (sender, mut receiver) = host_permit_prompt_channel();
        let handles = fresh_handles(Some(sender));
        let (ctx, root) = test_context(Some(Arc::clone(&handles)));

        let responder = tokio::spawn(async move {
            let prompt = receiver.recv().await.expect("prompt must arrive");
            assert!(prompt.approve(HostPermitVariant::SessionLease));
        });

        let output = sandbox_lease(&ctx, args("request", Some("brauche cargo")))
            .await
            .expect("an approved session lease must succeed");
        responder.await.expect("responder task must not panic");
        std::fs::remove_dir_all(&root).ok();

        assert!(output.text.contains("erteilt"), "{}", output.text);
        assert!(handles.registry.is_session_approved(ctx.session_id().as_str()));
        assert!(
            handles.registry.is_session_approved("a-completely-different-child-session"),
            "a sandbox-lease grant must cover every session id of this harw process, \
             not only the one that requested it"
        );
        assert!(handles.registry.global_approval_remaining().is_some());
    }

    /// Wie oben, für `SingleExecution`: die Einmalfreigabe muss den nächsten
    /// `shell.exec`-Aufruf **jeder** Session dieses Prozesses abdecken.
    #[tokio::test]
    async fn sandbox_lease_request_single_execution_answer_marks_global_single_use_visible_to_any_session()
     {
        let (sender, mut receiver) = host_permit_prompt_channel();
        let handles = fresh_handles(Some(sender));
        let (ctx, root) = test_context(Some(Arc::clone(&handles)));

        let responder = tokio::spawn(async move {
            let prompt = receiver.recv().await.expect("prompt must arrive");
            assert!(prompt.approve(HostPermitVariant::SingleExecution));
        });

        let output = sandbox_lease(&ctx, args("request", Some("brauche cargo")))
            .await
            .expect("an approved single execution must succeed");
        responder.await.expect("responder task must not panic");
        std::fs::remove_dir_all(&root).ok();

        assert!(output.text.contains("Einmalige"), "{}", output.text);
        assert!(handles.registry.has_single_use(ctx.session_id().as_str()));
        assert!(!handles.registry.is_session_approved(ctx.session_id().as_str()));
        assert!(
            handles.registry.has_global_single_use(),
            "the single-execution grant must be visible as a global single-use approval"
        );
        // Verbraucht durch eine ganz andere Session-ID, wie es ein
        // Kind-Agent tun würde -- beweist, dass die Freigabe prozessweit und
        // nicht sitzungseigen ist.
        assert!(handles.registry.take_single_use("a-completely-different-child-session"));
        assert!(!handles.registry.has_global_single_use());
    }

    #[tokio::test]
    async fn sandbox_lease_request_denied_answer_is_an_error() {
        let (sender, mut receiver) = host_permit_prompt_channel();
        let handles = fresh_handles(Some(sender));
        let (ctx, root) = test_context(Some(Arc::clone(&handles)));

        let responder = tokio::spawn(async move {
            let prompt = receiver.recv().await.expect("prompt must arrive");
            assert!(prompt.deny());
        });

        let result = sandbox_lease(&ctx, args("request", Some("brauche cargo"))).await;
        responder.await.expect("responder task must not panic");
        std::fs::remove_dir_all(&root).ok();

        assert!(matches!(result, Err(OpError::Execution(_))));
    }

    // ── unbekannte Aktion ──────────────────────────────────────────────────

    #[tokio::test]
    async fn sandbox_lease_unknown_action_is_rejected() {
        let (ctx, root) = test_context(None);
        let result = sandbox_lease(&ctx, args("frobnicate", None)).await;
        std::fs::remove_dir_all(&root).ok();
        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("frobnicate"), "{message}");
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }
}
