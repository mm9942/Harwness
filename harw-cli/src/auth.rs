//! `harw auth` — Setup-Token/OAuth beschaffen, direkt setzen, importieren und
//! den Credential-Status anzeigen.
//!
//! Dieses Modul koppelt die I/O-freie `harw-oauth`-Schicht (PKCE, Token-Store)
//! an das Terminal: es zeigt die Authorize-URL, liest Codes/Token von `stdin`,
//! speichert den Token als 0600-Datei. Secrets werden nie geloggt oder
//! ausgegeben.

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
    harw_home::ensure_home(&home).map_err(|error| error.to_string())?;

    match action {
        AuthAction::Login { provider } => login(&home, &provider),
        AuthAction::Token { provider } => token(&home, &provider),
        AuthAction::Import { source } => import(&source),
        AuthAction::Status => status(&home),
    }
}

/// PKCE-Paste-Flow: URL zeigen, Code von `stdin` lesen, Token holen + speichern.
fn login(home: &Path, provider: &str) -> Result<(), String> {
    require_anthropic(provider)?;

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
    require_anthropic(provider)?;

    let raw = if std::io::stdin().is_terminal() {
        eprintln!(
            "Füge den Setup-Token ein (z. B. Ausgabe von `claude setup-token`) und drücke Enter:"
        );
        read_secret_input("token> ")?
    } else {
        read_all_stdin()?
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("kein Token eingegeben".to_owned());
    }
    let token = SecretString::new(trimmed.to_owned().into_boxed_str());
    persist_and_hint(home, provider, &token)
}

/// Speichert den Token (0600), ohne ihn in einer Shell-Zeile auszugeben.
fn persist_and_hint(home: &Path, provider: &str, token: &SecretString) -> Result<(), String> {
    let secret_ref = harw_oauth::save_token(home, provider, token)
        .map_err(|error| format!("Token speichern fehlgeschlagen: {error}"))?;

    eprintln!(
        "\n✓ Token gespeichert (0600): {}",
        secret_ref.as_ref_string()
    );
    eprintln!(
        "\nDer Token wurde nicht ausgegeben. Aktiviere ihn über eine sichere, \
            nicht protokollierte Shell-Eingabe."
    );
    eprintln!(
        "\nDanach nutzt der Anthropic-native Weg (`default_provider = \"anthropic\"`) \
         den Token automatisch."
    );
    Ok(())
}

/// Importiert lokale Credentials und zeigt die nutzbare Referenz an.
fn import(source: &str) -> Result<(), String> {
    let provider = match source {
        "codex" | "codex-oauth" => "openai",
        "claude-cli" | "claude-setup-token" => "anthropic",
        other => return Err(format!("unbekannte Quelle: {other} (codex | claude-cli)")),
    };

    let detected = harw_model_catalog::detect_local_sources(provider);
    let mut any = false;
    for entry in &detected {
        if source != "codex" && entry.source.id != source {
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
        return Err(format!(
            "keine lokale Quelle für '{source}' gefunden (Provider {provider})"
        ));
    }
    eprintln!(
        "\nTrage die gewünschte Referenz als `auth = \"…\"` in \
         `providers/{provider}.toml` ein, um sie zu nutzen."
    );
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
    ];
    for (label, path) in files {
        println!("  file {:<28} {}", label, yes_no(path.is_file()));
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

/// Stellt sicher, dass der Provider `anthropic` ist (einziger OAuth-Provider).
fn require_anthropic(provider: &str) -> Result<(), String> {
    if provider == "anthropic" {
        Ok(())
    } else {
        Err(format!(
            "OAuth/Setup-Token wird derzeit nur für 'anthropic' unterstützt (gegeben: {provider})"
        ))
    }
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
