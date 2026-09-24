//! Onboarding-Wizard: richtet Provider, Modell und optional Channel ein.
//!
//! Dieses Modul besitzt den First-Run-Ablauf von `harw`: es fragt (interaktiv)
//! oder wählt (nicht-interaktiv per `HARW_ONBOARD_NONINTERACTIVE`) die minimale
//! Konfiguration, schreibt sie in den aktiven Profil-Layer und verifiziert das
//! Ergebnis über `harw_config::discover_config` + `validate`.
//!
//! Geschriebene Artefakte (relativ zum aktiven Profil-Verzeichnis):
//! - `providers/<name>.toml` ([`harw_config::ProviderToml`])
//! - `models/<id>.toml` ([`harw_config::ModelToml`])
//! - `config.toml` ([`harw_config::HarnessConfig`]) mit `default_provider`,
//!   `default_model` und den `onboarding.seen.*`-Flags.
//!
//! Secrets landen niemals als Klartext in einer TOML-Datei: Der Provider
//! referenziert seinen Schlüssel entweder per `env:`- oder (bei interaktiver
//! Key-Eingabe) per `file:`-[`harw_config::SecretRef`]. Die Schlüsseldatei
//! liegt unter `<home>/secrets/<name>.key` und wird unter Unix auf `0o600`
//! gesetzt.
//!
//! # Verantwortungsabgrenzung
//! Der Wizard schreibt bewusst in den **Profil**-Layer (nicht den Home-Layer),
//! weil `discover_config` `harness` pro Layer vollständig ersetzt (letzter
//! Layer gewinnt) — `default_provider`/`default_model`/`onboarding.seen` müssen
//! daher im Profil stehen, damit sie nicht von einem Repo-Layer verdrängt
//! werden.
//!
//! # Concurrency
//! Zustandslos; ein `harw`-Prozess pro Root-Space ist die erwartete Nutzung.
//!
//! # Errors
//! Alle fallierbaren Operationen liefern eine menschenlesbare `String`-Ursache.

use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use harw_config::{HarnessConfig, ModelToml, ProviderToml, SecretRef};

/// Env-Variable, die den rein defaultbasierten (nicht-interaktiven) Pfad
/// erzwingt: gesetzt (beliebiger Wert) → keine `stdin`-Reads.
const NONINTERACTIVE_ENV: &str = "HARW_ONBOARD_NONINTERACTIVE";

/// Monotonic suffix for sibling secret-file temporaries. `create_new` remains
/// the authority for uniqueness; this only keeps retries in one process
/// collision-free without introducing another dependency.
static SECRET_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Führt den Onboarding-Wizard aus (interaktiv, oder rein defaultbasiert, wenn
/// `HARW_ONBOARD_NONINTERACTIVE` gesetzt ist) und persistiert Provider,
/// Modell sowie `onboarding.seen`-Flags in den aktiven Profil-Layer.
///
/// # Description
/// Reihenfolge der Schritte: Provider → optionaler API-Key (Auth) → Modell →
/// optionaler Channel (nur interaktiv, standardmäßig übersprungen). Abschließend
/// wird die gemergte Konfiguration geladen und validiert; schlägt das fehl, wird
/// der Fehler zurückgegeben. Im nicht-interaktiven Pfad werden ausschließlich
/// Defaults verwendet und eine kurze Bestätigung nach `stderr` geschrieben.
///
/// # Arguments
/// - `home` (`&Path`): Root-Space (`~/.harw` bzw. `HARW_HOME`). Muss bereits
///   gescaffoldet sein (`harw_home::ensure_home`).
///
/// # Returns
/// `Ok(())`, wenn alle Artefakte geschrieben und die Endkonfiguration valide
/// ist.
///
/// # Errors
/// Ein `String` mit menschenlesbarer Ursache bei Eingabe-, Schreib- oder
/// Validierungsfehlern.
///
/// # Concurrency
/// Nutzt keinen geteilten Zustand; sicher aus einem einzelnen Prozess.
pub fn run_wizard(home: &Path) -> Result<(), String> {
    let interactive = std::env::var_os(NONINTERACTIVE_ENV).is_none();
    run_wizard_with_interaction_mode(home, interactive)
}

/// Führt den Wizard mit explizit vorgegebenem Interaktionsmodus aus.
///
/// Der private Seam hält den nicht-interaktiven Pfad deterministisch: Tests
/// müssen weder die Prozessumgebung verändern noch davon abhängen, ob Cargo
/// über ein TTY läuft.
fn run_wizard_with_interaction_mode(home: &Path, interactive: bool) -> Result<(), String> {
    // Interaktiv mit echtem Terminal: der ratatui-Setup-Picker über den
    // Provider-Katalog. Ohne TTY (Tests/Pipes) verwenden wir die
    // zeilenbasierten Prompts; ein expliziter Abbruch bleibt ein Abbruch.
    if interactive && std::io::stdin().is_terminal() {
        // Der Setup-Picker bleibt deterministisch. Die konto-spezifische
        // Quelle ist `harw models scan`, nicht ein impliziter Drittanbieter-
        // Download, der die Auswahl unerwartet aufbläht.
        let catalog = harw_model_catalog::embedded_catalog();
        if let Some(outcome) = harw_tui::run_setup(catalog).map_err(|e| e.to_string())? {
            persist_outcome(home, &outcome)?;
            return maybe_recommend_openrouter(home, interactive);
        }
        return Err("Einrichtung abgebrochen".to_owned());
    }

    let provider_name = prompt_default(interactive, "Provider-Name", "openai")?;
    let catalog = harw_model_catalog::embedded_catalog();
    let spec = catalog.iter().find(|provider| provider.id == provider_name);
    let base_url = prompt_default(
        interactive,
        "Provider-/Ressourcen-Basis-URL",
        spec.map_or("https://api.openai.com/v1", |p| p.base_url.as_str()),
    )?;
    let api_default = spec.map_or("openai-responses", |p| match p.api {
        harw_model_catalog::ProviderApi::AnthropicMessages => "anthropic-messages",
        harw_model_catalog::ProviderApi::OpenAiChat => "openai-chat",
        harw_model_catalog::ProviderApi::Ollama => "ollama",
        harw_model_catalog::ProviderApi::OpenAiResponses => "openai-responses",
    });
    let api = prompt_default(
        interactive,
        "API (openai-chat/openai-responses/anthropic-messages/ollama)",
        api_default,
    )?;
    let model = prompt_default(
        interactive,
        "Modell-ID / Deployment-Name",
        spec.and_then(|p| p.default_model.as_deref()).unwrap_or(""),
    )?;
    let secret_ref = prompt_default(
        interactive,
        "Secret-Referenz (env:/file:/secrets:)",
        "env:OPENAI_API_KEY",
    )?;
    let outcome = harw_tui::SetupOutcome {
        provider_id: provider_name,
        base_url,
        api,
        model,
        secret_ref: if secret_ref.is_empty() {
            None
        } else {
            Some(secret_ref)
        },
        auth_header: None,
    };
    persist_outcome(home, &outcome)?;
    maybe_recommend_openrouter(home, interactive)
}

/// Empfiehlt nach erfolgreicher Ersteinrichtung eines Providers, zusätzlich
/// `openrouter` einzurichten (Addendum C: Standard für interne Modellstellen
/// wie Session-Titel, Kontext-Verdichtung, Traumreflexion und
/// Explorer-/Recherche-Subagenten).
///
/// # Description
/// Überspringt still (ohne Ausgabe), wenn nicht-interaktiv, ohne TTY, oder
/// wenn bereits ein `providers/openrouter.toml` im aktiven Profil existiert.
/// Bei Zustimmung wird der Schlüssel wie beim ersten Provider entweder als
/// bereits fertige [`SecretRef`] übernommen oder über [`write_secret_file`]
/// als `file:`-Referenz abgelegt; danach wird `providers/openrouter.toml`
/// geschrieben (`api = "openai-chat"`, `base_url =
/// "https://openrouter.ai/api/v1"`, `enabled = true`, `models = []`).
///
/// # Errors
/// Ein `String` bei Schreib- oder Serialisierungsfehlern.
fn maybe_recommend_openrouter(home: &Path, interactive: bool) -> Result<(), String> {
    if !interactive || !std::io::stdin().is_terminal() {
        return Ok(());
    }
    let profile_name = harw_home::active_profile_name(home);
    let profile = harw_home::profile_dir(home, &profile_name).map_err(|e| e.to_string())?;
    // Nur ein *aktiver* OpenRouter zählt als „schon eingerichtet". Seit das
    // Scaffolding den gesamten Katalog als deaktivierte Dateien vorsät, wäre
    // die blosse Existenz der Datei kein Signal mehr und der Tipp liefe nie.
    if openrouter_is_enabled(&profile) {
        return Ok(());
    }

    println!();
    println!(
        "Tipp: Mit OpenRouter kann harw interne Aufgaben — Session-Titel, \
Kontext-Verdichtung, Traumreflexion sowie Explorer-/Recherche-Subagenten — \
über schnelle, günstige NVIDIA-Nemotron-Modelle abwickeln, statt dafür dein \
Hauptmodell zu belegen. Kostenlose ':free'-Varianten existieren, sind aber \
ratenbegrenzt und können Prompts protokollieren."
    );
    let answer = prompt_default(true, "OpenRouter jetzt einrichten? [y/N]", "n")?;
    if !matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "j" | "ja" | "y" | "yes"
    ) {
        println!(
            "Interne Aufgaben nutzen bis auf Weiteres das Hauptmodell; \
änderbar mit `harw models internal`."
        );
        return Ok(());
    }

    let raw_key = prompt_default(true, "OpenRouter-API-Schlüssel (leer = abbrechen)", "")?;
    let trimmed_key = raw_key.trim();
    if trimmed_key.is_empty() {
        println!(
            "Kein Schlüssel eingegeben — interne Aufgaben nutzen bis auf \
Weiteres das Hauptmodell; änderbar mit `harw models internal`."
        );
        return Ok(());
    }
    let auth_ref: SecretRef = if is_secret_ref(trimmed_key) {
        trimmed_key
            .parse()
            .map_err(|e: harw_config::ConfigError| e.to_string())?
    } else {
        write_secret_file(home, "openrouter", trimmed_key)?
    };

    let provider = ProviderToml {
        stream: None,
        name: "openrouter".to_owned(),
        api: "openai-chat".to_owned(),
        base_url: "https://openrouter.ai/api/v1".to_owned(),
        auth: Some(auth_ref),
        auth_header: None,
        api_key: None,
        headers: std::collections::HashMap::new(),
        models: Vec::new(),
        enabled: true,
        origin_allowlist: harw_config::OriginAllowlistToml::default(),
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
    };
    let providers_dir = profile.join("providers");
    create_dir_all(&providers_dir)?;
    write_file(
        &providers_dir.join("openrouter.toml"),
        &toml::to_string_pretty(&provider).map_err(|e| format!("provider serialisieren: {e}"))?,
    )?;
    println!("OpenRouter eingerichtet. Interne Modellstellen verwalten: `harw models internal`.");
    Ok(())
}

