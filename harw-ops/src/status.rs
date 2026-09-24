//! `/status` — Session- und Turn-Status-Operation.
//!
//! # Verantwortungsbereich
//! Implementiert die `status`-Operation, die als `/status`-Command (channel_parity)
//! und als readonly Model-Tool (approval = none) verfügbar ist. Gibt Informationen
//! über die aktuelle Session, den aktuellen Turn und den Sandbox-Kontext zurück.
//!
//! Zeigt zusätzlich (Welle 6b, sofern verdrahtet — siehe
//! `harw_provider_http::ProviderLoadRegistry`) die Nebenläufigkeits-/
//! Rate-Limit-Sichtbarkeit des aktiven Providers: konfigurierte/aktuelle
//! `max_concurrency`, freie Permits und beobachtete HTTP-429-Antworten. Die
//! Zeile fehlt stillschweigend, wenn keine Registry registriert ist oder der
//! aktive Provider keinen Eintrag darin hat — `/status` bleibt ohne diese
//! Verdrahtung voll funktionsfähig. Anpassen lässt sich die Grenze über die
//! separate Operation `/provider-concurrency` (siehe `crate::provider`).
//!
//! Zeigt außerdem (sofern verdrahtet) den Host-Lease-Zustand: „aktiv (bis
//! Strg+H oder /sandbox-lease revoke)“, „Einmalfreigabe“ oder „aus“. Eine
//! Host-Arbeitsphase hat keine Restlaufzeit (Nutzerentscheidung 2026-09-24):
//! sie endet nur durch den Nutzer. Seit der Behebung „volle
//! Sandbox-Deaktivierung“ (2026-09-21) meldet diese Zeile sowohl eine
//! sitzungseigene als auch eine prozessweite Freigabe
//! ([`harw_sandbox::HostPermitSessionRegistry::mark_global_approval`], über
//! `/sandbox-lease` gesetzt) — `is_session_approved`/`has_single_use`
//! berücksichtigen bereits beide, ein separater Blick auf
//! `has_global_approval` ist hier deshalb nicht nötig. Die Zeile fehlt
//! stillschweigend, wenn kein `Arc<harw_tool_shell::HostPermitHandles>` im
//! `OpContext` registriert ist — `/status` bleibt ohne diese Verdrahtung voll
//! funktionsfähig, genau wie bei der Provider-Concurrency-Zeile oben.
//! Freigeben lässt sich der Lease über `/sandbox-lease` (siehe
//! `crate::sandbox_lease`), beenden nur durch den Nutzer (Strg+H oder das
//! getippte `/sandbox-lease revoke`).
//!
//! Zeigt außerdem (Welle 3, sofern verdrahtet) die Kontextfenster-Auslastung
//! aus [`harw_operations::session_control::SessionController::context_usage`]:
//! belegte/verfügbare Tokens, Kompaktierungsschwelle, Ausgabe-Reserve und die
//! letzte Kompaktierung. Die Zeile fehlt stillschweigend, wenn der Controller
//! keine Momentaufnahme führt (Trait-Default `None`).
//!
//! # Schlüsseltypen
//! - [`StatusArgs`] — leerer Argument-Container (keine Parameter erforderlich)
//! - `StatusOperation` — generiert vom `#[operation]`-Makro
//!
//! # Nebenläufigkeit
//! `StatusOperation` ist `Send + Sync` (Unit-Struct ohne inneren Zustand, generiert
//! durch `#[operation]`-Makro).
//!
//! # Fehlertypen
//! Diese Operation erzeugt keine Fehler; `Ok(OpOutput::from(text))` wird immer zurückgegeben.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::status::StatusArgs;
//! // Die Operation wird über den harw-operations-Registry-Mechanismus aufgerufen.
//! ```

use std::sync::Arc;

use harw_macros::operation;
use harw_operations::session_control::{ContextUsageSnapshot, SharedSessionController};
use harw_operations::{OpContext, OpError, OpOutput};
use harw_tool_shell::HostPermitHandles;

