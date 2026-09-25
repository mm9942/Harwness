//! `/models` — Modellübersicht und -auswahl je Rolle (UIA, Worker, Orchestrator, interne Stellen).
//!
//! # Verantwortungsbereich
//! Bündelt die bislang über mehrere Befehle verstreute Modellwahl
//! (`/uia-model`, `/uia-worker-model`, `[internal_models.*]`) in **einer**
//! Operation mit einheitlicher Grammatik:
//!
//! | Eingabe | Wirkung |
//! |---|---|
//! | `/models` / `/models show` | Tabelle aller Rollen (Provider, Modell, Quelle, Effort) plus Live-Modell der Sitzung |
//! | `/models set <rolle> <ziel>` | Persistiert das Modell der Rolle in der Profil-`config.toml` |
//! | `/models reset <rolle>` | Entfernt die explizite Wahl der Rolle (zurück auf ihren Standard) |
//! | `/models worker` | Wahl je UIA-Worker-Rolle („wie UIA“ oder Provider/Modell), Runde 5 Teil G |
//! | `/models worker <rolle\|all> <uia\|ziel>` | Setzt die Wahl einer bzw. aller UIA-Worker-Rollen |
//!
//! `<ziel>` ist entweder eine bekannte Modell-ID (Schlüssel, `id` oder Alias
//! aus `config.models`; der Provider wird aus dem Katalog übernommen) oder
//! `provider/modell`, getrennt am **ersten** `/`, sofern das Präfix ein
//! aktivierter Provider ist (z. B. `openrouter/nvidia/nemotron-…`).
//!
//! Änderungen an Kind-Rollen (Orchestrator, Sub-Orchestrator, Worker,
//! Explorer, Recherche, Gedächtnis) gelten in einer TUI-Sitzung sofort für
//! **neu gestartete** Agenten (Live-Modellwechsel über
//! [`crate::live_model::LiveModelControl`]); alle übrigen Rollen wirken ab
//! der nächsten Sitzung. `/model` wechselt das Modell der laufenden Sitzung
//! live.
//!
//! # Sicherheitsregel — kein ModelTool
//! `permission = "operator"`, `visibility = "tui_only"`: das Modell darf die
//! Modellwahl (auch die seiner Kinder) nicht selbst verändern.
//!
//! # Schlüsseltypen
//! - [`ModelsArgs`] — positionale Argumente `action role target`.
//! - `ModelsOperation` — vom `#[operation]`-Makro erzeugter Op-Struct.
//!
//! # Datenvertrag (`OpOutput::data`)
//! `show` → `{"roles":[{"role","label","provider","model","source","effort"}],"live":{"provider","model"}}`;
//! `set`/`reset` → `{"action","role","provider","model","persisted","note"}`.
//!
//! # Nebenläufigkeit
//! Zustandslos; Persistenz über den austauschbaren
//! [`crate::config_util::SelectionPersistence`]-Dienst (bestes Bemühen, nie `Err`).
//!
//! # Fehler
//! - [`OpError::InvalidArguments`]: unbekanntes Sub-Kommando, unbekannte Rolle,
//!   fehlendes/unauflösbares Ziel, Worker-Modell eines fremden Providers.
//! - [`OpError::Execution`]: Config-Discovery fehlgeschlagen.

use harw_config::{ModelRole, ResolvedConfig, RoleModelRow};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput, SharedSessionController};
use serde_json::{Value, json};

/// Hinweis, der jeder Anzeige und jeder Änderung beigefügt wird.
pub(crate) const EFFECTIVE_NEXT_SESSION: &str =
    "Änderungen wirken ab der nächsten Sitzung (nur /model wechselt live).";

// ── Argumente ────────────────────────────────────────────────────────────────

/// Argumente der `/models`-Operation.
///
/// # Felder
/// - `action`: `show` (Standard), `set` oder `reset` (Token 0).
/// - `role`: Rollenschlüssel, z. B. `uia`, `worker-simple` (Token 1).
/// - `target`: Modell-ID/Alias oder `provider/modell` (Token 2, nur `set`).
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct ModelsArgs {
    /// Sub-Kommando; `None` wird als `show` behandelt.
    #[serde(default)]
    #[raw(first)]
    pub action: Option<String>,
    /// Rollenschlüssel (siehe [`ModelRole::key`]).
    #[serde(default)]
    #[raw(nth = 1)]
    pub role: Option<String>,
    /// Ziel für `set`.
    #[serde(default)]
    #[raw(nth = 2)]
    pub target: Option<String>,
}

// ── Hilfsfunktionen ──────────────────────────────────────────────────────────

