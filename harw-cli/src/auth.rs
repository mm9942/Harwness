//! `harw auth` — Setup-Token/OAuth beschaffen, direkt setzen, importieren und
//! den Credential-Status anzeigen.
//!
//! Dieses Modul koppelt die I/O-freie `harw-oauth`-Schicht (PKCE, Token-Store)
//! an das Terminal: es zeigt die Authorize-URL, liest Codes/Token von `stdin`
//! und speichert den Token verschlüsselt im SecretStore (`secrets:<id>`), nie
//! als Klartextdatei. Secrets werden nie geloggt oder ausgegeben.

use std::io::{IsTerminal as _, Read as _, Write as _};
use std::path::{Path, PathBuf};

use secrecy::SecretString;

use crate::cli::AuthAction;
use crate::home::resolve_home;

/// Führt die gewählte `harw auth`-Aktion aus.
///
/// # Errors
/// Ein `String` mit menschenlesbarer Ursache bei Home-Auflösung, Eingabe-,
/// Netzwerk- oder Store-Fehlern.
pub fn run(home_override: Option<PathBuf>, action: AuthAction) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    crate::home::ensure_home(&home).map_err(|error| error.to_string())?;

    match action {
        AuthAction::Login { provider } => login(&home, provider.as_str()),
        AuthAction::Token { provider } => token(&home, provider.as_str()),
        AuthAction::Import { source } => import(source.as_str()),
        AuthAction::Status => status(&home),
        AuthAction::Prune { provider } => prune(&home, provider.as_deref()),
        AuthAction::Migrate { dry_run } => crate::auth_migrate::run(&home, dry_run),
    }
}

/// `harw auth prune [provider]`: veraltete Pool-Einträge aus `auth.toml`
/// entfernen.
///
/// # Beschreibung
/// Liest die Provider-Konfiguration (`provider.auth`, `base_url`) über die
/// normale Config-Kette und die rohe `auth.toml` des Homes, entfernt je Pool
/// die Einträge aus [`crate::onboarding::stale_pool_indices`] und schreibt
/// `auth.toml` nur, wenn sich etwas geändert hat. Ausgegeben werden Provider,
/// Index und Label — nie ein Secret oder ein Verweis.
///
/// # Errors
/// Config-/`auth.toml`-Lese- oder Schreibfehler; ein ausdrücklich genannter
/// Provider ohne Pool ist kein Fehler (Meldung „nichts zu tun“).
fn prune(home: &Path, provider: Option<&str>) -> Result<(), String> {
    let layers = harw_home::config_layers(home).map_err(|error| error.to_string())?;
    let config = harw_config::discover_config(&layers).map_err(|error| error.to_string())?;
    let auth_path = harw_home::auth_path(home);
    let removed = crate::onboarding::prune_credential_pools(&auth_path, &config, provider)?;
    if removed.is_empty() {
        println!("Keine veralteten Pool-Einträge gefunden.");
        return Ok(());
    }
    for (provider, entries) in &removed {
        for (index, label) in entries {
            let label = label.as_deref().unwrap_or("ohne Label");
            println!("credential_pool.{provider}: Eintrag #{index} ({label}) entfernt");
        }
    }
    println!("auth.toml aktualisiert: {}", auth_path.display());
    Ok(())
}

/// PKCE-Paste-Flow: URL zeigen, Code von `stdin` lesen, Token holen + speichern.
fn login(home: &Path, provider: &str) -> Result<(), String> {
    require_anthropic_login(provider)?;
    warn_anthropic_subscription_token();

    let pkce = harw_oauth::generate_pkce();
    // `state` wird aus dem Verifier abgeleitet (deterministisch, ausreichend
    // als CSRF-Marker für den Paste-Flow).
    let state = harw_oauth::challenge_from_verifier(&format!("state:{}", pkce.verifier));
    let url = harw_oauth::authorize_url(&pkce.challenge, &state);

    eprintln!("\n1) Öffne diese URL im Browser und melde dich an:\n");
    println!("{url}");
    eprintln!(
        "\n2) Füge den zurückgegebenen Code hier ein (Form `code` oder `code#state`) \
         und drücke Enter:"
    );
    let line = read_secret_input("code> ")?;
    let (code, pasted_state) = harw_oauth::split_callback(&line);
    if code.is_empty() {
        return Err("kein Code eingegeben".to_owned());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("Runtime-Start fehlgeschlagen: {error}"))?;
    let client = reqwest::Client::new();
    let token = runtime
        .block_on(harw_oauth::exchange_code(
            &client,
            &code,
            &pasted_state,
            &state,
            &pkce.verifier,
        ))
        .map_err(|error| format!("Token-Exchange fehlgeschlagen: {error}"))?;

    persist_and_hint(home, provider, &token)
}