/// Formatiert die Kontextfenster-Auslastung als eine deutschsprachige Zeile
/// (Welle 3), geteilt von `/status` und `/usage`.
///
/// # Beschreibung
/// Form: `Kontext: <used>/<window> Tokens (<x>%), nächste Kompaktierung bei
/// <N>, Ausgabe-Reserve <R>, letzte Kompaktierung: <…>`. Unbekannte Werte
/// erscheinen als `n/v`; bei `window_tokens == 0` ist auch der Prozentwert
/// `n/v`. Die letzte Kompaktierung zeigt `vorher→nachher Tokens` samt
/// Auslöser, ob zusammengefasst wurde, und die Zahl ausgelassener
/// Werkzeugergebnisse — oder `keine`.
#[must_use]
pub(crate) fn format_context_line(usage: &ContextUsageSnapshot) -> String {
    fn opt(value: Option<u64>) -> String {
        value.map_or_else(|| "n/v".to_owned(), |v| v.to_string())
    }
    let percent = if usage.window_tokens > 0 {
        format!(
            "{:.1}%",
            (usage.used_tokens as f64 / usage.window_tokens as f64) * 100.0
        )
    } else {
        "n/v".to_owned()
    };
    let last = match &usage.last_compaction {
        None => "keine".to_owned(),
        Some(last) => {
            let mut details = vec![last.reason.clone()];
            if last.summarized {
                details.push("zusammengefasst".to_owned());
            }
            if last.elided_results > 0 {
                details.push(format!("{} Ergebnisse ausgelassen", last.elided_results));
            }
            format!(
                "{}→{} Tokens ({})",
                opt(last.tokens_before),
                opt(last.tokens_after),
                details.join(", ")
            )
        }
    };
    format!(
        "Kontext: {used}/{window} Tokens ({percent}), nächste Kompaktierung bei {threshold}, \
         Ausgabe-Reserve {reserve}, letzte Kompaktierung: {last}",
        used = usage.used_tokens,
        window = usage.window_tokens,
        threshold = opt(usage.threshold_tokens),
        reserve = opt(usage.reserve_tokens),
    )
}

/// Leerer Argument-Container für die `/status`-Operation.
///
/// # Beschreibung
/// Die Status-Operation benötigt keine Eingabeparameter — alle relevanten Daten
/// werden aus dem [`OpContext`] bezogen. Dieser Typ existiert, weil das
/// `#[operation]`-Makro einen `Deserialize + Default`-Args-Typ erfordert.
///
/// # Spec-Referenz
/// Plan v2 — `/status` Meta-Definition.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
pub struct StatusArgs {}