/// Kommagetrennte Liste aller gültigen Rollenschlüssel (für Fehlermeldungen).
fn known_roles() -> String {
    ModelRole::ALL
        .iter()
        .map(|role| role.key())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parst den Rollenschlüssel oder liefert eine deutsche Fehlermeldung.
fn parse_role(raw: Option<&str>, usage: &str) -> Result<ModelRole, OpError> {
    let raw = match raw.map(str::trim) {
        Some(value) if !value.is_empty() => value,
        _ => {
            return Err(OpError::InvalidArguments(format!(
                "Rolle fehlt. Aufruf: {usage}. Bekannte Rollen: {}.",
                known_roles()
            )));
        }
    };
    ModelRole::parse(raw).ok_or_else(|| {
        OpError::InvalidArguments(format!(
            "Unbekannte Rolle: '{raw}'. Bekannte Rollen: {}.",
            known_roles()
        ))
    })
}

/// Liefert den kanonischen Provider-Namen zu `name` (Schlüssel oder `name`
/// eines konfigurierten Providers), sofern dieser aktiviert ist.
fn enabled_provider_name(config: &ResolvedConfig, name: &str) -> Option<String> {
    config
        .providers
        .iter()
        .find(|(key, provider)| key.as_str() == name || provider.name == name)
        .filter(|(_, provider)| provider.enabled)
        .map(|(_, provider)| provider.name.clone())
}

/// Normalisiert einen Provider-Verweis aus dem Modellkatalog auf den
/// kanonischen Provider-Namen (Schlüssel → `name`); unbekannt bleibt unverändert.
fn canonical_provider_name(config: &ResolvedConfig, name: &str) -> String {
    config
        .providers
        .iter()
        .find(|(key, provider)| key.as_str() == name || provider.name == name)
        .map(|(_, provider)| provider.name.clone())
        .unwrap_or_else(|| name.to_owned())
}

/// Löst `target` zu `(provider, modell)` auf.
///
/// # Beschreibung
/// 1. Bekannte Modell-ID: Schlüssel in `config.models`, `id` oder Alias →
///    Provider aus dem Katalog, Modell = kanonische `id`.
/// 2. Sonst `provider/modell`, getrennt am ersten `/`, wenn das Präfix ein
///    aktivierter Provider ist.
///
/// # Fehler
/// [`OpError::InvalidArguments`] mit deutscher Meldung, wenn beides scheitert.
pub(crate) fn resolve_target(
    config: &ResolvedConfig,
    target: &str,
) -> Result<(String, String), OpError> {
    let known = config
        .models
        .get(target)
        .or_else(|| crate::model::configured_model(config, target));
    if let Some(model) = known {
        return Ok((
            canonical_provider_name(config, &model.provider),
            model.id.clone(),
        ));
    }
    if let Some((prefix, rest)) = target.split_once('/') {
        let rest = rest.trim();
        if !rest.is_empty()
            && let Some(provider) = enabled_provider_name(config, prefix.trim())
        {
            return Ok((provider, rest.to_owned()));
        }
    }
    Err(OpError::InvalidArguments(format!(
        "Unbekanntes Modell: '{target}'. Erwartet wird eine bekannte Modell-ID bzw. ein Alias \
         aus der Konfiguration oder 'provider/modell' mit einem aktivierten Provider."
    )))
}

/// Liefert den effektiven UIA-Provider: Live-/Config-UIA-Auswahl, sonst
/// `default_provider` (die UIA erbt ihn ohne eigenen Pin).
pub(crate) fn effective_uia_provider(
    controller: Option<&SharedSessionController>,
    config: &ResolvedConfig,
) -> Option<String> {
    let selection = crate::model::effective_uia_selection(controller, config);
    selection
        .provider()
        .map(str::to_owned)
        .or_else(|| config.harness.default_provider.clone())
        .map(|provider| canonical_provider_name(config, &provider))
}

/// Hängt eine Persistenz-Notiz an bzw. meldet den Erfolg.
pub(crate) fn finish_text(mut text: String, note: Option<&str>) -> String {
    match note {
        Some(note) => {
            text.push('\n');
            text.push_str(note);
        }
        None => {
            text.push_str(" — gespeichert. ");
            text.push_str(EFFECTIVE_NEXT_SESSION);
        }
    }
    text
}

/// Wie [`finish_text`], meldet aber eine live übernommene Rollenwahl.
///
/// # Argumente
/// - `live`: `true`, wenn die laufende Sitzung die Wahl bereits für neu
///   gestartete Agenten übernommen hat
///   ([`crate::live_model::LiveModelControl::set_internal_model`]).
fn finish_role_text(text: String, note: Option<&str>, live: bool) -> String {
    if !live {
        return finish_text(text, note);
    }
    let mut text = text;
    match note {
        Some(note) => {
            text.push_str(" — gilt ab sofort für neu gestartete Agenten.\n");
            text.push_str(note);
        }
        None => text.push_str(
            " — gespeichert; gilt ab sofort für neu gestartete Agenten \
             (laufende behalten ihr Modell).",
        ),
    }
    text
}

/// Zellinhalt für die Tabelle: `-` statt leer.
fn cell(value: Option<&str>) -> &str {
    value.filter(|value| !value.is_empty()).unwrap_or("-")
}

/// Baut Text und `data` für `show`.
///
/// # Argumente
/// - `rows`: aufgelöste Rollenzeilen ([`harw_config::resolve_role_models`]).
/// - `live_provider`/`live_model`: Live-Auswahl der Sitzung (Controller-Snapshot).
fn render_show(
    rows: &[RoleModelRow],
    live_provider: Option<&str>,
    live_model: Option<&str>,
) -> OpOutput {
    let headers = ["Rolle", "Provider", "Modell", "Quelle", "Effort"];
    let table: Vec<[String; 5]> = rows
        .iter()
        .map(|row| {
            [
                row.role.key().to_owned(),
                cell(row.provider.as_deref()).to_owned(),
                cell(row.model.as_deref()).to_owned(),
                row.source.label().to_owned(),
                cell(row.reasoning_effort.as_deref()).to_owned(),
            ]
        })
        .collect();

    let mut widths = headers.map(|header| header.chars().count());
    for line in &table {
        for (width, value) in widths.iter_mut().zip(line.iter()) {
            *width = (*width).max(value.chars().count());
        }
    }
    let format_line = |values: [&str; 5]| -> String {
        values
            .iter()
            .zip(widths.iter())
            .map(|(value, width)| {
                let pad = width.saturating_sub(value.chars().count());
                format!("{value}{}", " ".repeat(pad))
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };

    let mut lines = vec![
        format!(
            "Live (diese Sitzung): {} / {}",
            cell(live_provider),
            cell(live_model)
        ),
        String::new(),
        format_line(headers),
    ];
    for line in &table {
        lines.push(format_line([
            line[0].as_str(),
            line[1].as_str(),
            line[2].as_str(),
            line[3].as_str(),
            line[4].as_str(),
        ]));
    }
    lines.push(String::new());
    lines.push(format!(
        "{EFFECTIVE_NEXT_SESSION} Kind-Rollen (Orchestrator, Worker, Explorer, Recherche, \
         Gedächtnis) gelten sofort für neu gestartete Agenten. \
         Ändern: /models set <rolle> <modell|provider/modell>, \
         /models reset <rolle>."
    ));

    let roles: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "role": row.role.key(),
                "label": row.role.label(),
                "provider": row.provider,
                "model": row.model,
                "source": row.source.label(),
                "effort": row.reasoning_effort,
            })
        })
        .collect();
    let data = json!({
        "roles": roles,
        "live": { "provider": live_provider, "model": live_model },
        "note": EFFECTIVE_NEXT_SESSION,
    });
    OpOutput {
        text: lines.join("\n"),
        data: Some(data),
    }
}