/// Setzt einen bereits vorhandenen Setup-Token direkt (Eingabe über `stdin`).
fn token(home: &Path, provider: &str) -> Result<(), String> {
    require_token_provider(provider)?;
    if provider == "anthropic" {
        warn_anthropic_subscription_token();
    }

    let raw = if std::io::stdin().is_terminal() {
        eprintln!("{}", token_prompt(provider));
        read_secret_input("token> ")?
    } else {
        read_all_stdin()?
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("kein Token eingegeben".to_owned());
    }
    if provider == "openai" && looks_like_jwt(trimmed) {
        return Err(
            "der eingegebene Wert sieht wie ein ChatGPT-/Codex-OAuth-Token aus, nicht wie ein \
             OpenAI-Platform-API-Key. Harw sendet solche Tokens nicht an api.openai.com; \
             verwende einen Platform-API-Key."
                .to_owned(),
        );
    }
    let normalized: String = trimmed
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    if provider == "anthropic" {
        check_anthropic_token_format(&normalized).map_err(str::to_owned)?;
    }
    let token = SecretString::new(normalized.into_boxed_str());
    persist_and_hint(home, provider, &token)
}

/// Prüft das Format eines Anthropic-Tokens, ohne ihn auszugeben.
///
/// Setup-/OAuth-Tokens beginnen mit `sk-ant-oat`, API-Keys mit `sk-ant-api`;
/// beide bestehen nur aus sichtbarem ASCII. Fehlt das Präfix (z. B. nur der
/// hintere Teil eingefügt), würde Harw den Wert als API-Key senden und
/// Anthropic mit 401 antworten; Sonderzeichen (typografische Anführungszeichen
/// aus einem Paste) scheitern schon als HTTP-Header.
///
/// # Errors
/// Eine Klartext-Begründung ohne den Token-Wert.
fn check_anthropic_token_format(token: &str) -> Result<(), &'static str> {
    if !token.chars().all(|character| character.is_ascii_graphic()) {
        return Err(
            "der Token enthält unsichtbare oder Nicht-ASCII-Zeichen (z. B. typografische \
             Anführungszeichen aus einem Paste); bitte ohne Anführungszeichen erneut einfügen",
        );
    }
    if !token.starts_with("sk-ant-") {
        return Err(
            "der Token beginnt nicht mit `sk-ant-` (Setup-Tokens: `sk-ant-oat01-…`); \
             vermutlich wurde nur ein Teil eingefügt – bitte den vollständigen Token \
             aus `claude setup-token` einfügen",
        );
    }
    Ok(())
}

/// Kurzbefund für `harw auth status` über eine Anthropic-Token-Datei.
fn anthropic_token_file_note(path: &Path) -> &'static str {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return "";
    };
    let normalized: String = raw
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    match check_anthropic_token_format(&normalized) {
        Ok(()) if normalized.starts_with("sk-ant-oat") => " (Format: sk-ant-oat… ok)",
        Ok(()) => " (Format: sk-ant-… ohne `oat` – wird als API-Key gesendet)",
        Err(_) if !normalized.starts_with("sk-ant-") => {
            " (Format: Präfix `sk-ant-oat` fehlt – `harw auth token` neu ausführen)"
        }
        Err(_) => " (Format: ungültige Zeichen – `harw auth token` neu ausführen)",
    }
}