/// Persistiert das Ergebnis des ratatui-Setup-Pickers: schreibt Provider- und
/// Modell-TOML, ergänzt den `credential_pool` in `auth.toml` und setzt
/// `default_provider`/`default_model` + `onboarding.seen` im Profil.
///
/// Neben dem gewählten Modell wird — sofern der Katalog-Eintrag des Providers
/// keine Platzhalter-`base_url` hat (siehe [`is_placeholder_base_url`]) — die
/// gesamte Katalog-Modell-Liste ([`harw_model_catalog::embedded_catalog`])
/// als `providers/<id>.toml`-`models`-Eintrag und je eine
/// `models/<modell-id>.toml`-Datei angelegt, damit der `/model`-Picker nach
/// dem Onboarding die volle Auswahl zeigt. Bereits vorhandene
/// `models/<id>.toml`-Dateien werden dabei nicht überschrieben. Das gewählte
/// Modell bleibt in jedem Fall `default_model`.
///
/// # Errors
/// Ein `String` bei Schreib-, Serialisierungs- oder Validierungsfehlern.
/// `true`, wenn im Profil eine OpenRouter-Datei liegt, die auch aktiv ist.
///
/// Eine unlesbare oder unparsebare Datei gilt als „nicht aktiv": der Tipp ist
/// ein Hinweis, kein Sicherheitsentscheid, und darf an einem kaputten
/// Profileintrag nicht scheitern.
fn openrouter_is_enabled(profile: &Path) -> bool {
    let path = profile.join("providers").join("openrouter.toml");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return false;
    };
    toml::from_str::<ProviderToml>(&contents)
        .map(|provider| provider.enabled)
        .unwrap_or(false)
}

/// Übernimmt eine bereits vorgesäte Provider-Datei, statt sie zu ersetzen.
///
/// # Description
/// Das Scaffolding legt jeden Katalog-Provider deaktiviert und mit voller
/// Modellliste an. Würde das Onboarding diese Datei stumpf überschreiben, ginge
/// die Modellliste verloren und der Nutzer sähe nach dem Setup nur noch das
/// eine gewählte Modell. Deshalb wird eine vorhandene Datei geladen und nur in
/// den Feldern angefasst, über die das Setup tatsächlich entschieden hat.
///
/// # Arguments
/// - `path` (`&Path`): Zieldatei `providers/<id>.toml`.
/// - `fresh` (`ProviderToml`): die aus dem Setup-Ergebnis gebaute Fassung.
///
/// # Returns
/// Die zu schreibende Fassung: die zusammengeführte, falls `path` existiert und
/// parsebar ist, sonst `fresh` unverändert.
fn merge_with_seeded_provider(path: &Path, fresh: ProviderToml) -> ProviderToml {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return fresh;
    };
    let Ok(mut existing) = toml::from_str::<ProviderToml>(&contents) else {
        return fresh;
    };

    existing.api = fresh.api;
    existing.base_url = fresh.base_url;
    existing.auth = fresh.auth;
    existing.auth_header = fresh.auth_header;
    existing.enabled = true;
    for model in fresh.models {
        if !existing.models.contains(&model) {
            existing.models.push(model);
        }
    }
    existing
}