/// Führt `set <rolle> <ziel>` aus.
fn handle_set(
    ctx: &OpContext,
    config: &ResolvedConfig,
    role: ModelRole,
    target: &str,
) -> Result<OpOutput, OpError> {
    let (provider, model) = resolve_target(config, target)?;
    let persistence = crate::config_util::selection_persistence(ctx);
    // Live-Übernahme: nur eine Stelle, deren Provider jetzt erreichbar ist,
    // wird überhaupt gespeichert (sonst bliebe eine unbrauchbare Wahl stehen).
    if role.internal_point().is_some() {
        crate::live_model::ensure_provider_ready(ctx, &provider)?;
    }
    let mut live = false;

    let note = match role {
        ModelRole::Uia => {
            persistence.persist_uia_selection(Some(provider.as_str()), Some(model.as_str()))
        }
        // Runde 5, Teil G: keine Kopplung an den UIA-Provider mehr — die
        // Rolle `uia-worker` bekommt eine eigene Wahl mit eigenem Provider.
        ModelRole::UiaWorker => persistence.persist_uia_worker_role_model(
            "uia-worker",
            Some(format!("{provider}/{model}").as_str()),
        ),
        other => {
            let point = other.internal_point().ok_or_else(|| {
                OpError::InvalidArguments(format!(
                    "Für die Rolle '{}' ist keine eigene Modellwahl vorgesehen.",
                    other.key()
                ))
            })?;
            let note = persistence.persist_internal_model(
                point,
                Some(provider.as_str()),
                Some(model.as_str()),
            );
            live = crate::live_model::apply_internal_model(
                ctx,
                point,
                Some(harw_config::InternalModelChoice {
                    provider: Some(provider.clone()),
                    model: Some(model.clone()),
                }),
            );
            note
        }
    };

    let text = finish_role_text(
        format!("Modell für {} gesetzt: {provider}/{model}", role.label()),
        note.as_deref(),
        live,
    );
    Ok(OpOutput {
        text,
        data: Some(json!({
            "action": "set",
            "role": role.key(),
            "provider": provider,
            "model": model,
            "persisted": note.is_none(),
            "note": note,
        })),
    })
}

