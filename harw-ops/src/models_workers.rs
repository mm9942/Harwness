//! `/models worker` — eigene Modellwahl je UIA-Worker-Rolle (Runde 5, Teil G).
//!
//! # Grammatik
//! | Eingabe | Wirkung |
//! |---|---|
//! | `/models worker` / `/models worker show` | Wahl je Rolle („wie UIA“ oder Provider/Modell) plus UIA-Auswahl |
//! | `/models worker <rolle> uia` | Rolle folgt der UIA („wie UIA“) |
//! | `/models worker <rolle> <modell\|provider/modell>` | feste Wahl, beliebiger aktivierter Provider |
//! | `/models worker all uia` | alle Rollen „wie UIA“, alter `uia_worker_model`-Pin entfernt |
//!
//! Gespeichert wird über den [`crate::config_util::SelectionPersistence`]-Dienst
//! unter `[uia_worker_models]` der Profil-`config.toml`. Die TUI übernimmt
//! eine Wahl aus der Worker-Ansicht zusätzlich sofort für neu gestartete
//! Worker; getippt wirkt sie ab der nächsten Sitzung.
//!
//! # Datenvertrag (`OpOutput::data`)
//! `show` → `{"workers":[{"role","choice","follows_uia","provider","model","source","notice"}],"uia":{"provider","model"}}`;
//! setzen → `{"action":"worker","roles":[…],"choice","follows_uia","persisted","note"}`.

use harw_config::{
    ResolvedConfig, UIA_WORKER_ROLES, UiaWorkerModelChoice, provider_is_logged_in,
    resolve_uia_worker_models,
};
use harw_operations::{OpContext, OpError, OpOutput, SharedSessionController};
use serde_json::{Value, json};

/// Normalisiert einen Rollennamen (`UIA_Writer` → `uia-writer`) und prüft
/// ihn gegen [`UIA_WORKER_ROLES`].
fn parse_worker_role(raw: &str) -> Option<&'static str> {
    let normalized = raw.trim().to_ascii_lowercase().replace('_', "-");
    UIA_WORKER_ROLES
        .into_iter()
        .find(|role| *role == normalized)
}

/// Bekannte Rollen für Fehlermeldungen.
fn known_worker_roles() -> String {
    UIA_WORKER_ROLES.join(", ")
}

/// Baut die Anzeige (`show`).
fn render_workers(ctx: &OpContext, config: &ResolvedConfig) -> OpOutput {
    let controller = ctx.service::<SharedSessionController>();
    let selection = crate::model::effective_uia_selection(controller, config);
    let uia_provider = crate::models::effective_uia_provider(controller, config);
    let uia_model = selection
        .model()
        .map(str::to_owned)
        .or_else(|| config.harness.default_model.clone());

    let mut lines = vec![
        format!(
            "UIA: {} / {}",
            uia_provider.as_deref().unwrap_or("-"),
            uia_model.as_deref().unwrap_or("-")
        ),
        String::new(),
    ];
    let mut workers = Vec::new();
    for resolved in resolve_uia_worker_models(config) {
        let (provider, model) = match &resolved.choice {
            UiaWorkerModelChoice::Fixed { provider, model } => {
                (Some(provider.clone()), Some(model.clone()))
            }
            UiaWorkerModelChoice::FollowUia => (uia_provider.clone(), uia_model.clone()),
        };
        let mut line = format!(
            "{:<18} {:<32} ({})",
            resolved.role,
            resolved.choice.label(),
            resolved.source.label()
        );
        if let Some(notice) = &resolved.notice {
            line.push_str(" — ");
            line.push_str(notice);
        }
        lines.push(line);
        workers.push(json!({
            "role": resolved.role,
            "choice": resolved.choice.to_config_value(),
            "follows_uia": resolved.choice.follows_uia(),
            "provider": provider,
            "model": model,
            "source": resolved.source.label(),
            "notice": resolved.notice,
        }));
    }
    lines.push(String::new());
    lines.push(
        "Ändern: /models worker <rolle|all> <uia|modell|provider/modell>. Laufende Worker \
         behalten ihr Modell; neue Worker nehmen die neue Wahl."
            .to_owned(),
    );
    OpOutput {
        text: lines.join("\n"),
        data: Some(json!({
            "workers": workers,
            "uia": { "provider": uia_provider, "model": uia_model },
        })),
    }
}