/// Speichert den Token verschlüsselt im SecretStore (`secrets:<id>`) und
/// zeigt nur die Referenz an, nie den Token selbst.
///
/// # Errors
/// Config-Lesefehler oder ein fehlgeschlagener Store-Schreibvorgang (z. B.
/// ein konfigurierter, aber nicht erreichbarer AuthHub); es entsteht dann
/// keine Klartextdatei als Ersatz.
fn persist_and_hint(home: &Path, provider: &str, token: &SecretString) -> Result<(), String> {
    let layers = harw_home::config_layers(home).map_err(|error| error.to_string())?;
    let config = harw_config::discover_config(&layers).map_err(|error| error.to_string())?;
    let secret_ref = persist_token(home, &config, provider, token)?;

    eprintln!("\n✓ Token verschlüsselt gespeichert: {secret_ref}");
    eprintln!(
        "\nDer Token wurde nicht ausgegeben. Aktiviere ihn über eine sichere, \
            nicht protokollierte Shell-Eingabe."
    );
    eprintln!(
        "\nTrage diese Referenz als `auth = \"…\"` in `providers/{provider}.toml` ein, \
         damit Harw das Secret verwendet."
    );
    Ok(())
}

/// Legt den Token als `<provider>-oauth-token` im SecretStore ab und liefert
/// die Referenz `secrets:<id>`.
///
/// Versiegelt wird über AuthHub (V3), wenn `[infrastructure].auth_socket`
/// gesetzt ist, sonst lokal (V2) unter dem KEK des Homes. Es gibt keinen
/// Klartext-Fallback.
///
/// # Errors
/// Die Store-Fehlermeldung mit Präfix; sie nennt nie den Token-Wert.
fn persist_token(
    home: &Path,
    config: &harw_config::ResolvedConfig,
    provider: &str,
    token: &SecretString,
) -> Result<harw_config::SecretRef, String> {
    crate::secret_store::store_secret(
        home,
        config,
        &format!("{provider}-oauth-token"),
        "provider authentication",
        token,
    )
    .map_err(|error| format!("Token speichern fehlgeschlagen: {error}"))
}

/// Importiert lokale Credentials und zeigt die nutzbare Referenz an.
///
/// Ein ChatGPT-Login der Codex-CLI enthält einen kurzlebigen OAuth-Access-Token.
/// Dieser ist kein OpenAI-Platform-API-Key und darf nie an `api.openai.com`
/// weitergereicht werden. `codex` wählt den Platform-Key; `codex-oauth` nutzt
/// die nur lesende, an das Codex-Backend gebundene Login-Route.
fn import(source: &str) -> Result<(), String> {
    let provider = match source {
        "codex" | "codex-oauth" => "openai",
        "claude-cli" | "claude-setup-token" => "anthropic",
        "gemini-env" => "gemini",
        "mistral-env" => "mistral",
        other => {
            return Err(format!(
                "unbekannte Quelle: {other} (codex | codex-oauth | claude-cli | gemini-env | mistral-env)"
            ));
        }
    };
    if matches!(source, "claude-cli" | "claude-setup-token") {
        warn_anthropic_subscription_token();
    }

    let detected = harw_model_catalog::detect_local_sources(provider);
    let mut any = false;
    for entry in &detected {
        if entry.source.id != source {
            continue;
        }
        let mark = if entry.exists {
            "gefunden"
        } else {
            "nicht gefunden"
        };
        eprintln!("[{mark}] {} → {}", entry.source.id, entry.secret_ref);
        if entry.exists {
            any = true;
        }
    }
    if !any {
        if source == "codex" {
            return Err(
                "kein OpenAI-Platform-API-Key in ~/.codex/auth.json gefunden. Ein \
                 ChatGPT-Codex-Login nutzt `harw auth import codex-oauth`; alternativ \
                 `harw auth token openai` mit einem Platform-API-Key."
                    .to_owned(),
            );
        }
        if source == "codex-oauth" {
            return Err("kein Codex-Login gefunden; zuerst `codex login` ausführen".into());
        }
        return Err(format!(
            "keine lokale Quelle für '{source}' gefunden (Provider {provider})"
        ));
    }
    eprintln!(
        "\nTrage die gewünschte Referenz als `auth = \"…\"` in \
         `providers/{provider}.toml` ein, um sie zu nutzen."
    );
    if source == "codex-oauth" {
        eprintln!(
            "Codex-Route: api = \"openai-responses\", base_url = \"https://chatgpt.com/backend-api/codex\". Login/Refresh bleiben bei Codex; Harw liest die Datei pro Request neu."
        );
    }
    Ok(())
}