/// Führt `reset <rolle>` aus.
fn handle_reset(ctx: &OpContext, role: ModelRole) -> Result<OpOutput, OpError> {
    let persistence = crate::config_util::selection_persistence(ctx);
    let mut live = false;
    let note = match role {
        ModelRole::Uia => persistence.clear_uia_selection(),
        // Runde 5, Teil G: eigener Eintrag und alter Familien-Pin weg → die
        // Rolle folgt wieder der UIA.
        ModelRole::UiaWorker => {
            let role_note = persistence.persist_uia_worker_role_model("uia-worker", None);
            let legacy_note = persistence.persist_uia_worker_model(None);
            role_note.or(legacy_note)
        }
        other => {
            let point = other.internal_point().ok_or_else(|| {
                OpError::InvalidArguments(format!(
                    "Für die Rolle '{}' gibt es keine eigene Modellwahl zum Zurücksetzen.",
                    other.key()
                ))
            })?;
            let note = persistence.persist_internal_model(point, None, None);
            live = crate::live_model::apply_internal_model(ctx, point, None);
            note
        }
    };

    let text = finish_role_text(
        format!("Modellwahl für {} zurückgesetzt", role.label()),
        note.as_deref(),
        live,
    );
    Ok(OpOutput {
        text,
        data: Some(json!({
            "action": "reset",
            "role": role.key(),
            "provider": Value::Null,
            "model": Value::Null,
            "persisted": note.is_none(),
            "note": note,
        })),
    })
}

// ── Operation ────────────────────────────────────────────────────────────────