fn persist_outcome(home: &Path, outcome: &harw_tui::SetupOutcome) -> Result<(), String> {
    validate_provider_name(&outcome.provider_id)?;
    harw_provider_http::validate_endpoint(&outcome.base_url).map_err(|e| e.to_string())?;
    if outcome.model.trim().is_empty() {
        return Err("Modell-ID / Deployment-Name darf nicht leer sein".into());
    }
    if !matches!(
        outcome.api.as_str(),
        "openai-chat" | "openai-responses" | "anthropic-messages" | "ollama"
    ) {
        return Err("Nicht unterstützte Provider-API".into());
    }
    let profile_name = harw_home::active_profile_name(home);
    let profile = harw_home::profile_dir(home, &profile_name).map_err(|e| e.to_string())?;

    // secret_ref kann eine fertige Referenz (env:/file:/file-json:) ODER ein
    // roh eingegebener Schlüssel sein; letzterer wandert in eine 0600-Datei.
    let auth_ref: Option<SecretRef> = match &outcome.secret_ref {
        None => None,
        Some(raw) if raw.is_empty() => None,
        Some(raw) if is_secret_ref(raw) => Some(
            raw.parse()
                .map_err(|e: harw_config::ConfigError| e.to_string())?,
        ),
        Some(raw) => Some(write_secret_file(home, &outcome.provider_id, raw)?),
    };

    let model_id = if outcome.model.is_empty() {
        format!("{}-default", outcome.provider_id)
    } else {
        outcome.model.clone()
    };
    // Die volle Katalog-Modell-Liste dieses Providers ergänzen (nicht nur das
    // gewählte Modell), damit der `/model`-Picker nach dem Onboarding alle
    // Modelle anbietet. Das gewählte Modell bleibt `default_model`.
    let mut models_list = catalog_models_for_provider(&outcome.provider_id);
    if !models_list.iter().any(|id| id == &model_id) {
        models_list.push(model_id.clone());
    }
    let provider = ProviderToml {
        stream: None,
        name: outcome.provider_id.clone(),
        api: outcome.api.clone(),
        base_url: outcome.base_url.clone(),
        auth: auth_ref.clone(),
        auth_header: outcome.auth_header.clone(),
        api_key: None,
        headers: std::collections::HashMap::new(),
        models: models_list.clone(),
        enabled: true,
        origin_allowlist: harw_config::OriginAllowlistToml::default(),
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
    };
    let providers_dir = profile.join("providers");
    create_dir_all(&providers_dir)?;
    let provider_path = providers_dir.join(format!("{}.toml", outcome.provider_id));
    let provider = merge_with_seeded_provider(&provider_path, provider);
    write_file(
        &provider_path,
        &toml::to_string_pretty(&provider).map_err(|e| format!("provider serialisieren: {e}"))?,
    )?;

    let model = ModelToml {
        stream: None,
        rate_limit: None,
        id: model_id.clone(),
        name: None,
        provider: outcome.provider_id.clone(),
        aliases: Vec::new(),
        context_window: None,
        max_tokens: None,
        reasoning: false,
        input_types: Vec::new(),
        capabilities: harw_config::ModelCapabilitiesToml::default(),
        prompt_caching: None,
        default_reasoning_effort: None,
    };
    let models_dir = profile.join("models");
    create_dir_all(&models_dir)?;
    write_file(
        &models_dir.join(model_filename(&model_id)),
        &toml::to_string_pretty(&model).map_err(|e| format!("modell serialisieren: {e}"))?,
    )?;

    // Restliche Katalog-Modelle dieses Providers als Modell-Dateien anlegen,
    // damit sie im `/model`-Picker erscheinen. Eine bereits vorhandene Datei
    // (z. B. durch `harw models scan` angereichert) wird nicht überschrieben —
    // "Never delete anything" gilt auch für schon vorhandene Anreicherungen.
    for extra_id in models_list
        .iter()
        .filter(|id| id.as_str() != model_id.as_str())
    {
        let extra_path = models_dir.join(model_filename(extra_id));
        if extra_path.exists() {
            continue;
        }
        let extra_model = ModelToml {
            stream: None,
            rate_limit: None,
            id: extra_id.clone(),
            name: None,
            provider: outcome.provider_id.clone(),
            aliases: Vec::new(),
            context_window: None,
            max_tokens: None,
            reasoning: false,
            input_types: Vec::new(),
            capabilities: harw_config::ModelCapabilitiesToml::default(),
            prompt_caching: None,
            default_reasoning_effort: None,
        };
        write_file(
            &extra_path,
            &toml::to_string_pretty(&extra_model)
                .map_err(|e| format!("modell serialisieren: {e}"))?,
        )?;
    }

    // Credential-Pool-Eintrag (nur wenn eine Referenz vorliegt): wird
    // ersetzt, nicht angehängt — siehe `replace_pool_entries`.
    if let Some(secret) = &auth_ref {
        let auth_path = harw_home::auth_path(home);
        let mut auth = load_auth(&auth_path)?;
        let pool = auth
            .credential_pool
            .entry(outcome.provider_id.clone())
            .or_default();
        replace_pool_entries(pool, secret, &outcome.base_url);
        write_file(
            &auth_path.clone(),
            &toml::to_string_pretty(&auth).map_err(|e| format!("auth serialisieren: {e}"))?,
        )?;
    }

    let config_path = profile.join("config.toml");
    let mut config = load_or_default_config(&config_path)?;
    config.default_provider = Some(outcome.provider_id.clone());
    config.default_model = Some(model_id.clone());
    config.onboarding.seen.provider = true;
    config.onboarding.seen.model = true;
    config.base_dir = None;
    write_file(
        &config_path,
        &toml::to_string_pretty(&config).map_err(|e| format!("config serialisieren: {e}"))?,
    )?;

    let layers = harw_home::config_layers(home).map_err(|e| e.to_string())?;
    harw_config::discover_config(&layers)
        .and_then(|c| c.validate())
        .map_err(|e| e.to_string())?;
    eprintln!(
        "Einrichtung gespeichert: {} / {} (Profil: {}).",
        outcome.provider_id,
        model_id,
        profile.display()
    );
    Ok(())
}

/// Ersetzt den Credential-Pool eines Providers nach einem Onboarding.
///
/// # Description
/// Codex-Login und API-Key teilen sich die Provider-ID `openai`. Früher wurde
/// hier nur angehängt; nach einem Wechsel Codex → API blieb der
/// Codex-Login-Verweis (`…/.codex/auth.json#/tokens/access_token`) im Pool
/// stehen und ließ den Start scheitern, weil das Token nur für die
/// Codex-Route freigegeben ist. Jetzt gilt:
/// - der neue Verweis steht an erster Stelle (mit `base_url` der neuen Route);
/// - frühere Einträge bleiben nur erhalten, wenn sie zur neuen Route passen:
///   Codex-Login-Verweise nur auf der Codex-Route, alle anderen nur auf
///   Nicht-Codex-Routen (Erkennung über
///   [`harw_provider_http::is_codex_login_reference`] und
///   [`harw_provider_http::is_codex_base_url`]);
/// - ein früherer Eintrag mit demselben Verweis entfällt (Duplikat).
///
/// # Arguments
/// - `pool` (`&mut Vec<CredentialEntry>`): Pool-Einträge des Providers.
/// - `secret` (`&SecretRef`): neuer Verweis aus dem Onboarding.
/// - `base_url` (`&str`): Basis-URL der neuen Route.
fn replace_pool_entries(
    pool: &mut Vec<harw_config::CredentialEntry>,
    secret: &SecretRef,
    base_url: &str,
) {
    let codex_route = harw_provider_http::is_codex_login_reference(secret)
        || harw_provider_http::is_codex_base_url(base_url);
    let new_ref = secret.to_string();
    let previous = std::mem::take(pool);
    pool.push(harw_config::CredentialEntry {
        secret: secret.clone(),
        label: None,
        priority: 0,
        base_url: Some(base_url.to_owned()),
    });
    pool.extend(previous.into_iter().filter(|entry| {
        entry.secret.to_string() != new_ref
            && harw_provider_http::is_codex_login_reference(&entry.secret) == codex_route
    }));
}