/// Zeigt Vorhandensein aller bekannten Credential-Quellen — ohne Secrets.
fn status(home: &Path) -> Result<(), String> {
    eprintln!("Credential-Status (nur Vorhandensein, keine Werte):\n");

    let env_vars = [
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_FOUNDRY_API_KEY",
        "ANTHROPIC_FOUNDRY_BASE_URL",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "MISTRAL_API_KEY",
    ];
    for var in env_vars {
        println!("  env  {:<28} {}", var, yes_no(env_present(var)));
    }

    let home_str = home.to_string_lossy();
    let files = [
        ("~/.codex/auth.json", expand("~/.codex/auth.json")),
        (
            "~/.claude/.credentials.json",
            expand("~/.claude/.credentials.json"),
        ),
        (
            "<home>/secrets/anthropic-oauth.token",
            home.join("secrets").join("anthropic-oauth.token"),
        ),
        (
            "<home>/secrets/gemini-oauth.token",
            home.join("secrets").join("gemini-oauth.token"),
        ),
        (
            "<home>/secrets/mistral-oauth.token",
            home.join("secrets").join("mistral-oauth.token"),
        ),
    ];
    for (label, path) in files {
        let note = if label.ends_with("anthropic-oauth.token") && path.is_file() {
            anthropic_token_file_note(&path)
        } else {
            ""
        };
        println!("  file {:<28} {}{note}", label, yes_no(path.is_file()));
    }
    eprintln!("\n(Home: {home_str})");
    Ok(())
}

/// `true`, wenn eine Umgebungsvariable gesetzt und nicht leer ist.
fn env_present(var: &str) -> bool {
    std::env::var(var)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
}

/// Kompaktes Ja/Nein-Symbol.
fn yes_no(present: bool) -> &'static str {
    if present { "✓ gesetzt" } else { "— fehlt" }
}

/// Erweitert ein führendes `~/` gegen `$HOME`.
fn expand(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) if !home.is_empty() => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// Beschränkt den eingebauten Browser-PKCE-Flow auf Anthropic.
///
/// Andere Provider werden ausschließlich über `harw auth token <provider>`
/// eingerichtet, bis ein eigener, öffentlicher OAuth-Vertrag implementiert ist.
fn require_anthropic_login(provider: &str) -> Result<(), String> {
    if provider == "anthropic" {
        Ok(())
    } else {
        Err(format!(
            "Browser-OAuth wird derzeit nur für 'anthropic' unterstützt; verwende für {provider} `harw auth token {provider}`"
        ))
    }
}

/// Beschränkt direkte Secret-Eingabe auf Provider mit einem sicheren,
/// dokumentierten API-Key- bzw. Setup-Token-Weg. Der Browser-OAuth-Login
/// bleibt ausdrücklich Anthropic vorbehalten.
fn require_token_provider(provider: &str) -> Result<(), String> {
    match provider {
        "anthropic" | "openai" | "gemini" | "mistral" => Ok(()),
        other => Err(format!(
            "direkte Secret-Eingabe wird nur für anthropic, openai, gemini oder mistral unterstützt (gegeben: {other})"
        )),
    }
}

/// Gibt die Abo-OAuth-/Setup-Token-Warnung einmal vor der jeweiligen Aktion
/// aus (`eprintln!`, bestehender Ausgabestil dieser Datei). Bricht den Ablauf
/// nicht ab — nur ein Hinweis, keine Blockade.
fn warn_anthropic_subscription_token() {
    eprintln!(
        "\n{}\n",
        harw_provider_http::ANTHROPIC_SUBSCRIPTION_TOKEN_WARNING
    );
}

fn token_prompt(provider: &str) -> &'static str {
    match provider {
        "anthropic" => {
            "Füge den Setup-Token ein (z. B. Ausgabe von `claude setup-token`) und drücke Enter:"
        }
        "openai" => {
            "Füge einen OpenAI-Platform-API-Key ein (kein ChatGPT-/Codex-OAuth-Token) und drücke Enter:"
        }
        "gemini" => "Füge den Gemini-/Google-API-Key ein und drücke Enter:",
        "mistral" => "Füge den Mistral-API-Key ein und drücke Enter:",
        _ => "Füge das Secret ein und drücke Enter:",
    }
}