/// Gibt den aktuellen Session- und Turn-Status zurück.
///
/// # Beschreibung
/// Liest Session-ID, Turn-ID und Sandbox-Informationen aus dem [`OpContext`]
/// und formatiert sie als lesbare mehrzeilige Textausgabe. Der Berechtigungs-
/// zähler ergibt sich aus der Anzahl der im [`harw_authority::PermissionSet`]
/// enthaltenen Einträge.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Unveränderlicher Ausführungs-Kontext mit Session-,
///   Turn- und Sandbox-Daten.
/// - `_args` (`StatusArgs`): Leer — keine Parameter.
///
/// # Rückgabe
/// `Ok(OpOutput::from(text))` mit mehrzeiligem Statustext.
///
/// # Fehler
/// Diese Funktion gibt niemals `Err` zurück.
///
/// # Nebenläufigkeit
/// Zustandslos und sicher aus mehreren Threads aufrufbar.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run() — direkte Verwendung nur im Test-Kontext.
/// ```
#[operation(
    name = "status",
    summary = "Zeigt aktuellen Session- und Turn-Status.",
    domain = "session",
    permission = "observer",
    command(path = "/status", visibility = "channel_parity", busy = "immediate"),
    model_tool(readonly, approval = "none"),
    // Web-Fläche übernimmt exakt dieselbe Achse wie das ModelTool oben:
    // `method = "get"`, weil die Operation nur `ctx.session_id()`/`ctx.turn_id()`
    // liest, approval "none", weil ein reiner Statusabruf keine Bestätigung
    // erfordert.
    web(path = "/api/status", method = "get", approval = "none")
)]
async fn status(ctx: &OpContext, _args: StatusArgs) -> Result<OpOutput, OpError> {
    let session_id = ctx.session_id();
    let turn_id = ctx.turn_id();
    let sandbox = ctx.sandbox();

    let workspace_id = sandbox.workspace().workspace();
    let tenant_id = sandbox.workspace().tenant();
    let permission_count = sandbox.permissions().iter().count();

    // Provider und Modell aus dem Session-Controller abfragen, falls in der
    // ServiceMap registriert. `None` bedeutet: kein Controller in dieser
    // Ausfuehrungsumgebung (z. B. Test oder CLI-Echo-Pfad).
    let (provider, model, context_usage) = match ctx.service::<SharedSessionController>() {
        Some(controller) => {
            let snap = controller.snapshot();
            (
                snap.active_provider,
                snap.active_model,
                controller.context_usage(),
            )
        }
        None => (None, None, None),
    };

    let mut lines = vec![
        format!("Session: {session_id}"),
        format!("Turn:    {turn_id}"),
        format!("Sandbox: {workspace_id} ({tenant_id})"),
        format!("Permissions: {permission_count} aktiv"),
    ];

    // Provider- und Modellzeile nur anzeigen, wenn Informationen vorhanden.
    if let Some(p) = &provider {
        lines.push(format!("Provider: {p}"));
    }
    if let Some(m) = &model {
        lines.push(format!("Modell: {m}"));
    }

    // Welle 3 — Kontextfenster-Auslastung, sofern der Controller eine
    // Momentaufnahme führt (`SessionController::context_usage`). Fehlt sie,
    // wird die Zeile stillschweigend weggelassen.
    if let Some(usage) = &context_usage {
        lines.push(format_context_line(usage));
    }

    // W6b — UIA-Sichtbarkeit: Nebenläufigkeits-/Rate-Limit-Zustand des aktiven
    // Providers, sofern eine `ProviderLoadRegistry` in der `ServiceMap`
    // registriert ist (Composition Root) UND der Provider einen Eintrag
    // darin hat (aktuell: OpenAI-kompatible Backends). Fehlt eins von
    // beiden, wird die Zeile stillschweigend weggelassen — `/status` bleibt
    // ohne diese Verdrahtung voll funktionsfähig.
    if let Some(p) = &provider
        && let Some(registry) = ctx.service::<harw_provider_http::ProviderLoadRegistry>()
        && let Some(control) = registry.get(p)
    {
        let load_status = control.provider_status();
        let concurrency = match load_status.max_concurrency {
            Some(n) => n.to_string(),
            None => "unlimited".to_owned(),
        };
        lines.push(format!(
            "Provider-Concurrency: {concurrency} (verfügbar: {available}, 429 beobachtet: {count}x)",
            available = if load_status.available_permits == usize::MAX {
                "n/a".to_owned()
            } else {
                load_status.available_permits.to_string()
            },
            count = load_status.recent_rate_limited,
        ));
        // Client-seitige RPM/TPM-Budgets (je Bucket eine eingerückte Zeile).
        for budget in &load_status.budgets {
            lines.push(format!("  Budget: {budget}"));
        }
    }

    // Welle 2 (Plan Teil B5) — Host-Lease-Zustand, sofern eine
    // `Arc<HostPermitHandles>` in der ServiceMap registriert ist (siehe
    // `crate::sandbox_lease`). Fehlt sie, bleibt `/status` unverändert --
    // dieselbe "fehlt stillschweigend"-Regel wie bei der
    // Provider-Concurrency-Zeile oben. `is_session_approved`/
    // `has_single_use` berücksichtigen seit der "volle Sandbox-Deaktivierung"-
    // Behebung (2026-09-21) bereits sitzungseigene und prozessweite
    // Freigabe -- kein zusätzlicher Blick auf `has_global_approval` nötig.
    // Keine Restlaufzeit: eine Phase endet nur durch den Nutzer
    // (Nutzerentscheidung 2026-09-24).
    if let Some(handles) = ctx.service::<Arc<HostPermitHandles>>() {
        let session_id = ctx.session_id().as_str();
        let label = if handles.registry.is_session_approved(session_id) {
            "aktiv (bis Strg+H oder /sandbox-lease revoke)".to_owned()
        } else if handles.registry.has_single_use(session_id) {
            "Einmalfreigabe".to_owned()
        } else {
            "aus".to_owned()
        };
        lines.push(format!("Host-Lease: {label}"));
    }

    let text = lines.join("\n");

    Ok(OpOutput::from(text))
}