/// Indizes der Pool-Einträge, die nicht zur Route des Providers passen.
///
/// # Description
/// Dieselbe Regel wie [`replace_pool_entries`], angewandt auf einen
/// bestehenden Pool ohne neuen Eintrag: die Route ist Codex, wenn
/// `provider.auth` ein Codex-Login-Verweis ist oder `provider.base_url` die
/// Codex-Basis-URL; dann bleiben nur Codex-Login-Verweise, sonst nur
/// Nicht-Codex-Verweise. Zusätzlich gilt jeder spätere Eintrag mit demselben
/// Verweis wie ein früherer, behaltener Eintrag als veraltet (Duplikat).
/// Secrets werden dabei nie aufgelöst — nur die Verweise verglichen.
///
/// # Arguments
/// - `pool`: die Pool-Einträge des Providers in Dateireihenfolge.
/// - `provider`: seine Konfiguration; `None` (Provider unbekannt) → nichts
///   ist veraltet (ohne Route keine Aussage).
///
/// # Returns
/// Aufsteigende Indizes in `pool`.
pub(crate) fn stale_pool_indices(
    pool: &[harw_config::CredentialEntry],
    provider: Option<&harw_config::ProviderToml>,
) -> Vec<usize> {
    let Some(provider) = provider else {
        return Vec::new();
    };
    let codex_route = provider
        .auth
        .as_ref()
        .is_some_and(harw_provider_http::is_codex_login_reference)
        || harw_provider_http::is_codex_base_url(&provider.base_url);
    let mut kept: Vec<String> = Vec::new();
    let mut stale = Vec::new();
    for (index, entry) in pool.iter().enumerate() {
        let reference = entry.secret.to_string();
        if harw_provider_http::is_codex_login_reference(&entry.secret) != codex_route
            || kept.contains(&reference)
        {
            stale.push(index);
        } else {
            kept.push(reference);
        }
    }
    stale
}

/// Ergebnis von [`prune_credential_pools`] je Provider: Name und die
/// entfernten Einträge als `(Index, Label)` — nie der Verweis selbst.
pub(crate) type PrunedPool = (String, Vec<(usize, Option<String>)>);

/// Entfernt veraltete Pool-Einträge ([`stale_pool_indices`]) aus der
/// `auth.toml` unter `auth_path`.
///
/// # Arguments
/// - `auth_path`: `harw_home::auth_path(home)`.
/// - `config`: aufgelöste Konfiguration (liefert `providers`).
/// - `only`: nur diesen Provider prüfen; `None` → alle Pools.
///
/// # Returns
/// Je betroffenem Provider (sortiert) die entfernten `(Index, Label)`. Die
/// Datei wird nur geschrieben, wenn etwas entfernt wurde.
///
/// # Errors
/// Lese-, Parse-, Serialisierungs- oder Schreibfehler der `auth.toml`.
pub(crate) fn prune_credential_pools(
    auth_path: &Path,
    config: &harw_config::ResolvedConfig,
    only: Option<&str>,
) -> Result<Vec<PrunedPool>, String> {
    let mut auth = load_auth(auth_path)?;
    let mut providers: Vec<String> = auth
        .credential_pool
        .keys()
        .filter(|name| only.is_none_or(|only| only == name.as_str()))
        .cloned()
        .collect();
    providers.sort();
    let mut removed = Vec::new();
    for name in providers {
        let Some(pool) = auth.credential_pool.get_mut(&name) else {
            continue;
        };
        let stale = stale_pool_indices(pool, config.providers.get(&name));
        if stale.is_empty() {
            continue;
        }
        let labels: Vec<(usize, Option<String>)> = stale
            .iter()
            .filter_map(|index| pool.get(*index).map(|entry| (*index, entry.label.clone())))
            .collect();
        let mut index = 0usize;
        pool.retain(|_| {
            let keep = !stale.contains(&index);
            index += 1;
            keep
        });
        removed.push((name, labels));
    }
    if !removed.is_empty() {
        write_file(
            &auth_path.to_path_buf(),
            &toml::to_string_pretty(&auth).map_err(|e| format!("auth serialisieren: {e}"))?,
        )?;
    }
    Ok(removed)
}

/// Hinweiszeile für `stderr` beim Start, wenn ein Pool veraltete Einträge
/// trägt (die der Provider-Aufbau sonst nur per `tracing::warn!`
/// überspringt).
///
/// # Returns
/// `None` ohne Befund; sonst ein Satz mit den betroffenen Providern und dem
/// Befehl `harw auth prune`. Nennt nie einen Verweis oder ein Secret.
pub(crate) fn stale_pool_hint(config: &harw_config::ResolvedConfig) -> Option<String> {
    let mut affected: Vec<(String, usize)> = config
        .auth
        .credential_pool
        .iter()
        .map(|(name, pool)| {
            (
                name.clone(),
                stale_pool_indices(pool, config.providers.get(name)).len(),
            )
        })
        .filter(|(_, count)| *count > 0)
        .collect();
    if affected.is_empty() {
        return None;
    }
    affected.sort();
    let list: Vec<String> = affected
        .iter()
        .map(|(name, count)| format!("{name} ({count})"))
        .collect();
    Some(format!(
        "Hinweis: veraltete Credential-Pool-Einträge werden übersprungen: {}. \
         Aufräumen mit `harw auth prune`.",
        list.join(", ")
    ))
}

/// Encode API identifiers as a single collision-free filename component.
/// The model ID inside TOML remains unchanged (including provider namespaces).
fn model_filename(id: &str) -> String {
    let mut name = String::new();
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            name.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(name, "%{byte:02X}");
        }
    }
    name.push_str(".toml");
    name
}

/// Liefert die vollständige Katalog-Modell-Liste des Providers `provider_id`
/// aus [`harw_model_catalog::embedded_catalog`], sofern dessen `base_url`
/// keine Platzhalter-URL ist. Andernfalls (Provider nicht im Katalog, oder
/// dessen Eintrag ist ein Platzhalter-Template wie Azure Foundry oder ein
/// generischer Worker) wird eine leere Liste zurückgegeben, und der Aufrufer
/// fällt auf das einzeln gewählte Modell zurück.
///
/// # Description
/// Wird von [`persist_outcome`] genutzt, damit nach dem Onboarding nicht nur
/// das gewählte Modell, sondern die gesamte Modell-Liste des Providers als
/// `models`-Einträge und `models/<id>.toml`-Dateien vorliegt — der
/// `/model`-Picker zeigt sonst nur einen einzigen Eintrag.
///
/// # Arguments
/// - `provider_id` (`&str`): Katalog-Id des Providers (z. B. `"anthropic"`).
///
/// # Returns
/// `Vec<String>` mit den Modell-Ids aus dem Katalog-Eintrag, oder leer.
fn catalog_models_for_provider(provider_id: &str) -> Vec<String> {
    harw_model_catalog::embedded_catalog()
        .into_iter()
        .find(|spec| spec.id == provider_id)
        .filter(|spec| !is_placeholder_base_url(&spec.base_url))
        .map(|spec| spec.models)
        .unwrap_or_default()
}

/// `true`, wenn `base_url` ein Platzhalter-Template ist (kein echter,
/// direkt nutzbarer Endpunkt), erkannt an `<...>`-Platzhaltern, dem
/// Beispiel-Domain-Segment `.example/` oder der reservierten Testdomain
/// `example.invalid`.
///
/// Solche Katalog-Einträge (z. B. Azure AI Foundry mit `<resource>` oder ein
/// generischer Cloudflare-Worker-Eintrag) beschreiben kein konkret
/// erreichbares Modell-Set und werden deshalb nicht automatisch mit einer
/// vollständigen Modell-Liste vorbelegt.
fn is_placeholder_base_url(base_url: &str) -> bool {
    base_url.contains('<') || base_url.contains(".example/") || base_url.contains("example.invalid")
}

