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

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_operations::session_control::SharedSessionController;

/// Leerer Argument-Container für die `/status`-Operation.
///
/// # Beschreibung
/// Die Status-Operation benötigt keine Eingabeparameter — alle relevanten Daten
/// werden aus dem [`OpContext`] bezogen. Dieser Typ existiert, weil das
/// `#[operation]`-Makro einen `Deserialize + Default`-Args-Typ erfordert.
///
/// # Spec-Referenz
/// Plan v2 — `/status` Meta-Definition.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
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
    let (provider, model) = match ctx.service::<SharedSessionController>() {
        Some(controller) => {
            let snap = controller.snapshot();
            (snap.active_provider, snap.active_model)
        }
        None => (None, None),
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
    }

    let text = lines.join("\n");

    Ok(OpOutput::from(text))
}

#[cfg(test)]
mod tests {
    use super::{StatusArgs, status};
    use crate::testutil::toks;
    use harw_operations::context::ServiceMap;
    use harw_operations::{FromRawArgs, NullSessionController, OpContext, SessionController};
    use harw_authority::{Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
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
    ) -> (OpContext, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp =
            std::env::temp_dir().join(format!("harw-status-test-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(tmp.join("ws")).unwrap();
        let ws_registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .expect("WorkspaceRegistry::build");
        let binding = ws_registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("resolve binding");
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );

        let mut services = ServiceMap::new();
        let ctrl = NullSessionController::new();
        if let Some(provider) = active_provider {
            ctrl.set_active_provider(provider.to_owned())
                .expect("NullSessionController::set_active_provider");
        }
        services.insert(Arc::new(ctrl) as harw_operations::SharedSessionController);
        if let Some(registry) = registry {
            services.insert(registry);
        }

        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        (ctx, tmp)
    }

    #[tokio::test]
    async fn status_omits_concurrency_line_without_a_load_registry() {
        let (ctx, _tmp) = make_test_ctx(Some("openai"), None);
        let output = status(&ctx, StatusArgs {}).await.expect("status must not fail");
        assert!(
            !output.text.contains("Provider-Concurrency"),
            "no registry registered — must not fabricate a status line: {}",
            output.text
        );
    }

    #[tokio::test]
    async fn status_shows_concurrency_line_when_registry_has_the_active_provider() {
        let mut registry: ProviderLoadRegistry = ProviderLoadRegistry::new();
        registry.insert(
            "openai".to_owned(),
            Arc::new(StubLoadControl(ProviderLoadStatus {
                provider: "openai".to_owned(),
                max_concurrency: Some(4),
                available_permits: 3,
                rate_limit_wait: Some(Duration::from_millis(1500)),
                recent_rate_limited: 2,
            })),
        );
        let (ctx, _tmp) = make_test_ctx(Some("openai"), Some(registry));

        let output = status(&ctx, StatusArgs {}).await.expect("status must not fail");
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
    }

    #[tokio::test]
    async fn status_omits_concurrency_line_when_registry_lacks_the_active_provider() {
        let mut registry: ProviderLoadRegistry = ProviderLoadRegistry::new();
        registry.insert(
            "anthropic".to_owned(),
            Arc::new(StubLoadControl(ProviderLoadStatus {
                provider: "anthropic".to_owned(),
                max_concurrency: None,
                available_permits: usize::MAX,
                rate_limit_wait: None,
                recent_rate_limited: 0,
            })),
        );
        // Active provider is "openai", registry only has "anthropic".
        let (ctx, _tmp) = make_test_ctx(Some("openai"), Some(registry));

        let output = status(&ctx, StatusArgs {}).await.expect("status must not fail");
        assert!(
            !output.text.contains("Provider-Concurrency"),
            "registry has no entry for the active provider: {}",
            output.text
        );
    }
}