#[cfg(test)]
mod tests {
    use super::{StatusArgs, format_context_line, status};
    use crate::test_support::{TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_operations::context::ServiceMap;
    use harw_operations::session_control::{ContextUsageSnapshot, LastCompaction};
    use harw_operations::{FromRawArgs, NullSessionController, OpContext, SessionController};
    use harw_provider_http::{ProviderLoadControl, ProviderLoadRegistry, ProviderLoadStatus};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn test_status_args_from_raw_args_empty_tokens_returns_ok() {
        let result = StatusArgs::from_raw_args(&toks(&[]));
        assert!(result.is_ok());
    }

    #[test]
    fn test_status_args_from_raw_args_ignores_extra_tokens() {
        let result = StatusArgs::from_raw_args(&toks(&["ignored"]));
        assert!(result.is_ok());
    }

    /// Fixed-value stub — no live provider needed to test the status text
    /// wiring itself (see `harw-provider-http` for the real implementation).
    #[derive(Debug)]
    struct StubLoadControl(ProviderLoadStatus);

    impl ProviderLoadControl for StubLoadControl {
        fn provider_status(&self) -> ProviderLoadStatus {
            self.0.clone()
        }
        fn set_max_concurrency(&self, _target: Option<usize>) -> bool {
            true
        }
    }

    fn make_test_ctx(
        active_provider: Option<&str>,
        registry: Option<ProviderLoadRegistry>,
        host_permit_handles: Option<Arc<harw_tool_shell::HostPermitHandles>>,
    ) -> TestResult<(OpContext, std::path::PathBuf)> {
        make_test_ctx_with_context(active_provider, registry, host_permit_handles, None)
    }

    fn make_test_ctx_with_context(
        active_provider: Option<&str>,
        registry: Option<ProviderLoadRegistry>,
        host_permit_handles: Option<Arc<harw_tool_shell::HostPermitHandles>>,
        context_usage: Option<ContextUsageSnapshot>,
    ) -> TestResult<(OpContext, std::path::PathBuf)> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp =
            std::env::temp_dir().join(format!("harw-status-test-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(tmp.join("ws")).map_err(ctx("create test workspace"))?;
        let ws_registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("WorkspaceRegistry::build"))?;
        let binding = ws_registry
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
        let ctrl = NullSessionController::new();
        if let Some(provider) = active_provider {
            ctrl.set_active_provider(provider.to_owned())
                .map_err(ctx("NullSessionController::set_active_provider"))?;
        }
        ctrl.set_context_usage(context_usage)
            .map_err(ctx("NullSessionController::set_context_usage"))?;
        services.insert(Arc::new(ctrl) as harw_operations::SharedSessionController);
        if let Some(registry) = registry {
            services.insert(registry);
        }
        if let Some(handles) = host_permit_handles {
            services.insert(handles);
        }

        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        Ok((ctx, tmp))
    }

    #[tokio::test]
    async fn status_omits_concurrency_line_without_a_load_registry() -> TestResult {
        let (ctx, _tmp) = make_test_ctx(Some("openai"), None, None)?;
        let output = status(&ctx, StatusArgs {})
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        assert!(
            !output.text.contains("Provider-Concurrency"),
            "no registry registered — must not fabricate a status line: {}",
            output.text
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_shows_concurrency_line_when_registry_has_the_active_provider() -> TestResult {
        let mut registry: ProviderLoadRegistry = ProviderLoadRegistry::new();
        registry.insert(
            "openai".to_owned(),
            Arc::new(StubLoadControl(ProviderLoadStatus {
                provider: "openai".to_owned(),
                max_concurrency: Some(4),
                available_permits: 3,
                rate_limit_wait: Some(Duration::from_millis(1500)),
                recent_rate_limited: 2,
                budgets: Vec::new(),
            })),
        );
        let (ctx, _tmp) = make_test_ctx(Some("openai"), Some(registry), None)?;

        let output = status(&ctx, StatusArgs {})
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        assert!(
            output.text.contains("Provider-Concurrency: 4"),
            "expected max_concurrency in status text: {}",
            output.text
        );
        assert!(
            output.text.contains("verfügbar: 3"),
            "expected available_permits in status text: {}",
            output.text
        );
        assert!(
            output.text.contains("429 beobachtet: 2x"),
            "expected recent_rate_limited in status text: {}",
            output.text
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_omits_concurrency_line_when_registry_lacks_the_active_provider() -> TestResult {
        let mut registry: ProviderLoadRegistry = ProviderLoadRegistry::new();
        registry.insert(
            "anthropic".to_owned(),
            Arc::new(StubLoadControl(ProviderLoadStatus {
                provider: "anthropic".to_owned(),
                max_concurrency: None,
                available_permits: usize::MAX,
                rate_limit_wait: None,
                recent_rate_limited: 0,
                budgets: Vec::new(),
            })),
        );
        // Active provider is "openai", registry only has "anthropic".
        let (ctx, _tmp) = make_test_ctx(Some("openai"), Some(registry), None)?;

        let output = status(&ctx, StatusArgs {})
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        assert!(
            !output.text.contains("Provider-Concurrency"),
            "registry has no entry for the active provider: {}",
            output.text
        );
        Ok(())
    }

    // ── Welle 2 (Plan Teil B5) — Host-Lease-Zeile ─────────────────────────────

    /// Frische Handles mit leerem Ledger/Registry, ohne Fragekanal.
    fn fresh_host_permit_handles() -> Arc<harw_tool_shell::HostPermitHandles> {
        Arc::new(harw_tool_shell::HostPermitHandles {
            ledger: Arc::new(harw_sandbox::ProcessPermitLedger::default()),
            registry: Arc::new(harw_sandbox::HostPermitSessionRegistry::default()),
            prompts: None,
        })
    }

    #[tokio::test]
    async fn status_omits_host_lease_line_without_handles() -> TestResult {
        let (ctx, _tmp) = make_test_ctx(None, None, None)?;
        let output = status(&ctx, StatusArgs {})
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        assert!(
            !output.text.contains("Host-Lease"),
            "no handles registered — must not fabricate a status line: {}",
            output.text
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_shows_host_lease_off_with_handles_but_no_approval() -> TestResult {
        let handles = fresh_host_permit_handles();
        let (ctx, _tmp) = make_test_ctx(None, None, Some(handles))?;
        let output = status(&ctx, StatusArgs {})
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        assert!(
            output.text.contains("Host-Lease: aus"),
            "expected an 'aus' host-lease line: {}",
            output.text
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_shows_host_lease_active_until_the_user_ends_it() -> TestResult {
        let handles = fresh_host_permit_handles();
        let (ctx, _tmp) = make_test_ctx(None, None, Some(Arc::clone(&handles)))?;
        handles
            .registry
            .mark_session_approved(ctx.session_id().as_str());

        let output = status(&ctx, StatusArgs {})
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        assert!(
            output
                .text
                .contains("Host-Lease: aktiv (bis Strg+H oder /sandbox-lease revoke)"),
            "expected an active host-lease line without an expiry: {}",
            output.text
        );
        assert!(
            !output.text.contains("noch "),
            "a host lease has no remaining time any more: {}",
            output.text
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_shows_host_lease_single_use() -> TestResult {
        let handles = fresh_host_permit_handles();
        let (ctx, _tmp) = make_test_ctx(None, None, Some(Arc::clone(&handles)))?;
        handles
            .registry
            .mark_single_use(ctx.session_id().as_str().to_owned());

        let output = status(&ctx, StatusArgs {})
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        assert!(
            output.text.contains("Host-Lease: Einmalfreigabe"),
            "expected a single-use host-lease line: {}",
            output.text
        );
        Ok(())
    }

    /// Welle 2 setzte `mark_session_approved` mit der eigenen Session-ID
    /// voraus; seit der Behebung „volle Sandbox-Deaktivierung“ (2026-09-21,
    /// `/sandbox-lease`) setzt die Registry ihre Freigabe prozessweit über
    /// `mark_global_approval`. `/status` muss diese globale Freigabe genauso
    /// anzeigen wie eine sitzungseigene — auch für eine Session, die selbst
    /// nie über `mark_session_approved` zugestimmt hat.
    #[tokio::test]
    async fn status_shows_host_lease_active_for_a_global_approval_from_a_different_session()
    -> TestResult {
        let handles = fresh_host_permit_handles();
        let (ctx, _tmp) = make_test_ctx(None, None, Some(Arc::clone(&handles)))?;
        handles.registry.mark_global_approval();

        let output = status(&ctx, StatusArgs {})
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        assert!(
            output.text.contains("Host-Lease: aktiv"),
            "a process-wide global approval must show as active even for a session that \
             never approved itself: {}",
            output.text
        );
        Ok(())
    }

    // ── Welle 3 — Kontextzeile ────────────────────────────────────────────────

    fn sample_context_usage() -> ContextUsageSnapshot {
        ContextUsageSnapshot {
            used_tokens: 50_000,
            window_tokens: 200_000,
            estimated_next_tokens: Some(52_000),
            threshold_tokens: Some(144_000),
            reserve_tokens: Some(20_000),
            last_compaction: Some(LastCompaction {
                reason: "auto".to_owned(),
                tokens_before: Some(150_000),
                tokens_after: Some(40_000),
                summarized: true,
                elided_results: 3,
            }),
        }
    }

    #[test]
    fn format_context_line_shows_all_fields() {
        let line = format_context_line(&sample_context_usage());
        assert_eq!(
            line,
            "Kontext: 50000/200000 Tokens (25.0%), nächste Kompaktierung bei 144000, \
             Ausgabe-Reserve 20000, letzte Kompaktierung: 150000→40000 Tokens \
             (auto, zusammengefasst, 3 Ergebnisse ausgelassen)"
        );
    }

    #[test]
    fn format_context_line_marks_unknown_values_and_missing_compaction() {
        let line = format_context_line(&ContextUsageSnapshot::default());
        assert_eq!(
            line,
            "Kontext: 0/0 Tokens (n/v), nächste Kompaktierung bei n/v, \
             Ausgabe-Reserve n/v, letzte Kompaktierung: keine"
        );
    }

    #[tokio::test]
    async fn status_omits_context_line_without_a_context_snapshot() -> TestResult {
        let (ctx, _tmp) = make_test_ctx(None, None, None)?;
        let output = status(&ctx, StatusArgs {})
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        assert!(
            !output.text.contains("Kontext:"),
            "no context snapshot — must not fabricate a context line: {}",
            output.text
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_shows_context_line_from_the_session_controller() -> TestResult {
        let (ctx, _tmp) =
            make_test_ctx_with_context(None, None, None, Some(sample_context_usage()))?;
        let output = status(&ctx, StatusArgs {})
            .await
            .map_err(crate::test_support::ctx("status must not fail"))?;
        assert!(
            output
                .text
                .contains("Kontext: 50000/200000 Tokens (25.0%), nächste Kompaktierung bei 144000"),
            "expected the context line in status text: {}",
            output.text
        );
        Ok(())
    }
}