/// `true`, wenn `value` bereits eine Secret-Referenz ist (kein roher Wert).
fn is_secret_ref(value: &str) -> bool {
    ["env:", "file:", "file-json:", "keyring:", "secrets:"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

/// Lädt `auth.toml` als [`harw_config::AuthConfig`] oder liefert den Default.
fn load_auth(path: &Path) -> Result<harw_config::AuthConfig, String> {
    if path.exists() {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("auth lesen ({}): {e}", path.display()))?;
        toml::from_str(&content).map_err(|e| format!("auth parsen ({}): {e}", path.display()))
    } else {
        Ok(harw_config::AuthConfig::default())
    }
}

/// Lädt die Profil-`config.toml` als [`HarnessConfig`] oder liefert den Default,
/// falls die Datei nicht existiert.
fn load_or_default_config(path: &Path) -> Result<HarnessConfig, String> {
    if path.exists() {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("config lesen ({}): {e}", path.display()))?;
        toml::from_str(&content).map_err(|e| format!("config parsen ({}): {e}", path.display()))
    } else {
        Ok(HarnessConfig::default())
    }
}

/// Schreibt den API-Key nach `<home>/secrets/<name>.key`, setzt unter Unix
/// `0o600`, und gibt die zugehörige `file:`-[`SecretRef`] zurück. Der Schlüssel
/// wird niemals geloggt.
///
/// The destination is never opened for writing. Instead, a same-directory
/// temporary is created with `create_new`, written with restrictive Unix
/// permissions from the beginning, synced, and atomically renamed over the
/// destination. This both avoids following a destination symlink and avoids a
/// window in which a newly-created secret has a permissive mode.
fn write_secret_file(home: &Path, provider_name: &str, key: &str) -> Result<SecretRef, String> {
    validate_provider_name(provider_name)?;

    let secrets_dir = home.join("secrets");
    create_dir_all(&secrets_dir)?;
    let secrets_metadata = std::fs::symlink_metadata(&secrets_dir).map_err(|e| {
        format!(
            "schlüsselverzeichnis prüfen ({}): {e}",
            secrets_dir.display()
        )
    })?;
    if secrets_metadata.file_type().is_symlink() || !secrets_metadata.is_dir() {
        return Err(format!(
            "schlüsselverzeichnis ist kein echtes Verzeichnis ({})",
            secrets_dir.display()
        ));
    }
    let key_path = secrets_dir.join(format!("{provider_name}.key"));
    let temp_path = secrets_dir.join(format!(
        ".{provider_name}.key.tmp-{}-{}",
        std::process::id(),
        SECRET_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let mut temporary = secret_temp_file(&temp_path).map_err(|e| {
        format!(
            "schlüsseldatei temporär anlegen ({}): {e}",
            temp_path.display()
        )
    })?;
    let write_result = temporary
        .write_all(key.as_bytes())
        .and_then(|()| temporary.sync_all());
    if let Err(error) = write_result {
        drop(temporary);
        let _ = std::fs::remove_file(&temp_path);
        return Err(format!(
            "schlüsseldatei schreiben ({}): {error}",
            key_path.display()
        ));
    }
    drop(temporary);

    if let Err(error) = std::fs::rename(&temp_path, &key_path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(format!(
            "schlüsseldatei ersetzen ({}): {error}",
            key_path.display()
        ));
    }

    #[cfg(unix)]
    std::fs::File::open(&secrets_dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| {
            format!(
                "schlüsselverzeichnis synchronisieren ({}): {e}",
                secrets_dir.display()
            )
        })?;

    let absolute = key_path.canonicalize().unwrap_or_else(|_| key_path.clone());
    format!("file:{}", absolute.display())
        .parse()
        .map_err(|e: harw_config::ConfigError| e.to_string())
}

fn secret_temp_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// Validiert einen Provider-Namen, bevor er als Datei-Komponente verwendet wird.
///
/// Provider-Namen werden absichtlich auf ASCII-Buchstaben, Ziffern, `-` und `_`
/// beschränkt. Damit sind leere Namen, `.`/`..`, Pfadtrenner und Steuerzeichen
/// ausgeschlossen, ohne den ungültigen Eingabewert in die Fehlermeldung zu
/// übernehmen.
fn validate_provider_name(provider_name: &str) -> Result<(), String> {
    if provider_name.is_empty()
        || !provider_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "ungültiger Provider-Name: nur ASCII-Buchstaben, Ziffern, '-' und '_' sind erlaubt"
                .to_owned(),
        );
    }

    Ok(())
}

/// Fragt einen Wert per `stdin` ab (interaktiv) oder liefert den Default.
/// Leere Eingabe → Default.
fn prompt_default(interactive: bool, label: &str, default: &str) -> Result<String, String> {
    if !interactive {
        return Ok(default.to_owned());
    }
    print!("{label} [{default}]: ");
    flush_stdout()?;
    let line = read_line()?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

/// Liest eine Zeile von `stdin`.
fn read_line() -> Result<String, String> {
    let mut buffer = String::new();
    std::io::stdin()
        .read_line(&mut buffer)
        .map_err(|e| format!("stdin lesen: {e}"))?;
    Ok(buffer)
}

/// Leert den `stdout`-Puffer, damit Prompts vor der Eingabe erscheinen.
fn flush_stdout() -> Result<(), String> {
    std::io::stdout()
        .flush()
        .map_err(|e| format!("stdout flush: {e}"))
}

/// Legt ein Verzeichnis samt Eltern an (idempotent).
fn create_dir_all(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("verzeichnis anlegen ({}): {e}", dir.display()))
}