/// Zeigt oder ändert das Modell je Rolle.
///
/// # Beschreibung
/// Siehe Moduldoku für Grammatik und Datenvertrag. `show` liest die
/// aufgelöste Config (context-scoped zuerst, siehe
/// [`crate::provider::resolved_config`]) und den Live-Snapshot des
/// [`SharedSessionController`] (falls registriert).
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: unbekanntes Sub-Kommando, Rolle oder Ziel.
/// - [`OpError::Execution`]: Config-Discovery fehlgeschlagen.
///
/// # Panics
/// Nie.
#[operation(
    name = "models",
    summary = "Zeigt/setzt das Modell je Rolle (UIA, Worker, Orchestrator, interne Stellen); wirkt ab der nächsten Sitzung.",
    domain = "catalog_config",
    permission = "operator",
    category = "model",
    command(path = "/models", visibility = "tui_only", busy = "immediate")
)]
async fn models(ctx: &OpContext, args: ModelsArgs) -> Result<OpOutput, OpError> {
    let action = args
        .action
        .as_deref()
        .map(str::trim)
        .filter(|action| !action.is_empty())
        .unwrap_or("show");

    match action {
        "show" | "list" => {
            let config = crate::provider::resolved_config(ctx)?;
            let rows = harw_config::resolve_role_models(&config);
            let snapshot = ctx
                .service::<SharedSessionController>()
                .map(|controller| controller.snapshot());
            let live_provider = snapshot
                .as_ref()
                .and_then(|snap| snap.active_provider.as_deref());
            let live_model = snapshot
                .as_ref()
                .and_then(|snap| snap.active_model.as_deref());
            Ok(render_show(&rows, live_provider, live_model))
        }
        "set" => {
            let usage = "/models set <rolle> <modell|provider/modell>";
            let role = parse_role(args.role.as_deref(), usage)?;
            let target = match args.target.as_deref().map(str::trim) {
                Some(target) if !target.is_empty() => target.to_owned(),
                _ => {
                    return Err(OpError::InvalidArguments(format!(
                        "Ziel fehlt. Aufruf: {usage}."
                    )));
                }
            };
            let config = crate::provider::resolved_config(ctx)?;
            handle_set(ctx, &config, role, &target)
        }
        "reset" | "clear" => {
            let role = parse_role(args.role.as_deref(), "/models reset <rolle>")?;
            handle_reset(ctx, role)
        }
        // Runde 5, Teil G: eigene Modellwahl je UIA-Worker-Rolle.
        "worker" | "workers" => {
            let config = crate::provider::resolved_config(ctx)?;
            crate::models_workers::handle_worker(
                ctx,
                &config,
                args.role.as_deref(),
                args.target.as_deref(),
            )
        }
        other => Err(OpError::InvalidArguments(format!(
            "Unbekanntes /models-Sub-Kommando: '{other}'. Gültig: show, \
             set <rolle> <modell|provider/modell>, reset <rolle>, \
             worker [<rolle|all> <uia|modell|provider/modell>]."
        ))),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{ModelsArgs, ModelsOperation, resolve_target};
    use crate::config_util::{
        RecordedSelectionPersistCall, RecordingSelectionPersistence, SelectionPersistence,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_config::{InternalModelPoint, ModelRole, ResolvedConfig};
    use harw_operations::context::ServiceMap;
    use harw_operations::{
        FromRawArgs, NullSessionController, OpContext, OpError, OpOutput, Operation,
        SessionController, SharedSessionController, Surface,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn provider(name: &str, enabled: bool) -> harw_config::ProviderToml {
        harw_config::ProviderToml {
            stream: None,
            name: name.to_owned(),
            api: "openai-chat".to_owned(),
            base_url: format!("https://api.example.test/{name}"),
            auth: None,
            auth_header: None,
            api_key: None,
            headers: Default::default(),
            models: Vec::new(),
            enabled,
            origin_allowlist: Default::default(),
            rate_limit: None,
            max_concurrency: None,
            originator: None,
            default_reasoning_effort: None,
            gateway_identity_headers: false,
            request_timeout_secs: None,
            stream_idle_timeout_secs: None,
            retry_timeouts: None,
            max_tokens_field: None,
            send_reasoning_effort: None,
            strict_tools: None,
            parallel_tool_calls: None,
            allow_insecure_lan: false,
        }
    }

    fn model(id: &str, provider_name: &str, aliases: &[&str]) -> harw_config::ModelToml {
        harw_config::ModelToml {
            stream: None,
            rate_limit: None,
            id: id.to_owned(),
            name: None,
            provider: provider_name.to_owned(),
            aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
            context_window: None,
            max_tokens: None,
            reasoning: false,
            input_types: Vec::new(),
            capabilities: harw_config::ModelCapabilitiesToml::default(),
            prompt_caching: None,
            default_reasoning_effort: None,
        }
    }

    /// Zwei aktivierte Provider (`provider-a`, `openrouter`), ein
    /// deaktivierter (`off`), zwei Katalogmodelle.
    fn test_config() -> ResolvedConfig {
        let mut config = ResolvedConfig::default();
        config
            .providers
            .insert("provider-a".to_owned(), provider("provider-a", true));
        config
            .providers
            .insert("openrouter".to_owned(), provider("openrouter", true));
        config
            .providers
            .insert("off".to_owned(), provider("off", false));
        config.models.insert(
            "model-a".to_owned(),
            model("model-a-2026", "provider-a", &["a-latest"]),
        );
        config.models.insert(
            "router-model".to_owned(),
            model("vendor/router-model", "openrouter", &[]),
        );
        config
    }

    struct Fixture {
        ctx: OpContext,
        recorder: Arc<RecordingSelectionPersistence>,
        root: std::path::PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    fn fixture(
        config: ResolvedConfig,
        controller: Option<SharedSessionController>,
    ) -> TestResult<Fixture> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-models-test-{}-{id}", std::process::id()));
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
        services.insert(Arc::new(config));
        if let Some(controller) = controller {
            services.insert(controller);
        }
        Ok(Fixture {
            ctx: OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            recorder,
            root,
        })
    }

    async fn run(fixture: &Fixture, tokens: &[&str]) -> Result<OpOutput, OpError> {
        let args = ModelsArgs::from_raw_args(&toks(tokens))?;
        super::models(&fixture.ctx, args).await
    }

    fn data(output: &OpOutput) -> TestResult<&serde_json::Value> {
        output
            .data
            .as_ref()
            .ok_or(TestError::Missing("OpOutput.data"))
    }

    #[test]
    fn models_args_from_raw_args_maps_three_positions() -> TestResult {
        let args = ModelsArgs::from_raw_args(&toks(&["set", "uia", "model-a"]))
            .map_err(ctx("ModelsArgs::from_raw_args"))?;
        assert_eq!(args.action.as_deref(), Some("set"));
        assert_eq!(args.role.as_deref(), Some("uia"));
        assert_eq!(args.target.as_deref(), Some("model-a"));

        let empty = ModelsArgs::from_raw_args(&toks(&[])).map_err(ctx("leer"))?;
        assert_eq!(empty, ModelsArgs::default());
        Ok(())
    }

    #[test]
    fn models_declares_tui_only_command_without_model_tool() {
        let meta = ModelsOperation.meta();
        assert_eq!(meta.name, "models");
        assert!(
            !meta
                .surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. })),
            "das Modell darf die Modellwahl nicht selbst ändern"
        );
        assert!(meta.surfaces.iter().any(|surface| matches!(
            surface,
            Surface::Command {
                path,
                visibility: harw_operations::CommandVisibility::TuiOnly
            } if *path == "/models"
        )));
    }

    #[test]
    fn resolve_target_prefers_known_model_key_id_and_alias() -> TestResult {
        let config = test_config();
        for target in ["model-a", "model-a-2026", "a-latest"] {
            let resolved = resolve_target(&config, target).map_err(ctx("bekanntes Modell"))?;
            assert_eq!(
                resolved,
                ("provider-a".to_owned(), "model-a-2026".to_owned()),
                "Ziel '{target}'"
            );
        }
        // Eine Katalog-ID mit `/` darf nicht als provider/modell zerlegt werden.
        let resolved =
            resolve_target(&config, "vendor/router-model").map_err(ctx("ID mit Schrägstrich"))?;
        assert_eq!(
            resolved,
            ("openrouter".to_owned(), "vendor/router-model".to_owned())
        );
        Ok(())
    }

    #[test]
    fn resolve_target_splits_provider_prefix_at_first_slash() -> TestResult {
        let config = test_config();
        let resolved = resolve_target(&config, "openrouter/nvidia/nemotron-x")
            .map_err(ctx("provider/modell"))?;
        assert_eq!(
            resolved,
            ("openrouter".to_owned(), "nvidia/nemotron-x".to_owned())
        );
        Ok(())
    }

    #[test]
    fn resolve_target_rejects_unknown_and_disabled_provider() {
        let config = test_config();
        for target in [
            "unbekannt",
            "off/some-model",
            "nope/some-model",
            "openrouter/",
        ] {
            assert!(
                matches!(
                    resolve_target(&config, target),
                    Err(OpError::InvalidArguments(ref message)) if message.contains("Unbekanntes Modell")
                ),
                "Ziel '{target}' muss abgelehnt werden"
            );
        }
    }

    #[tokio::test]
    async fn models_show_lists_every_role_and_live_selection() -> TestResult {
        let controller = Arc::new(NullSessionController::new());
        controller
            .set_active_provider("provider-a".to_owned())
            .map_err(ctx("set_active_provider"))?;
        controller
            .set_active_model("model-a-2026".to_owned())
            .map_err(ctx("set_active_model"))?;
        let shared: SharedSessionController = controller;
        let fixture = fixture(test_config(), Some(shared))?;

        let output = run(&fixture, &["show"])
            .await
            .map_err(ctx("show darf nicht fehlschlagen"))?;
        let data = data(&output)?;
        let roles = data["roles"]
            .as_array()
            .ok_or(TestError::Missing("roles"))?;
        assert_eq!(roles.len(), ModelRole::ALL.len());
        for (row, role) in roles.iter().zip(ModelRole::ALL) {
            assert_eq!(row["role"], serde_json::json!(role.key()));
            for key in ["label", "provider", "model", "source", "effort"] {
                assert!(row.get(key).is_some(), "Feld '{key}' fehlt: {row}");
            }
        }
        assert_eq!(data["live"]["provider"], serde_json::json!("provider-a"));
        assert_eq!(data["live"]["model"], serde_json::json!("model-a-2026"));
        assert!(
            output.text.contains("ab der nächsten Sitzung"),
            "{}",
            output.text
        );
        assert!(output.text.contains("worker-simple"), "{}", output.text);
        assert!(fixture.recorder.calls().is_empty(), "show schreibt nichts");
        Ok(())
    }

    #[tokio::test]
    async fn models_show_without_controller_reports_null_live() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        let output = run(&fixture, &[]).await.map_err(ctx("show"))?;
        let data = data(&output)?;
        assert_eq!(data["live"]["provider"], serde_json::Value::Null);
        assert_eq!(data["live"]["model"], serde_json::Value::Null);
        Ok(())
    }

    #[tokio::test]
    async fn models_set_uia_persists_uia_selection() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        let output = run(&fixture, &["set", "uia", "a-latest"])
            .await
            .map_err(ctx("set uia"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![RecordedSelectionPersistCall::UiaSelection {
                provider: Some("provider-a".to_owned()),
                model: Some("model-a-2026".to_owned()),
            }]
        );
        let data = data(&output)?;
        assert_eq!(data["persisted"], serde_json::json!(true));
        assert!(output.text.contains("nächsten Sitzung"), "{}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn models_set_internal_role_persists_internal_model() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        run(
            &fixture,
            &["set", "explorer", "openrouter/nvidia/nemotron-x"],
        )
        .await
        .map_err(ctx("set explorer"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![RecordedSelectionPersistCall::InternalModel {
                point: InternalModelPoint::Explorer,
                provider: Some("openrouter".to_owned()),
                model: Some("nvidia/nemotron-x".to_owned()),
            }]
        );
        Ok(())
    }

    #[tokio::test]
    async fn models_set_orchestrator_uses_root_orchestrator_point() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        run(&fixture, &["set", "orchestrator", "model-a"])
            .await
            .map_err(ctx("set orchestrator"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![RecordedSelectionPersistCall::InternalModel {
                point: InternalModelPoint::RootOrchestrator,
                provider: Some("provider-a".to_owned()),
                model: Some("model-a-2026".to_owned()),
            }]
        );
        Ok(())
    }

    /// Live-Modellwechsel: `/models set orchestrator …` prüft den Provider,
    /// meldet die Wahl an die laufende Montage (neu gestartete Agenten nehmen
    /// sie sofort) und sagt das in der Bestätigung.
    #[tokio::test]
    async fn models_set_child_role_applies_live_when_the_runtime_supports_it() -> TestResult {
        type Call = (InternalModelPoint, Option<harw_config::InternalModelChoice>);
        #[derive(Default)]
        struct LiveRecorder {
            ready: std::sync::Mutex<Vec<String>>,
            calls: std::sync::Mutex<Vec<Call>>,
        }
        impl crate::live_model::LiveModelControl for LiveRecorder {
            fn ensure_provider_ready(&self, provider: &str) -> Result<(), String> {
                if let Ok(mut ready) = self.ready.lock() {
                    ready.push(provider.to_owned());
                }
                Ok(())
            }
            fn set_internal_model(
                &self,
                point: InternalModelPoint,
                choice: Option<harw_config::InternalModelChoice>,
            ) -> bool {
                if let Ok(mut calls) = self.calls.lock() {
                    calls.push((point, choice));
                }
                true
            }
        }

        let base = fixture(test_config(), None)?;
        let live = Arc::new(LiveRecorder::default());
        let persistence: Arc<dyn SelectionPersistence> = base.recorder.clone();
        let mut services = ServiceMap::new();
        services.insert(persistence);
        services.insert(Arc::new(test_config()));
        services.insert(Arc::clone(&live) as crate::live_model::SharedLiveModelControl);
        let live_ctx = OpContext::new(
            SessionId::new(),
            TurnId::new(),
            base.ctx.sandbox().clone(),
            services,
        );
        let args = ModelsArgs::from_raw_args(&toks(&["set", "orchestrator", "model-a"]))
            .map_err(ctx("args"))?;
        let output = super::models(&live_ctx, args)
            .await
            .map_err(ctx("set orchestrator"))?;

        assert_eq!(
            live.ready.lock().map(|ready| ready.clone()).ok(),
            Some(vec!["provider-a".to_owned()])
        );
        assert_eq!(
            live.calls.lock().map(|calls| calls.clone()).ok(),
            Some(vec![(
                InternalModelPoint::RootOrchestrator,
                Some(harw_config::InternalModelChoice {
                    provider: Some("provider-a".to_owned()),
                    model: Some("model-a-2026".to_owned()),
                }),
            )])
        );
        assert!(
            output
                .text
                .contains("gilt ab sofort für neu gestartete Agenten"),
            "{}",
            output.text
        );
        assert_eq!(
            base.recorder.calls().len(),
            1,
            "die Wahl wird weiterhin gespeichert"
        );
        Ok(())
    }

    /// Live-Schnappschuss: eine gespeicherte Rollenwahl steht im folgenden
    /// `/models show` (Datenquelle der Übersicht F8) sofort drin — auch nach
    /// einem Zurücksetzen. Vorher blieb die Tabelle bis zum Neustart beim
    /// Stand des Starts.
    #[tokio::test]
    async fn models_set_and_reset_are_visible_in_the_following_show() -> TestResult {
        let base = fixture(test_config(), None)?;
        let persistence: Arc<dyn SelectionPersistence> = base.recorder.clone();
        let live: crate::live_config::SharedLiveConfig =
            Arc::new(crate::live_config::LiveConfig::new(Arc::new(test_config())));
        let mut services = ServiceMap::new();
        services.insert(persistence);
        services.insert(Arc::new(test_config()));
        services.insert(Arc::clone(&live));
        let live_ctx = OpContext::new(
            SessionId::new(),
            TurnId::new(),
            base.ctx.sandbox().clone(),
            services,
        );
        async fn run_in(ctx: &OpContext, tokens: &[&str]) -> Result<OpOutput, OpError> {
            let args = ModelsArgs::from_raw_args(&toks(tokens))?;
            super::models(ctx, args).await
        }
        let row = |output: &OpOutput, role: &str| -> TestResult<serde_json::Value> {
            data(output)?["roles"]
                .as_array()
                .and_then(|roles| roles.iter().find(|row| row["role"] == role).cloned())
                .ok_or(TestError::Missing("role row"))
        };

        run_in(
            &live_ctx,
            &["set", "explorer", "openrouter/vendor/router-model"],
        )
        .await
        .map_err(ctx("set explorer"))?;
        run_in(&live_ctx, &["set", "uia", "model-a"])
            .await
            .map_err(ctx("set uia"))?;
        let shown = run_in(&live_ctx, &["show"]).await.map_err(ctx("show"))?;
        let explorer = row(&shown, "explorer")?;
        assert_eq!(explorer["provider"], serde_json::json!("openrouter"));
        assert_eq!(explorer["model"], serde_json::json!("vendor/router-model"));
        let uia = row(&shown, "uia")?;
        assert_eq!(uia["provider"], serde_json::json!("provider-a"));
        assert_eq!(uia["model"], serde_json::json!("model-a-2026"));

        run_in(&live_ctx, &["reset", "explorer"])
            .await
            .map_err(ctx("reset explorer"))?;
        let shown = run_in(&live_ctx, &["show"])
            .await
            .map_err(ctx("show after reset"))?;
        assert_ne!(
            row(&shown, "explorer")?["model"],
            serde_json::json!("vendor/router-model")
        );
        Ok(())
    }

    /// Runde 5, Teil G: ein UIA-Worker-Modell eines anderen Providers als
    /// die UIA wird angenommen und als eigene Rollenwahl gespeichert.
    #[tokio::test]
    async fn models_set_uia_worker_accepts_another_provider() -> TestResult {
        let mut config = test_config();
        config.harness.uia_provider = Some("provider-a".to_owned());
        let fixture = fixture(config, None)?;

        run(&fixture, &["set", "uia-worker", "openrouter/some-model"])
            .await
            .map_err(ctx("fremder Provider"))?;
        run(&fixture, &["set", "uia-worker", "model-a"])
            .await
            .map_err(ctx("passender Provider"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![
                RecordedSelectionPersistCall::UiaWorkerRoleModel {
                    role: "uia-worker".to_owned(),
                    value: Some("openrouter/some-model".to_owned()),
                },
                RecordedSelectionPersistCall::UiaWorkerRoleModel {
                    role: "uia-worker".to_owned(),
                    value: Some("provider-a/model-a-2026".to_owned()),
                },
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn models_reset_dispatches_per_role() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        run(&fixture, &["reset", "uia"])
            .await
            .map_err(ctx("reset uia"))?;
        run(&fixture, &["reset", "uia-worker"])
            .await
            .map_err(ctx("reset uia-worker"))?;
        run(&fixture, &["reset", "worker-simple"])
            .await
            .map_err(ctx("reset worker-simple"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![
                RecordedSelectionPersistCall::ClearUia,
                RecordedSelectionPersistCall::UiaWorkerRoleModel {
                    role: "uia-worker".to_owned(),
                    value: None,
                },
                RecordedSelectionPersistCall::UiaWorkerModel { model: None },
                RecordedSelectionPersistCall::InternalModel {
                    point: InternalModelPoint::WorkerSimple,
                    provider: None,
                    model: None,
                },
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn models_rejects_bad_input_in_german() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        for (tokens, needle) in [
            (vec!["frobnicate"], "Unbekanntes /models-Sub-Kommando"),
            (vec!["set"], "Rolle fehlt"),
            (vec!["set", "kapitän", "model-a"], "Unbekannte Rolle"),
            (vec!["set", "uia"], "Ziel fehlt"),
            (vec!["set", "uia", "gibtsnicht"], "Unbekanntes Modell"),
            (vec!["reset"], "Rolle fehlt"),
        ] {
            let result = run(&fixture, &tokens).await;
            match result {
                Err(OpError::InvalidArguments(message)) => assert!(
                    message.contains(needle),
                    "{tokens:?}: erwartet '{needle}' in '{message}'"
                ),
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{tokens:?}: InvalidArguments erwartet, war {other:?}"
                    )));
                }
            }
        }
        assert!(fixture.recorder.calls().is_empty());
        Ok(())
    }
}
