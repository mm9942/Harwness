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
        let mut catalog = harw_model_catalog::embedded_catalog();
        if let Err(error) = harw_model_catalog::enrich_models(&home.join("cache"), &mut catalog) {
            eprintln!("Modellkatalog: {error}");
        }
        if let Some(outcome) = harw_tui::run_setup(catalog).map_err(|e| e.to_string())? {
            return persist_outcome(home, &outcome);
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
    persist_outcome(home, &outcome)
}

/// Persistiert das Ergebnis des ratatui-Setup-Pickers: schreibt Provider- und
/// Modell-TOML, ergänzt den `credential_pool` in `auth.toml` und setzt
/// `default_provider`/`default_model` + `onboarding.seen` im Profil.
///
/// # Errors
/// Ein `String` bei Schreib-, Serialisierungs- oder Validierungsfehlern.
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
    let provider = ProviderToml {
        name: outcome.provider_id.clone(),
        api: outcome.api.clone(),
        base_url: outcome.base_url.clone(),
        auth: auth_ref.clone(),
        auth_header: outcome.auth_header.clone(),
        api_key: None,
        headers: std::collections::HashMap::new(),
        models: vec![model_id.clone()],
        enabled: true,
        origin_allowlist: harw_config::OriginAllowlistToml::default(),
    };
    let providers_dir = profile.join("providers");
    create_dir_all(&providers_dir)?;
    write_file(
        &providers_dir.join(format!("{}.toml", outcome.provider_id)),
        &toml::to_string_pretty(&provider).map_err(|e| format!("provider serialisieren: {e}"))?,
    )?;

    let model = ModelToml {
        id: model_id.clone(),
        name: None,
        provider: outcome.provider_id.clone(),
        aliases: Vec::new(),
        context_window: None,
        max_tokens: None,
        reasoning: false,
        input_types: Vec::new(),
        capabilities: harw_config::ModelCapabilitiesToml::default(),
    };
    let models_dir = profile.join("models");
    create_dir_all(&models_dir)?;
    write_file(
        &models_dir.join(model_filename(&model_id)),
        &toml::to_string_pretty(&model).map_err(|e| format!("modell serialisieren: {e}"))?,
    )?;

    // Credential-Pool-Eintrag (nur wenn eine Referenz vorliegt).
    if let Some(secret) = &auth_ref {
        let auth_path = harw_home::auth_path(home);
        let mut auth = load_auth(&auth_path)?;
        let pool = auth
            .credential_pool
            .entry(outcome.provider_id.clone())
            .or_default();
        if !pool.iter().any(|entry| {
            entry.secret.to_string() == secret.to_string()
                && entry.base_url.as_deref() == Some(outcome.base_url.as_str())
        }) {
            pool.push(harw_config::CredentialEntry {
                secret: secret.clone(),
                label: None,
                priority: 0,
                base_url: Some(outcome.base_url.clone()),
            });
        }
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

    #[tokio::test]
    async fn persisted_foundry_setup_sends_correct_wire_request_after_reload() {
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
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}{base}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let (end, length) = loop {
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
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
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                }
                let body: serde_json::Value =
                    serde_json::from_slice(&bytes[end..end + length]).unwrap();
                assert_eq!(body["model"], "production-deployment");
                write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", response.len(), response).unwrap();
            });
            let home = tempfile::tempdir().unwrap();
            harw_home::ensure_home(home.path()).unwrap();
            let outcome = harw_tui::SetupOutcome {
                provider_id: "foundry".into(),
                base_url: endpoint,
                api: api.into(),
                model: "production-deployment".into(),
                secret_ref: Some("test-resource-key".into()),
                auth_header: Some(header.into()),
            };
            persist_outcome(home.path(), &outcome).unwrap();
            let layers = harw_home::config_layers(home.path()).unwrap();
            let config = harw_config::discover_config(&layers).unwrap();
            let provider =
                harw_provider_http::build_provider_with_home(&config, home.path(), None).unwrap();
            let result = provider
                .respond(harw_core::ModelRequest {
                    system_prompt: String::new(),
                    instruction_fragments: vec![],
                    context: vec![],
                    history: harw_core::ConversationHistory::new(),
                    tools: vec![],
                    context_assembly: Default::default(),
                    reasoning_effort: None,
                    model_id: None,
                    provider_id: Some("foundry".into()),
                })
                .await
                .unwrap();
            assert_eq!(result.message.as_deref(), Some("OK"));
            server.join().unwrap();
        }
    }

    #[test]
    fn setup_with_namespaced_model_survives_reload_and_repeat() {
        let home = tempfile::tempdir().unwrap();
        harw_home::ensure_home(home.path()).unwrap();
        let outcome = harw_tui::SetupOutcome {
            provider_id: "cloudflare".into(),
            base_url: "https://example.test/v1".into(),
            api: "openai-chat".into(),
            model: "@cf/moonshotai/kimi-k2.7-code".into(),
            secret_ref: Some("env:CLOUDFLARE_API_TOKEN".into()),
            auth_header: None,
        };
        persist_outcome(home.path(), &outcome).unwrap();
        persist_outcome(home.path(), &outcome).unwrap();
        let layers = harw_home::config_layers(home.path()).unwrap();
        let config = harw_config::discover_config(&layers).unwrap();
        config.validate().unwrap();
        assert!(config.harness.onboarding.seen.is_complete());
        assert_eq!(
            config.harness.default_model.as_deref(),
            Some(outcome.model.as_str())
        );
        assert!(config.models.contains_key(&outcome.model));
        assert_eq!(
            config.providers["cloudflare"].models,
            std::slice::from_ref(&outcome.model)
        );
        let auth = load_auth(&harw_home::auth_path(home.path())).unwrap();
        assert_eq!(auth.credential_pool["cloudflare"].len(), 1);
        assert_ne!(model_filename("a/b"), model_filename("a%2Fb"));
        assert!(!model_filename("../../escape").contains('/'));
    }

    #[test]
    fn test_run_wizard_noninteractive_writes_provider_model_and_config() {
        // Test-Isolation: eindeutiger Temp-Pfad über process::id().
        let home = std::env::temp_dir().join(format!(
            "harw-onboarding-test-{}-{}",
            std::process::id(),
            "noninteractive"
        ));
        // Vorherige Reste entfernen (best effort).
        let _ = std::fs::remove_dir_all(&home);

        harw_home::ensure_home(&home).expect("ensure_home");
        run_wizard_with_interaction_mode(&home, false).expect("run_wizard");

        let profile_name = harw_home::active_profile_name(&home);
        let profile = harw_home::profile_dir(&home, &profile_name).expect("profile_dir");

        assert!(
            profile.join("providers").join("openai.toml").exists(),
            "providers/openai.toml sollte existieren"
        );
        assert!(
            profile.join("models").join("gpt-5.4.toml").exists(),
            "models/gpt-5.4.toml sollte existieren"
        );

        let layers = harw_home::config_layers(&home).expect("config_layers");
        let resolved = harw_config::discover_config(&layers).expect("discover_config");
        assert!(
            resolved.harness.onboarding.seen.is_complete(),
            "onboarding.seen sollte vollständig sein"
        );
        assert_eq!(resolved.harness.default_provider.as_deref(), Some("openai"));
        assert_eq!(resolved.harness.default_model.as_deref(), Some("gpt-5.4"));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn write_secret_file_accepts_safe_provider_name() {
        let home = std::env::temp_dir().join(format!(
            "harw-onboarding-test-{}-safe-provider",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);

        let secret_ref = write_secret_file(&home, "openai_compat-1", "test-key")
            .expect("safe provider name should be accepted");
        let key_path = home.join("secrets/openai_compat-1.key");

        assert_eq!(
            std::fs::read_to_string(&key_path).expect("read key"),
            "test-key"
        );
        assert!(secret_ref.to_string().starts_with("file:"));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn write_secret_file_rejects_traversal_and_unsafe_provider_names() {
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
            let error = write_secret_file(&home, provider_name, "must-not-write")
                .expect_err("unsafe provider name should be rejected");
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
    }

    #[cfg(unix)]
    #[test]
    fn write_secret_file_creates_secret_with_mode_600_from_the_start() {
        use std::os::unix::fs::PermissionsExt as _;

        let home = std::env::temp_dir().join(format!(
            "harw-onboarding-test-{}-secret-mode",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);

        write_secret_file(&home, "mode-check", "secret").expect("write secret");
        let mode = std::fs::metadata(home.join("secrets/mode-check.key"))
            .expect("secret metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn write_secret_file_replaces_destination_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let home = std::env::temp_dir().join(format!(
            "harw-onboarding-test-{}-secret-symlink",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let secrets_dir = home.join("secrets");
        std::fs::create_dir_all(&secrets_dir).expect("create secrets directory");
        let outside = home.join("outside.key");
        std::fs::write(&outside, "must remain unchanged").expect("write outside sentinel");
        symlink(&outside, secrets_dir.join("symlink-check.key")).expect("create key symlink");

        write_secret_file(&home, "symlink-check", "new secret").expect("replace symlink");

        assert_eq!(
            std::fs::read_to_string(&outside).expect("read outside sentinel"),
            "must remain unchanged"
        );
        assert_eq!(
            std::fs::read_to_string(secrets_dir.join("symlink-check.key"))
                .expect("read replacement secret"),
            "new secret"
        );
        assert!(
            !std::fs::symlink_metadata(secrets_dir.join("symlink-check.key"))
                .expect("replacement metadata")
                .file_type()
                .is_symlink()
        );

        let _ = std::fs::remove_dir_all(&home);
    }
}