/// Schreibt Textinhalt in eine Datei (überschreibend).
fn write_file(path: &PathBuf, content: &str) -> Result<(), String> {
    let parent = path.parent().ok_or("Datei hat kein Elternverzeichnis")?;
    create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".setup-{}-{}.tmp",
        std::process::id(),
        SECRET_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = secret_temp_file(&temporary).map_err(|e| e.to_string())?;
    let result = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&temporary, path));
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("Datei schreiben ({}): {error}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[tokio::test]
    async fn persisted_foundry_setup_sends_correct_wire_request_after_reload() -> TestResult {
        use std::io::{Read, Write};
        for (api, header, base, path, response) in [
            (
                "openai-chat",
                "api-key",
                "/openai/v1",
                "/openai/v1/chat/completions",
                r#"{"choices":[{"message":{"content":"OK"}}]}"#,
            ),
            (
                "openai-responses",
                "api-key",
                "/openai/v1",
                "/openai/v1/responses",
                r#"{"output":[{"type":"message","content":[{"type":"output_text","text":"OK"}]}]}"#,
            ),
            (
                "anthropic-messages",
                "x-api-key",
                "/anthropic",
                "/anthropic/v1/messages",
                r#"{"content":[{"type":"text","text":"OK"}]}"#,
            ),
            (
                "anthropic-messages",
                "bearer",
                "/anthropic",
                "/anthropic/v1/messages",
                r#"{"content":[{"type":"text","text":"OK"}]}"#,
            ),
        ] {
            let listener =
                std::net::TcpListener::bind("127.0.0.1:0").map_err(ctx("bind listener"))?;
            let endpoint = format!(
                "http://{}{base}",
                listener.local_addr().map_err(ctx("listener local_addr"))?
            );
            let server = std::thread::spawn(move || -> TestResult {
                let (mut stream, _) = listener.accept().map_err(ctx("accept connection"))?;
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .map_err(ctx("set_read_timeout"))?;
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let (end, length) = loop {
                    let n = stream.read(&mut buffer).map_err(ctx("stream read"))?;
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .ok_or(TestError::Missing("content-length header"))?
                            .parse()
                            .map_err(ctx("parse content-length"))?;
                        assert!(headers.starts_with(&format!("post {path} http/1.1")));
                        if header == "bearer" {
                            assert!(headers.contains("authorization: bearer test-resource-key"));
                            assert!(!headers.contains("oauth-2025"));
                        } else {
                            assert!(headers.contains(&format!("{header}: test-resource-key")));
                            assert!(!headers.contains("authorization: bearer"));
                        }
                        break (end + 4, length);
                    }
                };
                while bytes.len() < end + length {
                    let n = stream.read(&mut buffer).map_err(ctx("stream read"))?;
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                }
                let body: serde_json::Value = serde_json::from_slice(&bytes[end..end + length])
                    .map_err(ctx("parse body json"))?;
                assert_eq!(body["model"], "production-deployment");
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response.len(),
                    response
                )
                .map_err(ctx("write response"))?;
                Ok(())
            });
            let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
            harw_home::ensure_home(home.path()).map_err(ctx("ensure_home"))?;
            let outcome = harw_tui::SetupOutcome {
                provider_id: "foundry".into(),
                base_url: endpoint,
                api: api.into(),
                model: "production-deployment".into(),
                secret_ref: Some("test-resource-key".into()),
                auth_header: Some(header.into()),
            };
            persist_outcome(home.path(), &outcome).map_err(ctx("persist_outcome"))?;
            let layers = harw_home::config_layers(home.path()).map_err(ctx("config_layers"))?;
            let config = harw_config::discover_config(&layers).map_err(ctx("discover_config"))?;
            let provider = harw_provider_http::build_provider_with_home(&config, home.path(), None)
                .map_err(ctx("build_provider_with_home"))?;
            let result = provider
                .respond(harw_core::ModelRequest {
                    stream: None,
                    system_prompt: String::new(),
                    instruction_fragments: vec![],
                    context: vec![],
                    history: harw_core::ConversationHistory::new(),
                    tools: vec![],
                    context_assembly: Default::default(),
                    reasoning_effort: None,
                    model_id: None,
                    provider_id: Some("foundry".into()),
                    // Reiner Verbindungstest beim Onboarding: kein Datenblock,
                    // kein Token-Limit, keine Sonderbegrenzung für Tool-Ergebnisse.
                    data_block: None,
                    max_output_tokens: None,
                    tool_result_max_bytes: None,
                    cancel: None,
                    identity: None,
                })
                .await
                .map_err(ctx("provider respond"))?;
            assert_eq!(result.message.as_deref(), Some("OK"));
            server
                .join()
                .map_err(|_| TestError::Unexpected("server thread panicked".into()))??;
        }
        Ok(())
    }

    #[test]
    fn setup_with_namespaced_model_survives_reload_and_repeat() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        harw_home::ensure_home(home.path()).map_err(ctx("ensure_home"))?;
        let outcome = harw_tui::SetupOutcome {
            provider_id: "cloudflare".into(),
            base_url: "https://example.test/v1".into(),
            api: "openai-chat".into(),
            model: "@cf/moonshotai/kimi-k2.7-code".into(),
            secret_ref: Some("env:CLOUDFLARE_API_TOKEN".into()),
            auth_header: None,
        };
        persist_outcome(home.path(), &outcome).map_err(ctx("persist_outcome"))?;
        persist_outcome(home.path(), &outcome).map_err(ctx("persist_outcome (repeat)"))?;
        let layers = harw_home::config_layers(home.path()).map_err(ctx("config_layers"))?;
        let config = harw_config::discover_config(&layers).map_err(ctx("discover_config"))?;
        config.validate().map_err(ctx("config validate"))?;
        assert!(config.harness.onboarding.seen.is_complete());
        assert_eq!(
            config.harness.default_model.as_deref(),
            Some(outcome.model.as_str())
        );
        assert!(config.models.contains_key(&outcome.model));
        // Die gesamte Katalog-Modell-Liste von "cloudflare" wird mitgeschrieben
        // (nicht nur das gewählte Modell) — der `/model`-Picker soll die volle
        // Auswahl anbieten. Das gewählte Modell bleibt Teil der Liste.
        let mut got_models = config.providers["cloudflare"].models.clone();
        got_models.sort();
        got_models.dedup();
        let mut want_models = harw_model_catalog::embedded_catalog()
            .into_iter()
            .find(|spec| spec.id == "cloudflare")
            .map(|spec| spec.models)
            .unwrap_or_default();
        want_models.sort();
        assert_eq!(got_models, want_models);
        assert!(
            config.providers["cloudflare"]
                .models
                .contains(&outcome.model)
        );
        let auth = load_auth(&harw_home::auth_path(home.path())).map_err(ctx("load_auth"))?;
        assert_eq!(auth.credential_pool["cloudflare"].len(), 1);
        assert_ne!(model_filename("a/b"), model_filename("a%2Fb"));
        assert!(!model_filename("../../escape").contains('/'));
        Ok(())
    }

    /// Onboardet `openai` nacheinander mit zwei Verweisen und liefert den
    /// resultierenden Pool als kanonische Referenz-Strings.
    fn pool_after_two_onboardings(
        first: (&str, &str),
        second: (&str, &str),
    ) -> TestResult<Vec<String>> {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        harw_home::ensure_home(home.path()).map_err(ctx("ensure_home"))?;
        for (base_url, secret_ref) in [first, second] {
            let outcome = harw_tui::SetupOutcome {
                provider_id: "openai".into(),
                base_url: base_url.into(),
                api: "openai-responses".into(),
                model: "gpt-5.4".into(),
                secret_ref: Some(secret_ref.into()),
                auth_header: None,
            };
            persist_outcome(home.path(), &outcome).map_err(ctx("persist_outcome"))?;
        }
        let auth = load_auth(&harw_home::auth_path(home.path())).map_err(ctx("load_auth"))?;
        Ok(auth.credential_pool["openai"]
            .iter()
            .map(|entry| entry.secret.to_string())
            .collect())
    }

    const CODEX_REF: &str =
        "file-json:/nonexistent-harw-test-home/.codex/auth.json#/tokens/access_token";

    #[test]
    fn persist_outcome_codex_then_api_key_keeps_only_key_in_pool() -> TestResult {
        let pool = pool_after_two_onboardings(
            ("https://chatgpt.com/backend-api/codex", CODEX_REF),
            ("https://api.openai.com/v1", "env:OPENAI_API_KEY"),
        )?;
        assert_eq!(pool, vec!["env:OPENAI_API_KEY".to_owned()]);
        Ok(())
    }

    #[test]
    fn persist_outcome_api_key_then_codex_keeps_only_codex_login_in_pool() -> TestResult {
        let pool = pool_after_two_onboardings(
            ("https://api.openai.com/v1", "env:OPENAI_API_KEY"),
            ("https://chatgpt.com/backend-api/codex", CODEX_REF),
        )?;
        assert_eq!(pool.len(), 1);
        assert!(pool[0].ends_with(".codex/auth.json#/tokens/access_token"));
        Ok(())
    }

    fn provider(base_url: &str, auth: &str) -> TestResult<harw_config::ProviderToml> {
        toml::from_str(&format!(
            "name = \"openai\"\napi = \"openai-responses\"\nbase_url = \"{base_url}\"\nauth = \"{auth}\"\n"
        ))
        .map_err(ctx("provider toml"))
    }

    fn pool_entry(
        reference: &str,
        label: Option<&str>,
    ) -> TestResult<harw_config::CredentialEntry> {
        Ok(harw_config::CredentialEntry {
            secret: reference.parse().map_err(ctx("secret ref"))?,
            label: label.map(str::to_owned),
            priority: 0,
            base_url: None,
        })
    }

    #[test]
    fn stale_pool_indices_drop_wrong_route_entries_and_duplicates() -> TestResult {
        let pool = vec![
            pool_entry("env:OPENAI_API_KEY", Some("key"))?,
            pool_entry(CODEX_REF, Some("codex"))?,
            pool_entry("env:OPENAI_API_KEY", Some("dup"))?,
            pool_entry("env:OPENAI_OTHER", None)?,
        ];
        let api = provider("https://api.openai.com/v1", "env:OPENAI_API_KEY")?;
        assert_eq!(stale_pool_indices(&pool, Some(&api)), vec![1, 2]);
        let codex = provider("https://chatgpt.com/backend-api/codex", CODEX_REF)?;
        assert_eq!(stale_pool_indices(&pool, Some(&codex)), vec![0, 2, 3]);
        // Ohne Provider-Konfiguration keine Aussage.
        assert!(stale_pool_indices(&pool, None).is_empty());
        Ok(())
    }

    #[test]
    fn prune_credential_pools_rewrites_auth_toml_and_hint_names_the_provider() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let auth_path = home.path().join("auth.toml");
        let mut auth = harw_config::AuthConfig::default();
        auth.credential_pool.insert(
            "openai".to_owned(),
            vec![
                pool_entry("env:OPENAI_API_KEY", Some("key"))?,
                pool_entry(CODEX_REF, Some("codex"))?,
            ],
        );
        auth.credential_pool
            .insert("other".to_owned(), vec![pool_entry("env:OTHER_KEY", None)?]);
        std::fs::write(
            &auth_path,
            toml::to_string_pretty(&auth).map_err(ctx("auth toml"))?,
        )
        .map_err(ctx("write auth"))?;
        let mut config = harw_config::ResolvedConfig::default();
        config.providers.insert(
            "openai".to_owned(),
            provider("https://api.openai.com/v1", "env:OPENAI_API_KEY")?,
        );
        config.auth = auth;

        let hint = stale_pool_hint(&config).ok_or(TestError::Missing("hint"))?;
        assert!(hint.contains("openai (1)"), "{hint}");
        assert!(hint.contains("harw auth prune"), "{hint}");
        assert!(
            !hint.contains("codex/auth.json"),
            "kein Verweis im Hinweis: {hint}"
        );

        // Nur `other` angefragt: nichts zu tun, Datei bleibt.
        let none = prune_credential_pools(&auth_path, &config, Some("other"))
            .map_err(ctx("prune other"))?;
        assert!(none.is_empty());

        let removed =
            prune_credential_pools(&auth_path, &config, None).map_err(ctx("prune all"))?;
        assert_eq!(
            removed,
            vec![("openai".to_owned(), vec![(1, Some("codex".to_owned()))])]
        );
        let reloaded = load_auth(&auth_path).map_err(ctx("reload auth"))?;
        let refs: Vec<String> = reloaded.credential_pool["openai"]
            .iter()
            .map(|entry| entry.secret.to_string())
            .collect();
        assert_eq!(refs, vec!["env:OPENAI_API_KEY".to_owned()]);
        assert_eq!(reloaded.credential_pool["other"].len(), 1);

        config.auth = reloaded;
        assert!(stale_pool_hint(&config).is_none());
        Ok(())
    }

    #[test]
    fn replace_pool_entries_puts_new_first_and_keeps_matching_route_entries() -> TestResult {
        let old_key: SecretRef = "env:OPENAI_OLD".parse().map_err(ctx("old ref"))?;
        let new_key: SecretRef = "env:OPENAI_NEW".parse().map_err(ctx("new ref"))?;
        let entry = |secret: &SecretRef| harw_config::CredentialEntry {
            secret: secret.clone(),
            label: None,
            priority: 0,
            base_url: None,
        };
        let mut pool = vec![entry(&old_key), entry(&new_key)];
        replace_pool_entries(&mut pool, &new_key, "https://api.openai.com/v1");
        let refs: Vec<String> = pool.iter().map(|e| e.secret.to_string()).collect();
        assert_eq!(refs, vec!["env:OPENAI_NEW", "env:OPENAI_OLD"]);
        Ok(())
    }

    #[test]
    fn test_run_wizard_noninteractive_writes_provider_model_and_config() -> TestResult {
        // Test-Isolation: eindeutiger Temp-Pfad über process::id().
        let home = std::env::temp_dir().join(format!(
            "harw-onboarding-test-{}-{}",
            std::process::id(),
            "noninteractive"
        ));
        // Vorherige Reste entfernen (best effort).
        let _ = std::fs::remove_dir_all(&home);

        harw_home::ensure_home(&home).map_err(ctx("ensure_home"))?;
        run_wizard_with_interaction_mode(&home, false).map_err(ctx("run_wizard"))?;

        let profile_name = harw_home::active_profile_name(&home);
        let profile = harw_home::profile_dir(&home, &profile_name).map_err(ctx("profile_dir"))?;

        assert!(
            profile.join("providers").join("openai.toml").exists(),
            "providers/openai.toml sollte existieren"
        );
        assert!(
            profile.join("models").join("gpt-5.4.toml").exists(),
            "models/gpt-5.4.toml sollte existieren"
        );

        let layers = harw_home::config_layers(&home).map_err(ctx("config_layers"))?;
        let resolved = harw_config::discover_config(&layers).map_err(ctx("discover_config"))?;
        assert!(
            resolved.harness.onboarding.seen.is_complete(),
            "onboarding.seen sollte vollständig sein"
        );
        assert_eq!(resolved.harness.default_provider.as_deref(), Some("openai"));
        assert_eq!(resolved.harness.default_model.as_deref(), Some("gpt-5.4"));

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[test]
    fn write_secret_file_accepts_safe_provider_name() -> TestResult {
        let home = std::env::temp_dir().join(format!(
            "harw-onboarding-test-{}-safe-provider",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);

        let secret_ref = write_secret_file(&home, "openai_compat-1", "test-key")
            .map_err(ctx("safe provider name should be accepted"))?;
        let key_path = home.join("secrets/openai_compat-1.key");

        assert_eq!(
            std::fs::read_to_string(&key_path).map_err(ctx("read key"))?,
            "test-key"
        );
        assert!(secret_ref.to_string().starts_with("file:"));

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[test]
    fn write_secret_file_rejects_traversal_and_unsafe_provider_names() -> TestResult {
        let home = std::env::temp_dir().join(format!(
            "harw-onboarding-test-{}-unsafe-provider",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);

        for provider_name in [
            "../outside",
            "nested/provider",
            r"nested\provider",
            ".",
            "..",
            "bad.name",
            "bad\nname",
        ] {
            let result = write_secret_file(&home, provider_name, "must-not-write");
            let Err(error) = result else {
                return Err(TestError::Unexpected(
                    "unsafe provider name should be rejected".into(),
                ));
            };
            assert!(
                error.starts_with("ungültiger Provider-Name:"),
                "unexpected user-safe error: {error}"
            );
        }

        assert!(
            !home.exists(),
            "rejection must happen before directory creation"
        );

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn write_secret_file_creates_secret_with_mode_600_from_the_start() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;

        let home = std::env::temp_dir().join(format!(
            "harw-onboarding-test-{}-secret-mode",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);

        write_secret_file(&home, "mode-check", "secret").map_err(ctx("write secret"))?;
        let mode = std::fs::metadata(home.join("secrets/mode-check.key"))
            .map_err(ctx("secret metadata"))?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn write_secret_file_replaces_destination_symlink_without_following_it() -> TestResult {
        use std::os::unix::fs::symlink;

        let home = std::env::temp_dir().join(format!(
            "harw-onboarding-test-{}-secret-symlink",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let secrets_dir = home.join("secrets");
        std::fs::create_dir_all(&secrets_dir).map_err(ctx("create secrets directory"))?;
        let outside = home.join("outside.key");
        std::fs::write(&outside, "must remain unchanged").map_err(ctx("write outside sentinel"))?;
        symlink(&outside, secrets_dir.join("symlink-check.key"))
            .map_err(ctx("create key symlink"))?;

        write_secret_file(&home, "symlink-check", "new secret").map_err(ctx("replace symlink"))?;

        assert_eq!(
            std::fs::read_to_string(&outside).map_err(ctx("read outside sentinel"))?,
            "must remain unchanged"
        );
        assert_eq!(
            std::fs::read_to_string(secrets_dir.join("symlink-check.key"))
                .map_err(ctx("read replacement secret"))?,
            "new secret"
        );
        assert!(
            !std::fs::symlink_metadata(secrets_dir.join("symlink-check.key"))
                .map_err(ctx("replacement metadata"))?
                .file_type()
                .is_symlink()
        );

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    // ── Task: volle Katalog-Modell-Liste nach Onboarding ─────────────────────

    /// Onboarding von `anthropic` muss die gesamte Katalog-Modell-Liste in
    /// `providers/anthropic.toml` schreiben (nicht nur das gewählte Modell),
    /// dafür je eine `models/<id>.toml` anlegen, und das gewählte Modell bleibt
    /// `default_model`.
    #[test]
    fn persist_outcome_populates_full_catalog_model_list_for_anthropic() -> TestResult {
        let home = std::env::temp_dir().join(format!(
            "harw-onboarding-test-{}-anthropic-full-catalog",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        harw_home::ensure_home(&home).map_err(ctx("ensure_home"))?;

        let outcome = harw_tui::SetupOutcome {
            provider_id: "anthropic".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            api: "anthropic-messages".into(),
            model: "claude-opus-4-8".into(),
            secret_ref: Some("env:ANTHROPIC_API_KEY".into()),
            auth_header: None,
        };
        persist_outcome(&home, &outcome).map_err(ctx("persist_outcome"))?;

        let profile_name = harw_home::active_profile_name(&home);
        let profile = harw_home::profile_dir(&home, &profile_name).map_err(ctx("profile_dir"))?;

        let expected_models = harw_model_catalog::embedded_catalog()
            .into_iter()
            .find(|spec| spec.id == "anthropic")
            .map(|spec| spec.models)
            .unwrap_or_default();
        assert!(
            !expected_models.is_empty(),
            "test fixture assumption: anthropic catalog entry has models"
        );

        let layers = harw_home::config_layers(&home).map_err(ctx("config_layers"))?;
        let config = harw_config::discover_config(&layers).map_err(ctx("discover_config"))?;
        let mut got_models = config.providers["anthropic"].models.clone();
        got_models.sort();
        let mut want_models = expected_models.clone();
        want_models.sort();
        assert_eq!(
            got_models, want_models,
            "provider.models must contain the full catalog model list"
        );
        assert_eq!(
            config.harness.default_model.as_deref(),
            Some("claude-opus-4-8"),
            "default_model must stay the chosen model"
        );

        for id in &expected_models {
            assert!(
                profile.join("models").join(model_filename(id)).exists(),
                "missing model file for catalog model {id}"
            );
        }

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    /// Katalog-Einträge mit Platzhalter-`base_url` (hier `cf-worker`,
    /// `https://<dein-worker>.example/v1`) dürfen die Modell-Liste nicht
    /// automatisch aufblähen — nur das im Setup gewählte Modell landet in
    /// `providers/<id>.toml`.
    #[test]
    fn persist_outcome_skips_catalog_expansion_for_placeholder_base_url_provider() -> TestResult {
        let home = std::env::temp_dir().join(format!(
            "harw-onboarding-test-{}-cfworker-placeholder",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        harw_home::ensure_home(&home).map_err(ctx("ensure_home"))?;

        // Der Katalog-Eintrag "cf-worker" hat absichtlich einen nicht-leeren
        // `models`-Array *und* eine Platzhalter-`base_url` — genau der Fall, den
        // die Skip-Regel abdecken muss.
        let catalog_models = harw_model_catalog::embedded_catalog()
            .into_iter()
            .find(|spec| spec.id == "cf-worker")
            .map(|spec| spec.models)
            .unwrap_or_default();
        assert!(
            catalog_models.len() > 1,
            "test fixture assumption: cf-worker catalog entry has multiple models"
        );

        let outcome = harw_tui::SetupOutcome {
            provider_id: "cf-worker".into(),
            base_url: "https://my-worker.workers.test/v1".into(),
            api: "openai-chat".into(),
            model: "@cf/moonshotai/kimi-k2.7-code".into(),
            secret_ref: Some("env:CF_WORKER_TOKEN".into()),
            auth_header: None,
        };
        persist_outcome(&home, &outcome).map_err(ctx("persist_outcome"))?;

        let layers = harw_home::config_layers(&home).map_err(ctx("config_layers"))?;
        let config = harw_config::discover_config(&layers).map_err(ctx("discover_config"))?;
        assert_eq!(
            config.providers["cf-worker"].models,
            vec![outcome.model.clone()],
            "placeholder catalog entries must not auto-expand the model list"
        );

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    /// Ein bereits vorhandenes `models/<id>.toml` (z. B. durch `harw models
    /// scan` angereichert) darf beim Auffüllen der Katalog-Modell-Liste nicht
    /// überschrieben werden.
    #[test]
    fn persist_outcome_does_not_overwrite_existing_catalog_model_file() -> TestResult {
        let home = std::env::temp_dir().join(format!(
            "harw-onboarding-test-{}-anthropic-preserve",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        harw_home::ensure_home(&home).map_err(ctx("ensure_home"))?;

        let profile_name = harw_home::active_profile_name(&home);
        let profile = harw_home::profile_dir(&home, &profile_name).map_err(ctx("profile_dir"))?;
        let models_dir = profile.join("models");
        std::fs::create_dir_all(&models_dir).map_err(ctx("create models dir"))?;
        let preseeded_path = models_dir.join(model_filename("claude-sonnet-5"));
        let preseeded_marker = "id = \"claude-sonnet-5\"\nprovider = \"anthropic\"\nname = \"Enriched by harw models scan\"\n";
        std::fs::write(&preseeded_path, preseeded_marker).map_err(ctx("preseed model file"))?;

        let outcome = harw_tui::SetupOutcome {
            provider_id: "anthropic".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            api: "anthropic-messages".into(),
            model: "claude-opus-4-8".into(),
            secret_ref: Some("env:ANTHROPIC_API_KEY".into()),
            auth_header: None,
        };
        persist_outcome(&home, &outcome).map_err(ctx("persist_outcome"))?;

        let after =
            std::fs::read_to_string(&preseeded_path).map_err(ctx("read preseeded model file"))?;
        assert_eq!(
            after, preseeded_marker,
            "pre-existing model file must not be overwritten"
        );

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }
}