/// Führt `/models worker …` aus (siehe Moduldoku).
///
/// # Fehler
/// [`OpError::InvalidArguments`] für eine unbekannte Rolle, ein fehlendes
/// oder unauflösbares Ziel.
pub(crate) fn handle_worker(
    ctx: &OpContext,
    config: &ResolvedConfig,
    role: Option<&str>,
    target: Option<&str>,
) -> Result<OpOutput, OpError> {
    let role = role.map(str::trim).filter(|role| !role.is_empty());
    let target = target.map(str::trim).filter(|target| !target.is_empty());
    let role = match (role, target) {
        (None | Some("show" | "list"), None) => return Ok(render_workers(ctx, config)),
        (Some(role), Some(_)) => role,
        (Some(_), None) => {
            return Err(OpError::InvalidArguments(
                "Ziel fehlt. Aufruf: /models worker <rolle|all> <uia|modell|provider/modell>."
                    .to_owned(),
            ));
        }
        (None, Some(_)) => {
            return Err(OpError::InvalidArguments(
                "Rolle fehlt. Aufruf: /models worker <rolle|all> <uia|modell|provider/modell>."
                    .to_owned(),
            ));
        }
    };
    let target = target.unwrap_or_default();

    let roles: Vec<&'static str> = if matches!(role.to_ascii_lowercase().as_str(), "all" | "alle") {
        UIA_WORKER_ROLES.to_vec()
    } else {
        vec![parse_worker_role(role).ok_or_else(|| {
            OpError::InvalidArguments(format!(
                "Unbekannte UIA-Worker-Rolle: '{role}'. Bekannte Rollen: {}, all.",
                known_worker_roles()
            ))
        })?]
    };

    let choice = match UiaWorkerModelChoice::parse(target) {
        Some(UiaWorkerModelChoice::FollowUia) => UiaWorkerModelChoice::FollowUia,
        _ => {
            let (provider, model) = crate::models::resolve_target(config, target)?;
            UiaWorkerModelChoice::Fixed { provider, model }
        }
    };
    let value = choice.to_config_value();

    let persistence = crate::config_util::selection_persistence(ctx);
    let mut notes: Vec<String> = roles
        .iter()
        .filter_map(|role| persistence.persist_uia_worker_role_model(role, Some(&value)))
        .collect();
    if roles.len() == UIA_WORKER_ROLES.len() && choice.follows_uia() {
        // „alle wie UIA“ entfernt auch den alten Familien-Pin.
        notes.extend(persistence.persist_uia_worker_model(None));
    }
    let note = (!notes.is_empty()).then(|| notes.join("\n"));

    let mut text = crate::models::finish_text(
        format!(
            "UIA-Worker-Modell für {}: {}",
            if roles.len() == 1 {
                roles.first().copied().unwrap_or_default().to_owned()
            } else {
                "alle Rollen".to_owned()
            },
            choice.label()
        ),
        note.as_deref(),
    );
    if let UiaWorkerModelChoice::Fixed { provider, .. } = &choice
        && !provider_is_logged_in(config, provider)
    {
        text.push_str(&format!(
            "\nHinweis: Provider „{provider}“ ist nicht angemeldet — bis zur Anmeldung folgt \
             die Rolle der UIA."
        ));
    }
    Ok(OpOutput {
        text,
        data: Some(json!({
            "action": "worker",
            "roles": roles,
            "choice": value,
            "follows_uia": choice.follows_uia(),
            "persisted": note.is_none(),
            "note": note.map_or(Value::Null, Value::String),
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_worker_role;
    use crate::config_util::{
        RecordedSelectionPersistCall, RecordingSelectionPersistence, SelectionPersistence,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_config::ResolvedConfig;
    use harw_operations::context::ServiceMap;
    use harw_operations::{OpContext, OpError, OpOutput};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn provider(name: &str) -> TestResult<harw_config::ProviderToml> {
        toml::from_str(&format!(
            "name = \"{name}\"\napi = \"openai-chat\"\nbase_url = \"https://api.example.test/{name}\"\nauth = \"env:TEST_KEY\"\n"
        ))
        .map_err(ctx("provider toml"))
    }

    fn config() -> TestResult<ResolvedConfig> {
        let mut config = ResolvedConfig::default();
        for name in ["anthropic", "openai"] {
            config.providers.insert(name.to_owned(), provider(name)?);
        }
        config.models.insert(
            "gpt-5".to_owned(),
            toml::from_str("id = \"gpt-5\"\nprovider = \"openai\"\n").map_err(ctx("model"))?,
        );
        config.harness.uia_provider = Some("anthropic".to_owned());
        config.harness.uia_model = Some("claude-opus-5-5".to_owned());
        Ok(config)
    }

    struct Fixture {
        ctx: OpContext,
        config: ResolvedConfig,
        recorder: Arc<RecordingSelectionPersistence>,
        root: std::path::PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    fn fixture(config: ResolvedConfig) -> TestResult<Fixture> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-models-worker-test-{}-{id}",
            std::process::id()
        ));
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
        let recorder = Arc::new(RecordingSelectionPersistence::new());
        let persistence: Arc<dyn SelectionPersistence> = recorder.clone();
        let mut services = ServiceMap::new();
        services.insert(persistence);
        services.insert(Arc::new(config.clone()));
        Ok(Fixture {
            ctx: OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            config,
            recorder,
            root,
        })
    }

    /// Führt `tokens` (ohne das führende `worker`) wie `/models worker …` aus.
    async fn run(fixture: &Fixture, tokens: &[&str]) -> Result<OpOutput, OpError> {
        let rest = tokens.strip_prefix(&["worker"]).unwrap_or(tokens);
        super::handle_worker(
            &fixture.ctx,
            &fixture.config,
            rest.first().copied(),
            rest.get(1).copied(),
        )
    }

    #[test]
    fn worker_roles_parse_case_and_underscore_insensitively() {
        assert_eq!(parse_worker_role("UIA_Writer"), Some("uia-writer"));
        assert_eq!(
            parse_worker_role("uia-latex-writer"),
            Some("uia-latex-writer")
        );
        assert_eq!(parse_worker_role("host-process-worker"), None);
    }

    /// Eine Worker-Wahl bei einem anderen Provider als der UIA wird
    /// gespeichert (Kopplung aufgehoben).
    #[tokio::test]
    async fn worker_choice_on_another_provider_is_persisted() -> TestResult {
        let fixture = fixture(config()?)?;
        let output = run(&fixture, &["worker", "uia-writer", "gpt-5"])
            .await
            .map_err(ctx("worker set"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![RecordedSelectionPersistCall::UiaWorkerRoleModel {
                role: "uia-writer".to_owned(),
                value: Some("openai/gpt-5".to_owned()),
            }]
        );
        let data = output.data.ok_or(TestError::Missing("data"))?;
        assert_eq!(data["follows_uia"], serde_json::json!(false));
        Ok(())
    }

    /// `all uia` setzt jede Rolle auf „wie UIA“ und entfernt den alten Pin.
    #[tokio::test]
    async fn worker_all_uia_resets_every_role_and_the_legacy_pin() -> TestResult {
        let fixture = fixture(config()?)?;
        run(&fixture, &["worker", "all", "uia"])
            .await
            .map_err(ctx("worker all uia"))?;
        let calls = fixture.recorder.calls();
        assert_eq!(calls.len(), harw_config::UIA_WORKER_ROLES.len() + 1);
        for role in harw_config::UIA_WORKER_ROLES {
            assert!(
                calls.contains(&RecordedSelectionPersistCall::UiaWorkerRoleModel {
                    role: role.to_owned(),
                    value: Some("uia".to_owned()),
                }),
                "{role}: {calls:?}"
            );
        }
        assert_eq!(
            calls.last(),
            Some(&RecordedSelectionPersistCall::UiaWorkerModel { model: None })
        );
        Ok(())
    }

    /// `show` listet jede Rolle mit ihrer Wahl; „wie UIA“ zeigt das
    /// UIA-Modell.
    #[tokio::test]
    async fn worker_show_lists_every_role_with_its_choice() -> TestResult {
        let mut config = config()?;
        config
            .harness
            .uia_worker_models
            .set("uia-explorer", Some("openai/gpt-5".to_owned()));
        let fixture = fixture(config)?;
        let output = run(&fixture, &["worker"]).await.map_err(ctx("show"))?;
        let data = output.data.ok_or(TestError::Missing("data"))?;
        let workers = data["workers"]
            .as_array()
            .ok_or(TestError::Missing("workers"))?;
        assert_eq!(workers.len(), harw_config::UIA_WORKER_ROLES.len());
        for worker in workers {
            if worker["role"] == serde_json::json!("uia-explorer") {
                assert_eq!(worker["choice"], serde_json::json!("openai/gpt-5"));
                assert_eq!(worker["follows_uia"], serde_json::json!(false));
            } else {
                assert_eq!(worker["choice"], serde_json::json!("uia"));
                assert_eq!(worker["model"], serde_json::json!("claude-opus-5-5"));
            }
        }
        assert!(fixture.recorder.calls().is_empty(), "show schreibt nichts");
        Ok(())
    }

    #[tokio::test]
    async fn worker_rejects_unknown_role_and_missing_target() -> TestResult {
        let fixture = fixture(config()?)?;
        for (tokens, needle) in [
            (
                vec!["worker", "host-process-worker", "uia"],
                "Unbekannte UIA-Worker-Rolle",
            ),
            (vec!["worker", "uia-writer"], "Ziel fehlt"),
            (
                vec!["worker", "uia-writer", "gibtsnicht"],
                "Unbekanntes Modell",
            ),
        ] {
            match run(&fixture, &tokens).await {
                Err(OpError::InvalidArguments(message)) => {
                    assert!(message.contains(needle), "{tokens:?}: {message}");
                }
                other => {
                    return Err(TestError::Unexpected(format!("{tokens:?}: {other:?}")));
                }
            }
        }
        assert!(fixture.recorder.calls().is_empty());
        Ok(())
    }
}