/// Erkennt das dreiteilige, URL-sichere Format eines JWT, ohne seinen Inhalt
/// zu dekodieren oder auszugeben. OpenAI-Platform-API-Keys verwenden dieses
/// Format nicht; ChatGPT-/Codex-Access-Tokens dagegen schon.
fn looks_like_jwt(value: &str) -> bool {
    let mut parts = value.split('.');
    let is_segment = |segment: Option<&str>| {
        segment.is_some_and(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    };
    is_segment(parts.next())
        && is_segment(parts.next())
        && is_segment(parts.next())
        && parts.next().is_none()
}

/// Liest ein Secret ohne Terminal-Echo oder aus einer nicht-interaktiven Pipe.
///
/// Für interaktive Unix-Terminals wird `stty` verwendet. Kann das Echo nicht
/// sicher deaktiviert oder anschließend nicht wiederhergestellt werden, wird
/// die Eingabe abgebrochen (fail closed), statt ein Secret im Terminal zu
/// riskieren. Andere interaktive Plattformen werden derzeit nicht unterstützt.
fn read_secret_input(prompt: &str) -> Result<String, String> {
    if !std::io::stdin().is_terminal() {
        return read_all_stdin();
    }

    #[cfg(unix)]
    {
        eprint!("{prompt}");
        std::io::stderr().flush().ok();
        set_terminal_echo(false)?;
        let result = read_line_stdin();
        let restore = set_terminal_echo(true);
        if let Err(error) = restore {
            return Err(format!(
                "Terminal-Echo konnte nicht wiederhergestellt werden: {error}"
            ));
        }
        eprintln!();
        result
    }

    #[cfg(not(unix))]
    {
        let _ = prompt;
        Err(
            "interaktive Secret-Eingabe wird auf dieser Plattform nicht unterstützt; \
             verwende eine sichere Pipe"
                .to_owned(),
        )
    }
}

#[cfg(unix)]
fn set_terminal_echo(enabled: bool) -> Result<(), String> {
    let mode = if enabled { "echo" } else { "-echo" };
    let status = std::process::Command::new("stty")
        .arg(mode)
        .status()
        .map_err(|error| format!("stty starten: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("stty {mode} fehlgeschlagen: {status}"))
    }
}

/// Liest genau eine Zeile von `stdin`.
fn read_line_stdin() -> Result<String, String> {
    let mut buffer = String::new();
    std::io::stdin()
        .read_line(&mut buffer)
        .map_err(|error| format!("stdin lesen: {error}"))?;
    Ok(buffer.trim().to_owned())
}

/// Liest `stdin` vollständig (für gepipte Token-Eingabe).
fn read_all_stdin() -> Result<String, String> {
    let mut buffer = String::new();
    std::io::stdin()
        .read_to_string(&mut buffer)
        .map_err(|error| format!("stdin lesen: {error}"))?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    #[test]
    fn anthropic_token_format_check_explains_without_echo() {
        assert!(check_anthropic_token_format("sk-ant-oat01-abcDEF_123").is_ok());
        assert!(check_anthropic_token_format("sk-ant-api03-abc").is_ok());
        let missing = check_anthropic_token_format("oat01-abcDEF")
            .err()
            .unwrap_or_default();
        assert!(missing.contains("sk-ant-"), "{missing}");
        assert!(!missing.contains("abcDEF"));
        let quoted = check_anthropic_token_format("\u{201c}sk-ant-oat01-x\u{201d}")
            .err()
            .unwrap_or_default();
        assert!(quoted.contains("Anführungszeichen"), "{quoted}");
    }

    #[test]
    fn anthropic_token_file_note_flags_missing_prefix() -> Result<(), std::io::Error> {
        let dir = std::env::temp_dir().join(format!("harw-auth-note-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let file = dir.join("anthropic-oauth.token");
        std::fs::write(&file, "oat01-only-the-tail\n")?;
        assert!(anthropic_token_file_note(&file).contains("fehlt"));
        std::fs::write(&file, "sk-ant-oat01-complete\n")?;
        assert!(anthropic_token_file_note(&file).contains("ok"));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    use super::persist_token;
    use super::{anthropic_token_file_note, check_anthropic_token_format, looks_like_jwt};
    #[cfg(any(target_os = "linux", target_os = "android"))]
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn jwt_like_values_are_rejected_for_openai_platform_auth() {
        assert!(looks_like_jwt(
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1c2VyIn0.signature"
        ));
        assert!(!looks_like_jwt("sk-proj-example"));
        assert!(!looks_like_jwt("not.a.jwt.with.four.parts"));
        assert!(!looks_like_jwt("missing..segment"));
    }

    /// Sammelt rekursiv alle regulären Dateien unter `dir`.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn files_below(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> TestResult {
        for entry in std::fs::read_dir(dir).map_err(ctx("read directory"))? {
            let path = entry.map_err(ctx("directory entry"))?.path();
            if path.is_dir() {
                files_below(&path, out)?;
            } else if path.is_file() {
                out.push(path);
            }
        }
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn persist_token_stores_encrypted_and_writes_no_token_file() -> TestResult {
        use secrecy::ExposeSecret as _;

        let home = tempfile::TempDir::new().map_err(ctx("temporary home"))?;
        let config = harw_config::ResolvedConfig::default();
        let raw = "sk-ant-oat01-persist-token-test-value";
        let token = secrecy::SecretString::from(raw.to_owned());

        let secret_ref = persist_token(home.path(), &config, "anthropic", &token)
            .map_err(ctx("persist token"))?;
        let harw_config::SecretRef::Secrets(id) = &secret_ref else {
            return Err(TestError::Unexpected(format!(
                "expected secrets: reference, got {secret_ref}"
            )));
        };
        assert!(uuid_like(id), "secret id is not a UUID: {id}");
        assert!(
            !home
                .path()
                .join("secrets")
                .join("anthropic-oauth.token")
                .exists()
        );

        let mut files = Vec::new();
        files_below(home.path(), &mut files)?;
        for file in &files {
            let bytes = std::fs::read(file).map_err(ctx("read home file"))?;
            assert!(
                !bytes
                    .windows(raw.len())
                    .any(|window| window == raw.as_bytes()),
                "plaintext token found in {}",
                file.display()
            );
        }

        let auth_raw = std::fs::read_to_string(harw_home::auth_path(home.path()))
            .map_err(ctx("read bootstrapped auth.toml"))?;
        let auth: harw_config::AuthConfig =
            toml::from_str(&auth_raw).map_err(ctx("parse bootstrapped auth.toml"))?;
        let mut reloaded = harw_config::ResolvedConfig {
            auth,
            ..harw_config::ResolvedConfig::default()
        };
        reloaded.providers.insert(
            "anthropic".to_owned(),
            toml::from_str::<harw_config::ProviderToml>(&format!(
                "name = \"anthropic\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"{secret_ref}\"\n"
            ))
            .map_err(ctx("valid test provider"))?,
        );
        let resolver = crate::secret_store::open_configured_secret_resolver(home.path(), &reloaded)
            .map_err(ctx("open configured resolver"))?
            .ok_or(TestError::Missing("sealed resolver"))?;
        let resolved = harw_provider_http::SecretResolver::resolve(&resolver, id)
            .map_err(ctx("resolve stored token"))?;
        assert_eq!(resolved.expose_secret(), token.expose_secret());
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn persist_token_fails_closed_when_hub_unreachable() -> TestResult {
        let home = tempfile::TempDir::new().map_err(ctx("temporary home"))?;
        let socket_dir = tempfile::TempDir::new().map_err(ctx("temporary socket dir"))?;
        let config = harw_config::ResolvedConfig {
            infrastructure: Some(harw_config::InfrastructureSection {
                auth_socket: Some(socket_dir.path().join("missing.sock")),
                ..harw_config::InfrastructureSection::default()
            }),
            ..harw_config::ResolvedConfig::default()
        };
        let token = secrecy::SecretString::from("sk-ant-oat01-unreachable-hub".to_owned());

        let result = persist_token(home.path(), &config, "anthropic", &token);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "unreachable hub must fail the write".into(),
            ));
        };
        assert!(!error.contains("unreachable-hub"), "{error}");
        assert!(
            !home
                .path()
                .join("secrets")
                .join("anthropic-oauth.token")
                .exists()
        );
        let records = home.path().join("sealed-secrets").join("secrets");
        if records.is_dir() {
            let count = std::fs::read_dir(&records)
                .map_err(ctx("read sealed records"))?
                .count();
            assert_eq!(count, 0, "no record may be written without the hub");
        }
        Ok(())
    }

    /// `true` für die Textform einer UUID (8-4-4-4-12 Hex-Ziffern).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn uuid_like(value: &str) -> bool {
        let groups: Vec<&str> = value.split('-').collect();
        groups.len() == 5
            && groups
                .iter()
                .zip([8_usize, 4, 4, 4, 12])
                .all(|(group, len)| {
                    group.len() == len && group.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
    }
}
